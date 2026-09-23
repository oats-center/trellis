import { type KV, type KvEntry, Kvm } from "@nats-io/kv";
import type { NatsConnection } from "@nats-io/nats-core/internal";
import { AsyncResult, type BaseError, Result } from "@oatscenter/result";

import type { Codec } from "./generated.ts";
import { KVError, ValidationError } from "./errors/index.ts";
import { decodeSubject, escapeKvKey } from "./helpers.ts";

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
}>;

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
    context: key === undefined ? undefined : { key },
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

/** Typed access to a generated, versioned KV resource. */
export class TypedKV<T> {
  private constructor(
    readonly representation: KvRepresentation<T>,
    readonly kv: KV,
    readonly migrations: ResourceMigrations<T>,
    readonly isCurrent: () => boolean,
  ) {}

  /** Opens or creates a typed KV bucket. */
  static open<T>(
    nats: NatsConnection,
    name: string,
    representation: KvRepresentation<T>,
    options: KvOpenOptions<T> = {},
  ): AsyncResult<TypedKV<T>, KVError> {
    return AsyncResult.from((async () => {
      try {
        const kvm = new Kvm(nats);
        const kv = options.bindOnly
          ? await kvm.open(name)
          : await kvm.create(name, {
            history: options.history ?? 1,
            ttl: options.ttl ?? 0,
            ...(options.replicas === undefined
              ? {}
              : { replicas: options.replicas }),
            ...(options.maxValueBytes === undefined
              ? {}
              : { maxValueSize: options.maxValueBytes }),
          });
        await ensureExistingBucketOptions(kv, name, options);
        return Result.ok(
          new TypedKV(
            representation,
            kv,
            options.migrations ?? {},
            options.isCurrent ?? (() => true),
          ),
        );
      } catch (cause) {
        return Result.err(kvError("open", undefined, cause));
      }
    })());
  }

  /** Returns the current value, or `undefined` when absent or deleted. */
  get(key: string): AsyncResult<T | undefined, KVError | ValidationError> {
    return this.getEntry(key).map((entry) =>
      entry?.operation === "put" ? entry.value : undefined
    );
  }

  /** Returns the latest value or tombstone with authoritative revision metadata. */
  getEntry(
    key: string,
  ): AsyncResult<TypedKvEntry<T> | undefined, KVError | ValidationError> {
    return AsyncResult.from((async () => {
      try {
        this.#assertCurrent();
        const entry = await this.kv.get(escapeKvKey(key));
        return entry
          ? await decodeEntry(this.representation, this.migrations, entry)
          : Result.ok(undefined);
      } catch (cause) {
        return Result.err(kvError("get", key, cause));
      }
    })());
  }

  /** Creates a value only when the key has no current value. */
  create(key: string, value: T): AsyncResult<TypedKvEntry<T>, KVError> {
    return AsyncResult.from((async () => {
      try {
        this.#assertCurrent();
        const revision = await this.kv.create(
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
    })());
  }

  /** Writes a value without a revision precondition. */
  put(key: string, value: T): AsyncResult<TypedKvEntry<T>, KVError> {
    return AsyncResult.from((async () => {
      try {
        this.#assertCurrent();
        const revision = await this.kv.put(
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
    })());
  }

  /** Replaces a value only when its current revision matches. */
  replace(
    key: string,
    revision: ResourceRevision,
    value: T,
  ): AsyncResult<TypedKvEntry<T>, KVError> {
    return AsyncResult.from((async () => {
      try {
        this.#assertCurrent();
        const nextRevision = await this.kv.update(
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
    })());
  }

  /** Deletes a key, optionally requiring its current revision. */
  delete(key: string, revision?: ResourceRevision): AsyncResult<void, KVError> {
    return AsyncResult.from((async () => {
      try {
        this.#assertCurrent();
        await this.kv.delete(
          escapeKvKey(key),
          revision === undefined ? {} : { previousSeq: revision },
        );
        return Result.ok(undefined);
      } catch (cause) {
        return Result.err(kvError("delete", key, cause));
      }
    })());
  }

  /** Returns all retained revisions for a key, including tombstones. */
  history(
    key: string,
  ): AsyncResult<readonly TypedKvEntry<T>[], KVError | ValidationError> {
    return AsyncResult.from((async () => {
      try {
        this.#assertCurrent();
        const history = await this.kv.history({ key: escapeKvKey(key) });
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
    })());
  }

  /** Watches retained initialization and subsequent revisions for one key. */
  watch(
    key: string,
  ): AsyncResult<AsyncIterable<KvWatchItem<T>>, KVError> {
    return AsyncResult.from((async () => {
      try {
        this.#assertCurrent();
        const watcher = await this.kv.watch({
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
    })());
  }

  /** Lists live keys using the backend's bounded iterator. */
  keys(
    filter: string | string[] = ">",
  ): AsyncResult<AsyncIterable<string>, KVError> {
    return AsyncResult.from((async () => {
      try {
        this.#assertCurrent();
        return Result.ok(await this.kv.keys(filter));
      } catch (cause) {
        return Result.err(kvError("keys", undefined, cause));
      }
    })());
  }

  /** Returns the backend live-value count. */
  status(): AsyncResult<{ values: number }, KVError> {
    return AsyncResult.from((async () => {
      try {
        this.#assertCurrent();
        return Result.ok({ values: (await this.kv.status()).values });
      } catch (cause) {
        return Result.err(kvError("status", undefined, cause));
      }
    })());
  }

  #assertCurrent(): void {
    if (!this.isCurrent()) throw new Error("KV resource binding is stale");
  }
}
