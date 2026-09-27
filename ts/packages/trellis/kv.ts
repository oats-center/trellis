import { type KV, type KvEntry, Kvm } from "@nats-io/kv";
import type { NatsConnection } from "@nats-io/nats-core/internal";
import { AsyncResult, type BaseError, isErr, Result } from "@oatscenter/result";
import { JetStreamApiCodes } from "@nats-io/jetstream";

import type { Codec } from "./generated.ts";
import { KVError, TransportError, ValidationError } from "./errors/index.ts";
import type { ResourceTransportCheck } from "./auth/authorization/transport_state.ts";
import { decodeSubject, escapeKvKey } from "./helpers.ts";
import { recordCatalogDuration } from "./telemetry/metrics.ts";

const KV_MAGIC = new Uint8Array([0x54, 0x52, 0x4b, 0x56]);
const KV_ENVELOPE_FORMAT = 1;
const KV_ENVELOPE_HEADER_BYTES = 9;
const textEncoder = new TextEncoder();
const textDecoder = new TextDecoder("utf-8", { fatal: true });

declare const resourceRevisionBrand: unique symbol;

/** Opaque storage revision returned by State and KV resources. */
export type ResourceRevision = number & {
  readonly [resourceRevisionBrand]: "ResourceRevision";
};

function revisionFromBackend(revision: number): ResourceRevision {
  return revision as ResourceRevision;
}

/** A direct historical-to-current representation migration. */
export type KvMigration<TCurrent, THistoric = unknown> = (
  value: THistoric,
) =>
  | TCurrent
  | Result<TCurrent, BaseError>
  | Promise<TCurrent | Result<TCurrent, BaseError>>;

/** Runtime representation metadata emitted by a generated participant. */
export type KvRepresentation<T> = Readonly<{
  codec: Codec<T>;
  version: number;
  migrations: Readonly<Record<number, Codec<unknown>>>;
}>;

/** Caller-supplied direct migrations keyed by historical representation version. */
export type ResourceMigrations<T> = Readonly<Record<number, KvMigration<T>>>;

/** The operation represented by one retained KV revision. */
export type KvOperation = "put" | "delete";

/** Immutable value and storage metadata for one KV revision. */
export type TypedKvEntry<T> = Readonly<{
  key: string;
  value?: T;
  revision: ResourceRevision;
  timestamp: Date;
  operation: KvOperation;
}>;

/** A fallible entry yielded by a KV watcher. */
export type KvWatchItem<T> = Result<TypedKvEntry<T>, KVError | ValidationError>;

/** Options used to open a typed KV resource. */
export type KvOpenOptions<T = unknown> = Readonly<{
  history?: number;
  ttl?: number;
  bindOnly?: boolean;
  maxValueBytes?: number;
  replicas?: number;
  migrations?: ResourceMigrations<T>;
  isCurrent?: () => boolean;
  /** Transport-admission check for operations on this bucket. @internal */
  transport?: ResourceTransportCheck;
}>;

/** Failures a KV read operation can report. */
export type KVReadError = KVError | ValidationError | TransportError;
/** Failures a KV mutating or listing operation can report. */
export type KVOperationError = KVError | TransportError;

/** Opens or creates a physical KV bucket with the requested options. */
async function openKv(
  nats: NatsConnection,
  name: string,
  options: KvOpenOptions,
): Promise<KV> {
  const kvm = new Kvm(nats);
  const kv = options.bindOnly ? await kvm.open(name) : await kvm.create(name, {
    history: options.history ?? 1,
    ttl: options.ttl ?? 0,
    ...(options.replicas === undefined ? {} : { replicas: options.replicas }),
    ...(options.maxValueBytes === undefined
      ? {}
      : { maxValueSize: options.maxValueBytes }),
  });
  await ensureExistingBucketOptions(kv, name, options);
  return kv;
}

type ExistingBucketStatus = Pick<
  Awaited<ReturnType<KV["status"]>>,
  "bucket" | "history" | "ttl" | "maxValueSize" | "max_bytes"
>;

/** Verifies that an existing physical bucket satisfies the requested limits. */
export async function ensureExistingBucketOptions(
  kv: { status(): Promise<ExistingBucketStatus> },
  name: string,
  options: KvOpenOptions,
): Promise<void> {
  const status = await kv.status();
  const history = options.history ?? 1;
  const ttl = options.ttl ?? 0;
  if (status.bucket !== name) {
    throw new Error(
      `KV bucket '${name}' opened physical bucket '${status.bucket}'`,
    );
  }
  if (status.history < history) {
    throw new Error(
      `KV bucket '${name}' history ${status.history} is less than requested ${history}`,
    );
  }
  if (status.ttl !== ttl) {
    throw new Error(`KV bucket '${name}' TTL does not match requested TTL`);
  }
  if (status.max_bytes > 0) {
    throw new Error(
      `KV bucket '${name}' has an uncommitted provider total size limit`,
    );
  }
  const maxValueBytes = options.maxValueBytes;
  if (maxValueBytes === undefined && status.maxValueSize > 0) {
    throw new Error(
      `KV bucket '${name}' has an uncommitted provider value size limit`,
    );
  }
  if (
    maxValueBytes !== undefined &&
    status.maxValueSize > 0 && status.maxValueSize < maxValueBytes
  ) {
    throw new Error(
      `KV bucket '${name}' provider size limit is less than requested`,
    );
  }
}

function kvError(
  operation: string,
  key: string | undefined,
  cause: unknown,
): KVError {
  return new KVError({
    operation,
    cause,
    context: {
      ...(key === undefined ? {} : { key }),
      ...(cause instanceof Error && "code" in cause
        ? { code: cause.code }
        : {}),
      ...(cause instanceof Error && "subject" in cause
        ? { subject: cause.subject }
        : {}),
    },
  });
}

function validationError(
  entry: KvEntry,
  message: string,
  cause?: unknown,
): ValidationError {
  return new ValidationError({
    errors: [{ path: "", message }],
    cause,
    context: {
      key: decodeSubject(entry.key),
      revision: entry.revision,
    },
  });
}

/** Encode a current value in the strict TRKV envelope. */
export function encodeResourceValue<T>(
  representation: KvRepresentation<T>,
  value: T,
) {
  if (
    !Number.isInteger(representation.version) || representation.version <= 0 ||
    representation.version > 0xffff_ffff
  ) {
    throw new RangeError("KV representation version must be a positive u32");
  }
  const body = textEncoder.encode(
    JSON.stringify(representation.codec.encode(value)),
  );
  const bytes = new Uint8Array(KV_ENVELOPE_HEADER_BYTES + body.length);
  bytes.set(KV_MAGIC);
  bytes[4] = KV_ENVELOPE_FORMAT;
  new DataView(bytes.buffer).setUint32(5, representation.version);
  bytes.set(body, KV_ENVELOPE_HEADER_BYTES);
  return bytes;
}

/** Decode and optionally migrate one envelope without writing it back. */
export async function decodeResourceValue<T>(
  representation: KvRepresentation<T>,
  migrations: ResourceMigrations<T>,
  bytes: Uint8Array,
): Promise<T> {
  if (
    bytes.length < KV_ENVELOPE_HEADER_BYTES ||
    KV_MAGIC.some((byte, index) => bytes[index] !== byte) ||
    bytes[4] !== KV_ENVELOPE_FORMAT
  ) throw new Error("Invalid TRKV envelope");
  const version = new DataView(
    bytes.buffer,
    bytes.byteOffset,
    bytes.byteLength,
  ).getUint32(5);
  const encoded = JSON.parse(
    textDecoder.decode(bytes.subarray(KV_ENVELOPE_HEADER_BYTES)),
  );
  if (version === representation.version) {
    return representation.codec.decode(encoded);
  }
  const historicCodec = representation.migrations[version];
  const migrate = migrations[version];
  if (!historicCodec || !migrate) {
    throw new Error(`Unsupported resource representation version ${version}`);
  }
  const migrated = await migrate(historicCodec.decode(encoded));
  if (migrated instanceof Result) {
    if (migrated.isErr()) throw migrated.error;
    return representation.codec.decode(representation.codec.encode(
      migrated.unwrapOrElse(() => {
        throw new Error("Resource migration unexpectedly failed");
      }),
    ));
  }
  return representation.codec.decode(representation.codec.encode(migrated));
}

async function decodeEntry<T>(
  representation: KvRepresentation<T>,
  migrations: ResourceMigrations<T>,
  entry: KvEntry,
): Promise<Result<TypedKvEntry<T>, ValidationError>> {
  const base = {
    key: decodeSubject(entry.key),
    revision: revisionFromBackend(entry.revision),
    timestamp: entry.created,
  };
  if (entry.operation === "DEL" || entry.operation === "PURGE") {
    return Result.ok({ ...base, operation: "delete" });
  }

  const bytes = entry.value;
  if (
    bytes.length < KV_ENVELOPE_HEADER_BYTES ||
    KV_MAGIC.some((byte, index) => bytes[index] !== byte) ||
    bytes[4] !== KV_ENVELOPE_FORMAT
  ) {
    return Result.err(validationError(entry, "Invalid TRKV envelope"));
  }
  try {
    const value = await decodeResourceValue(representation, migrations, bytes);
    return Result.ok({ ...base, operation: "put", value });
  } catch (cause) {
    return Result.err(
      validationError(entry, "Failed to decode or migrate KV value", cause),
    );
  }
}

export class TypedKV<T> {
  #kv?: KV;
  #opener?: () => Promise<KV>;

  private constructor(
    readonly name: string,
    readonly representation: KvRepresentation<T>,
    readonly migrations: ResourceMigrations<T>,
    readonly isCurrent: () => boolean,
    readonly transport?: ResourceTransportCheck,
    opener?: () => Promise<KV>,
    materialized?: KV,
  ) {
    this.#kv = materialized;
    this.#opener = opener;
  }

  /** Opens or creates a typed KV bucket. */
  static open<T>(
    nats: NatsConnection,
    name: string,
    representation: KvRepresentation<T>,
    options: KvOpenOptions<T> = {},
  ): AsyncResult<TypedKV<T>, KVError> {
    return AsyncResult.from((async () => {
      try {
        const kv = await openKv(nats, name, options);
        return Result.ok(
          new TypedKV(
            name,
            representation,
            options.migrations ?? {},
            options.isCurrent ?? (() => true),
            options.transport,
            undefined,
            kv,
          ),
        );
      } catch (cause) {
        return Result.err(kvError("open", undefined, cause));
      }
    })());
  }

  /**
   * Binds a typed KV bucket without opening it.
   *
   * The backing bucket is opened lazily on first operation, after the transport
   * check admits that operation, so a resource whose broker subjects are not yet
   * adopted never performs an unauthorized NATS request during refresh.
   * @internal
   */
  static bind<T>(
    nats: NatsConnection,
    name: string,
    representation: KvRepresentation<T>,
    options: KvOpenOptions<T> = {},
  ): TypedKV<T> {
    return new TypedKV(
      name,
      representation,
      options.migrations ?? {},
      options.isCurrent ?? (() => true),
      options.transport,
      () => openKv(nats, name, options),
    );
  }

  /** The opened NATS KV handle; throws until first materialization. */
  get kv(): KV {
    if (!this.#kv) {
      throw new Error("KV resource binding is not yet materialized");
    }
    return this.#kv;
  }

  async #backing(): Promise<KV> {
    let kv = this.#kv;
    if (!kv) {
      kv = await this.#opener!();
      this.#kv = kv;
    }
    return kv;
  }

  /** Returns the current value, or `undefined` when absent or deleted. */
  get(key: string): AsyncResult<T | undefined, KVReadError> {
    return this.#observe("read", () => this.#readEntry(key)).map((entry) =>
      entry?.operation === "put" ? entry.value : undefined
    );
  }

  /** Returns the latest value or tombstone with authoritative revision metadata. */
  getEntry(
    key: string,
  ): AsyncResult<TypedKvEntry<T> | undefined, KVReadError> {
    return this.#observe("read", () => this.#readEntry(key));
  }

  #readEntry(
    key: string,
  ): AsyncResult<TypedKvEntry<T> | undefined, KVReadError> {
    return AsyncResult.from((async () => {
      try {
        this.#assertCurrent();
        const blocked = await this.#admission("read");
        if (blocked) return Result.err(blocked);
        const entry = await (await this.#backing()).get(escapeKvKey(key));
        return entry
          ? await decodeEntry(this.representation, this.migrations, entry)
          : Result.ok(undefined);
      } catch (cause) {
        return Result.err(kvError("get", key, cause));
      }
    })());
  }

  /** Creates a value only when the key has no current value. */
  create(
    key: string,
    value: T,
  ): AsyncResult<TypedKvEntry<T>, KVOperationError> {
    return this.#observe("cas", () =>
      AsyncResult.from((async () => {
        try {
          this.#assertCurrent();
          const blocked = await this.#admission("write");
          if (blocked) return Result.err(blocked);
          const revision = await (await this.#backing()).create(
            escapeKvKey(key),
            encodeResourceValue(this.representation, value),
          );
          return Result.ok({
            key,
            value,
            revision: revisionFromBackend(revision),
            timestamp: new Date(),
            operation: "put",
          });
        } catch (cause) {
          return Result.err(kvError("create", key, cause));
        }
      })()));
  }

  /** Writes a value without a revision precondition. */
  put(key: string, value: T): AsyncResult<TypedKvEntry<T>, KVOperationError> {
    return this.#observe("write", () =>
      AsyncResult.from((async () => {
        try {
          this.#assertCurrent();
          const blocked = await this.#admission("write");
          if (blocked) return Result.err(blocked);
          const revision = await (await this.#backing()).put(
            escapeKvKey(key),
            encodeResourceValue(this.representation, value),
          );
          return Result.ok({
            key,
            value,
            revision: revisionFromBackend(revision),
            timestamp: new Date(),
            operation: "put",
          });
        } catch (cause) {
          return Result.err(kvError("put", key, cause));
        }
      })()));
  }

  /** Replaces a value only when its current revision matches. */
  replace(
    key: string,
    revision: ResourceRevision,
    value: T,
  ): AsyncResult<TypedKvEntry<T>, KVOperationError> {
    return this.#observe("cas", () =>
      AsyncResult.from((async () => {
        try {
          this.#assertCurrent();
          const blocked = await this.#admission("write");
          if (blocked) return Result.err(blocked);
          const nextRevision = await (await this.#backing()).update(
            escapeKvKey(key),
            encodeResourceValue(this.representation, value),
            revision,
          );
          return Result.ok({
            key,
            value,
            revision: revisionFromBackend(nextRevision),
            timestamp: new Date(),
            operation: "put",
          });
        } catch (cause) {
          return Result.err(kvError("replace", key, cause));
        }
      })()));
  }

  /** Deletes a key, optionally requiring its current revision. */
  delete(
    key: string,
    revision?: ResourceRevision,
  ): AsyncResult<void, KVOperationError> {
    return this.#observe("delete", () =>
      AsyncResult.from((async () => {
        try {
          this.#assertCurrent();
          const blocked = await this.#admission("write");
          if (blocked) return Result.err(blocked);
          await (await this.#backing()).delete(
            escapeKvKey(key),
            revision === undefined ? {} : { previousSeq: revision },
          );
          return Result.ok(undefined);
        } catch (cause) {
          return Result.err(kvError("delete", key, cause));
        }
      })()));
  }

  /** Returns all retained revisions for a key, including tombstones. */
  history(
    key: string,
  ): AsyncResult<readonly TypedKvEntry<T>[], KVReadError> {
    return this.#observe("list", () =>
      AsyncResult.from((async () => {
        try {
          this.#assertCurrent();
          const blocked = await this.#admission("read");
          if (blocked) return Result.err(blocked);
          const history = await (await this.#backing()).history({
            key: escapeKvKey(key),
          });
          const entries: TypedKvEntry<T>[] = [];
          for await (const entry of history) {
            this.#assertCurrent();
            const decoded = await decodeEntry(
              this.representation,
              this.migrations,
              entry,
            );
            if (decoded.isErr()) return decoded;
            entries.push(decoded.unwrapOrElse(() => {
              throw new Error("KV history decode unexpectedly failed");
            }));
          }
          entries.sort((left, right) => left.revision - right.revision);
          return Result.ok(entries);
        } catch (cause) {
          return Result.err(kvError("history", key, cause));
        }
      })()));
  }

  /** Watches retained initialization and subsequent revisions for one key. */
  watch(
    key: string,
  ): AsyncResult<AsyncIterable<KvWatchItem<T>>, KVOperationError> {
    return this.#observe("watch_setup", () =>
      AsyncResult.from((async () => {
        try {
          this.#assertCurrent();
          const blocked = await this.#admission("read");
          if (blocked) return Result.err(blocked);
          const watcher = await (await this.#backing()).watch({
            key: escapeKvKey(key),
            include: "history",
          });
          const representation = this.representation;
          const migrations = this.migrations;
          const isCurrent = this.isCurrent;
          return Result.ok({
            async *[Symbol.asyncIterator]() {
              try {
                for await (const entry of watcher) {
                  if (!isCurrent()) {
                    throw new Error("KV resource binding is stale");
                  }
                  yield await decodeEntry(representation, migrations, entry);
                }
              } finally {
                watcher.stop();
              }
            },
          });
        } catch (cause) {
          return Result.err(kvError("watch", key, cause));
        }
      })()));
  }

  /** Lists live keys using the backend's bounded iterator. */
  keys(
    filter: string | string[] = ">",
  ): AsyncResult<AsyncIterable<string>, KVOperationError> {
    return this.#observe("list", () =>
      AsyncResult.from((async () => {
        try {
          this.#assertCurrent();
          const blocked = await this.#admission("read");
          if (blocked) return Result.err(blocked);
          const keys = await (await this.#backing()).keys(filter);
          return Result.ok({
            async *[Symbol.asyncIterator]() {
              try {
                yield* keys;
              } catch (cause) {
                throw kvError("keys", undefined, cause);
              }
            },
          });
        } catch (cause) {
          return Result.err(kvError("keys", undefined, cause));
        }
      })()));
  }

  /** Returns the backend live-value count. */
  status(): AsyncResult<{ values: number }, KVOperationError> {
    return this.#observe("read", () =>
      AsyncResult.from((async () => {
        try {
          this.#assertCurrent();
          const blocked = await this.#admission("read");
          if (blocked) return Result.err(blocked);
          return Result.ok({
            values: (await (await this.#backing()).status()).values,
          });
        } catch (cause) {
          return Result.err(kvError("status", undefined, cause));
        }
      })()));
  }

  #observe<V, E extends BaseError>(
    operation: "read" | "write" | "list" | "cas" | "delete" | "watch_setup",
    run: () => AsyncResult<V, E>,
  ): AsyncResult<V, E> {
    return AsyncResult.from((async () => {
      const startedAt = performance.now();
      let outcome = "error";
      try {
        const result = await run();
        const value = result.take();
        outcome = isErr(value)
          ? value.error instanceof KVError &&
              value.error.cause instanceof Error &&
              Reflect.get(value.error.cause, "code") ===
                JetStreamApiCodes.StreamWrongLastSequence
            ? "conflict"
            : "error"
          : value === undefined && operation === "read"
          ? "not_found"
          : "ok";
        return result;
      } finally {
        recordCatalogDuration(
          "trellis.storage.duration",
          performance.now() - startedAt,
          {
            "trellis.backend": "kv",
            "trellis.operation": operation,
            "trellis.phase": "total",
            "trellis.outcome": outcome,
          },
        );
      }
    })());
  }

  /** Return the transport failure when `action` is not yet admitted. */
  async #admission(
    action: "read" | "write",
  ): Promise<TransportError | undefined> {
    return await this.transport?.(action);
  }

  #assertCurrent(): void {
    if (!this.isCurrent()) throw new Error("KV resource binding is stale");
  }
}
