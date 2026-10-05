import { type KV, type KvEntry, Kvm } from "@nats-io/kv";
import type { NatsConnection } from "@nats-io/nats-core/internal";
import { AsyncResult, type BaseError, isErr, Result } from "@oatscenter/result";
import { JetStreamApiCodes } from "@nats-io/jetstream";

import type { Codec } from "./generated.ts";
import { KVError, TransportError, ValidationError } from "./errors/index.ts";
import type {
  TransportLease,
  TransportRequirement,
  TrellisTransportProvider,
} from "./transport/generations.ts";
import { decodeSubject, escapeKvKey } from "./helpers.ts";
import { recordCatalogDuration } from "./telemetry/metrics.ts";

const KV_MAGIC = new Uint8Array([0x54, 0x52, 0x4b, 0x56]);
const KV_ENVELOPE_FORMAT = 1;
const KV_ENVELOPE_HEADER_BYTES = 9;
const textEncoder = new TextEncoder();
const textDecoder = new TextDecoder("utf-8", { fatal: true });
/** Default budget for acquiring a transport generation for one KV operation. */
const DEFAULT_RESOURCE_ACQUIRE_TIMEOUT_MS = 30_000;

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
  /**
   * Budget for acquiring a transport generation on a finite operation.
   * Defaults to 30 seconds.
   */
  acquireTimeoutMs?: number;
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
  readonly #transport: TrellisTransportProvider;
  readonly #options: KvOpenOptions<T>;
  readonly #acquireTimeoutMs: number;
  /**
   * One generation-local backend per physical connection.
   *
   * Keyed by the connection object so a public handle keeps working across an
   * automatic generation adoption without sharing an adapter between a drained
   * generation and its replacement.
   */
  readonly #adapters = new WeakMap<NatsConnection, KV>();

  private constructor(
    transport: TrellisTransportProvider,
    readonly name: string,
    readonly representation: KvRepresentation<T>,
    readonly migrations: ResourceMigrations<T>,
    readonly isCurrent: () => boolean,
    options: KvOpenOptions<T>,
  ) {
    this.#transport = transport;
    this.#options = options;
    this.#acquireTimeoutMs = options.acquireTimeoutMs ??
      DEFAULT_RESOURCE_ACQUIRE_TIMEOUT_MS;
  }

  /** Opens or creates a typed KV bucket on the current generation. */
  static open<T>(
    transport: TrellisTransportProvider,
    name: string,
    representation: KvRepresentation<T>,
    options: KvOpenOptions<T> = {},
  ): AsyncResult<TypedKV<T>, KVError> {
    return AsyncResult.from((async () => {
      const handle = new TypedKV(
        transport,
        name,
        representation,
        options.migrations ?? {},
        options.isCurrent ?? (() => true),
        options,
      );
      try {
        await handle.#materialize();
        return Result.ok(handle);
      } catch (cause) {
        return Result.err(kvError("open", undefined, cause));
      }
    })());
  }

  /**
   * Binds a typed KV bucket without opening it.
   *
   * The backing bucket is materialized against the generation acquired for the
   * first operation, after that operation's exact transport requirement is
   * admitted, so a resource whose broker subjects are not yet adopted never
   * performs an unauthorized NATS request.
   * @internal
   */
  static bind<T>(
    transport: TrellisTransportProvider,
    name: string,
    representation: KvRepresentation<T>,
    options: KvOpenOptions<T> = {},
  ): TypedKV<T> {
    return new TypedKV(
      transport,
      name,
      representation,
      options.migrations ?? {},
      options.isCurrent ?? (() => true),
      options,
    );
  }

  /** Exact broker subjects one KV action needs on its generation. */
  #requirement(action: "read" | "write"): TransportRequirement {
    return action === "read"
      ? { publish: [`$JS.API.DIRECT.GET.KV_${this.name}`] }
      : { publish: [`$KV.${this.name}.>`] };
  }

  /**
   * Acquire a generation covering `action`, or the current generation for an
   * unclassified open.
   */
  #lease(action?: "read" | "write"): Promise<TransportLease> {
    return this.#transport.acquireFor(
      action === undefined ? {} : this.#requirement(action),
      { deadlineMs: Date.now() + this.#acquireTimeoutMs },
    );
  }

  /** Return the generation-local backend for `nc`, opening it once per nc. */
  async #adapter(nc: NatsConnection): Promise<KV> {
    let kv = this.#adapters.get(nc);
    if (!kv) {
      kv = await openKv(nc, this.name, this.#options);
      this.#adapters.set(nc, kv);
    }
    return kv;
  }

  /**
   * Acquire a generation and its backend for one finite operation.
   *
   * The caller must invoke `release` in a `finally` once the exchange ends. A
   * backend that fails to open releases the generation before rethrowing.
   */
  async #acquire(
    action?: "read" | "write",
  ): Promise<{ kv: KV; release: () => void }> {
    const lease = await this.#lease(action);
    try {
      const kv = await this.#adapter(lease.nc);
      return { kv, release: () => lease.release() };
    } catch (cause) {
      lease.release();
      throw cause;
    }
  }

  /** Eagerly open the bucket against the current generation. */
  async #materialize(): Promise<void> {
    const lease = await this.#lease();
    try {
      await this.#adapter(lease.nc);
    } finally {
      lease.release();
    }
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
        const acquired = await this.#acquire("read");
        try {
          const entry = await acquired.kv.get(escapeKvKey(key));
          return entry
            ? await decodeEntry(this.representation, this.migrations, entry)
            : Result.ok(undefined);
        } finally {
          acquired.release();
        }
      } catch (cause) {
        return Result.err(
          cause instanceof TransportError ? cause : kvError("get", key, cause),
        );
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
          const acquired = await this.#acquire("write");
          try {
            const revision = await acquired.kv.create(
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
          } finally {
            acquired.release();
          }
        } catch (cause) {
          return Result.err(
            cause instanceof TransportError
              ? cause
              : kvError("create", key, cause),
          );
        }
      })()));
  }

  /** Writes a value without a revision precondition. */
  put(key: string, value: T): AsyncResult<TypedKvEntry<T>, KVOperationError> {
    return this.#observe("write", () =>
      AsyncResult.from((async () => {
        try {
          this.#assertCurrent();
          const acquired = await this.#acquire("write");
          try {
            const revision = await acquired.kv.put(
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
          } finally {
            acquired.release();
          }
        } catch (cause) {
          return Result.err(
            cause instanceof TransportError
              ? cause
              : kvError("put", key, cause),
          );
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
          const acquired = await this.#acquire("write");
          try {
            const nextRevision = await acquired.kv.update(
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
          } finally {
            acquired.release();
          }
        } catch (cause) {
          return Result.err(
            cause instanceof TransportError
              ? cause
              : kvError("replace", key, cause),
          );
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
          const acquired = await this.#acquire("write");
          try {
            await acquired.kv.delete(
              escapeKvKey(key),
              revision === undefined ? {} : { previousSeq: revision },
            );
            return Result.ok(undefined);
          } finally {
            acquired.release();
          }
        } catch (cause) {
          return Result.err(
            cause instanceof TransportError
              ? cause
              : kvError("delete", key, cause),
          );
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
          const acquired = await this.#acquire("read");
          try {
            const history = await acquired.kv.history({
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
          } finally {
            acquired.release();
          }
        } catch (cause) {
          return Result.err(
            cause instanceof TransportError
              ? cause
              : kvError("history", key, cause),
          );
        }
      })()));
  }

  /**
   * Watches retained initialization and subsequent revisions. A key retains its
   * history; omitting the key watches the bucket's last values and updates.
   * Aborting stops the watcher and releases its pinned transport generation.
   */
  watch(
    key?: string,
    options: { signal?: AbortSignal } = {},
  ): AsyncResult<AsyncIterable<KvWatchItem<T>>, KVOperationError> {
    return this.#observe("watch_setup", () =>
      AsyncResult.from((async () => {
        try {
          this.#assertCurrent();
          options.signal?.throwIfAborted();
          // A watcher is a pinned observation: it holds its generation until the
          // iteration ends so a rollover cannot retire the backend underneath it.
          const acquired = await this.#acquire("read");
          let watcher;
          try {
            watcher = await acquired.kv.watch({
              ...(key === undefined ? {} : { key: escapeKvKey(key) }),
              include: key === undefined ? "" : "history",
            });
          } catch (cause) {
            acquired.release();
            throw cause;
          }
          const representation = this.representation;
          const migrations = this.migrations;
          const isCurrent = this.isCurrent;
          let released = false;
          const stop = () => {
            watcher.stop();
            if (!released) {
              released = true;
              acquired.release();
            }
          };
          options.signal?.addEventListener("abort", stop, { once: true });
          if (options.signal?.aborted) stop();
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
                stop();
                options.signal?.removeEventListener("abort", stop);
              }
            },
          });
        } catch (cause) {
          return Result.err(
            cause instanceof TransportError
              ? cause
              : kvError("watch", key, cause),
          );
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
          // The key iterator holds its generation until it is exhausted so a
          // rollover cannot retire the backend mid-listing.
          const acquired = await this.#acquire("read");
          let keys: AsyncIterable<string>;
          try {
            keys = await acquired.kv.keys(filter);
          } catch (cause) {
            acquired.release();
            throw cause;
          }
          return Result.ok({
            async *[Symbol.asyncIterator]() {
              try {
                yield* keys;
              } catch (cause) {
                throw kvError("keys", undefined, cause);
              } finally {
                acquired.release();
              }
            },
          });
        } catch (cause) {
          return Result.err(
            cause instanceof TransportError
              ? cause
              : kvError("keys", undefined, cause),
          );
        }
      })()));
  }

  /** Returns the backend live-value count. */
  status(): AsyncResult<{ values: number }, KVOperationError> {
    return this.#observe("read", () =>
      AsyncResult.from((async () => {
        try {
          this.#assertCurrent();
          const acquired = await this.#acquire("read");
          try {
            return Result.ok({ values: (await acquired.kv.status()).values });
          } finally {
            acquired.release();
          }
        } catch (cause) {
          return Result.err(
            cause instanceof TransportError
              ? cause
              : kvError("status", undefined, cause),
          );
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

  #assertCurrent(): void {
    if (!this.isCurrent()) throw new Error("KV resource binding is stale");
  }
}
