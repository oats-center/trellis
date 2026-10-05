import {
  type ObjectInfo,
  type ObjectStore,
  type ObjectStoreStatus,
  Objm,
} from "@nats-io/obj";
import type { NatsConnection } from "@nats-io/nats-core/internal";
import {
  AsyncResult,
  Result,
  type Result as ResultType,
} from "@oatscenter/result";
import { StoreError, TransportError } from "./errors/index.ts";
import type {
  TransportLease,
  TransportRequirement,
  TrellisTransportProvider,
} from "./transport/generations.ts";
import {
  decodePaginationCursor,
  encodePaginationCursor,
  paginationQueryDigest,
} from "./auth/protocol_wasm.ts";
import { TypedStoreEntry } from "./store_entry.ts";
import { boundedObjectStream } from "./store_reader.ts";
export { TypedStoreEntry } from "./store_entry.ts";

const INTERNAL_CONTENT_TYPE_METADATA_KEY = "__trellis_content_type";
const DEFAULT_STORE_WAIT_POLL_INTERVAL_MS = 250;
/** Default budget for acquiring a transport generation for one Store operation. */
const DEFAULT_RESOURCE_ACQUIRE_TIMEOUT_MS = 30_000;
const MAX_STORE_LIST_LIMIT = 500;
const DEFAULT_STORE_LIST_LIMIT = 100;
const STORE_LIST_CURSOR_ENDPOINT = "trellis.store.list";

export type StoreBody =
  | Uint8Array
  | ReadableStream<Uint8Array>
  | AsyncIterable<Uint8Array>;

export type StoreWaitOptions = {
  timeoutMs?: number;
  pollIntervalMs?: number;
  signal?: AbortSignal;
};

export type StoreOpenOptions = {
  ttlMs?: number;
  maxObjectBytes?: number;
  maxTotalBytes?: number;
  bindOnly?: boolean;
  isCurrent?: () => boolean;
  /**
   * Budget for acquiring a transport generation on a finite operation.
   * Defaults to 30 seconds.
   */
  acquireTimeoutMs?: number;
};

/** Failures a Store operation can report. */
export type StoreOperationError = StoreError | TransportError;

export type StorePutOptions = {
  contentType?: string;
  metadata?: Record<string, string>;
};

/** Explicit bounded query for listing object metadata in a typed store. */
export type StoreListOptions = {
  prefix?: string;
  cursor?: string;
  limit?: number;
};

/** One key-sorted page of object metadata and its opaque continuation. */
export type StoreListPage = {
  entries: StoreInfo[];
  nextCursor?: string;
};

export type StoreInfo = {
  key: string;
  size: number;
  updatedAt: string;
  digest?: string;
  contentType?: string;
  metadata: Record<string, string>;
};

export type StoreStatus = {
  size: number;
  sealed: boolean;
  ttlMs: number;
  maxObjectBytes?: number;
  maxTotalBytes?: number;
};

/** Structural ObjectStore read surface used by a returned store entry. */
type ObjectStoreLike = {
  get(key: string): Promise<ReadableStream<Uint8Array> | null>;
};

function metadataWithContentType(
  options?: StorePutOptions,
): Record<string, string> | undefined {
  if (!options?.metadata && !options?.contentType) {
    return undefined;
  }

  return {
    ...(options?.metadata ?? {}),
    ...(options?.contentType
      ? { [INTERNAL_CONTENT_TYPE_METADATA_KEY]: options.contentType }
      : {}),
  };
}

function storeInfoFromObjectInfo(info: ObjectInfo): StoreInfo {
  const { [INTERNAL_CONTENT_TYPE_METADATA_KEY]: contentType, ...metadata } =
    info.metadata ?? {};
  return {
    key: info.name,
    size: info.size,
    updatedAt: info.mtime,
    ...(info.digest ? { digest: info.digest } : {}),
    ...(contentType ? { contentType } : {}),
    metadata,
  };
}

function compareStoreKeys(left: string, right: string): number {
  const leftBytes = new TextEncoder().encode(left);
  const rightBytes = new TextEncoder().encode(right);
  for (
    let index = 0;
    index < Math.min(leftBytes.length, rightBytes.length);
    index++
  ) {
    if (leftBytes[index] !== rightBytes[index]) {
      return leftBytes[index] - rightBytes[index];
    }
  }
  return leftBytes.length - rightBytes.length;
}

function streamFromAsyncIterable(
  iterable: AsyncIterable<Uint8Array>,
): ReadableStream<Uint8Array> {
  const iterator = iterable[Symbol.asyncIterator]();
  return new ReadableStream<Uint8Array>({
    async pull(controller) {
      const next = await iterator.next();
      if (next.done) {
        controller.close();
        return;
      }
      controller.enqueue(next.value);
    },
    async cancel(reason) {
      await iterator.return?.(reason);
    },
  });
}

function validateStoreListOptions(
  opts: StoreListOptions = {},
): ResultType<Required<StoreListOptions>, StoreError> {
  const limit = opts.limit ?? DEFAULT_STORE_LIST_LIMIT;
  if (!Number.isInteger(limit) || limit <= 0) {
    return Result.err(
      new StoreError({
        operation: "list",
        context: { reason: "invalid_limit", limit },
      }),
    );
  }
  if (limit > MAX_STORE_LIST_LIMIT) {
    return Result.err(
      new StoreError({
        operation: "list",
        context: {
          reason: "limit_exceeded",
          limit,
          maxLimit: MAX_STORE_LIST_LIMIT,
        },
      }),
    );
  }

  const cursor = opts.cursor ?? "";
  if (opts.cursor !== undefined && cursor.length === 0) {
    return Result.err(
      new StoreError({
        operation: "list",
        context: { reason: "invalid_cursor" },
      }),
    );
  }

  return Result.ok({ prefix: opts.prefix ?? "", cursor, limit });
}

function enforceMaxObjectBytes(
  stream: ReadableStream<Uint8Array>,
  maxObjectBytes?: number,
): ReadableStream<Uint8Array> {
  if (maxObjectBytes === undefined) {
    return stream;
  }

  const reader = stream.getReader();
  let totalBytes = 0;

  return new ReadableStream<Uint8Array>({
    async pull(controller) {
      const next = await reader.read();
      if (next.done) {
        controller.close();
        return;
      }

      totalBytes += next.value.length;
      if (totalBytes > maxObjectBytes) {
        controller.error(
          new StoreError({
            operation: "put",
            context: {
              reason: "max_object_bytes_exceeded",
              maxObjectBytes,
              attemptedBytes: totalBytes,
            },
          }),
        );
        await reader.cancel();
        return;
      }

      controller.enqueue(next.value);
    },
    async cancel(reason) {
      await reader.cancel(reason);
    },
  });
}

async function bytesFromStream(
  stream: ReadableStream<Uint8Array>,
): Promise<Uint8Array> {
  const reader = stream.getReader();
  const chunks: Uint8Array[] = [];
  let totalLength = 0;

  while (true) {
    const next = await reader.read();
    if (next.done) {
      break;
    }
    chunks.push(next.value);
    totalLength += next.value.length;
  }

  const merged = new Uint8Array(totalLength);
  let offset = 0;
  for (const chunk of chunks) {
    merged.set(chunk, offset);
    offset += chunk.length;
  }
  return merged;
}

function isNotFoundStoreError(error: StoreError | TransportError): boolean {
  if (!(error instanceof StoreError)) return false;
  return error.getContext().reason === "not_found";
}

/** Whether a Store operation failed because transport has not adopted it yet. */
function isTransportError(
  error: StoreError | TransportError,
): error is TransportError {
  return error instanceof TransportError;
}

/** Opens or creates a physical object store with the requested options. */
async function openStore(
  nats: NatsConnection,
  name: string,
  options: StoreOpenOptions,
): Promise<ObjectStore> {
  const objm = new Objm(nats);
  const store = options.bindOnly
    ? await objm.open(name)
    : await objm.create(name, {
      ...(options.ttlMs && options.ttlMs > 0
        ? { ttl: options.ttlMs * 1_000_000 }
        : {}),
      ...(options.maxTotalBytes !== undefined
        ? { max_bytes: options.maxTotalBytes }
        : {}),
    });
  await ensureExistingStoreOptions(store, name, options);
  return store;
}

/** Build the open failure for a Store binding. */
function storeOpenError(name: string, cause: unknown): StoreError {
  return new StoreError({
    operation: "open",
    cause,
    context: {
      name,
      ...(cause instanceof Error && "code" in cause
        ? { code: cause.code }
        : {}),
      ...(cause instanceof Error && "subject" in cause
        ? { subject: cause.subject }
        : {}),
    },
  });
}

function storeAbortedError(
  operation: string,
  key: string,
  cause: unknown,
): StoreError {
  return new StoreError({
    operation,
    cause,
    context: { key, reason: "aborted" },
  });
}

function storeTimeoutError(operation: string, key: string): StoreError {
  return new StoreError({
    operation,
    context: { key, reason: "timeout" },
  });
}

/**
 * Settle `operation` within an absolute deadline and optional abort signal.
 *
 * Object/KV APIs do not accept a per-call timeout for backend open or metadata
 * reads, so the caller's budget is enforced at this boundary; the abandoned
 * request keeps running under the connection's own timeout.
 */
function withinDeadline<T>(
  operation: Promise<T>,
  deadlineMs: number,
  signal: AbortSignal | undefined,
  onTimeout: () => Error,
  onAbort: () => Error,
): Promise<T> {
  const remaining = deadlineMs - Date.now();
  if (remaining <= 0) return Promise.reject(onTimeout());
  if (signal?.aborted) return Promise.reject(onAbort());
  return new Promise<T>((resolve, reject) => {
    let settled = false;
    const finish = (): boolean => {
      if (settled) return false;
      settled = true;
      clearTimeout(timer);
      signal?.removeEventListener("abort", onSignalAbort);
      return true;
    };
    const succeed = (value: T) => {
      if (finish()) resolve(value);
    };
    const fail = (cause: unknown) => {
      if (finish()) reject(cause);
    };
    const timer = setTimeout(() => fail(onTimeout()), remaining);
    const onSignalAbort = () => fail(onAbort());
    signal?.addEventListener("abort", onSignalAbort, { once: true });
    operation.then(succeed, fail);
  });
}

async function sleepWithSignal(
  ms: number,
  signal?: AbortSignal,
): Promise<void> {
  if (signal?.aborted) {
    throw signal.reason ??
      new DOMException("The operation was aborted", "AbortError");
  }

  await new Promise<void>((resolve, reject) => {
    const timeoutId = setTimeout(() => {
      cleanup();
      resolve();
    }, ms);

    const onAbort = () => {
      cleanup();
      reject(
        signal?.reason ??
          new DOMException("The operation was aborted", "AbortError"),
      );
    };

    const cleanup = () => {
      clearTimeout(timeoutId);
      signal?.removeEventListener("abort", onAbort);
    };

    signal?.addEventListener("abort", onAbort, { once: true });
  });
}

function streamFromBody(
  body: Exclude<StoreBody, Uint8Array>,
): ReadableStream<Uint8Array> {
  return body instanceof ReadableStream ? body : streamFromAsyncIterable(body);
}

/**
 * Release a generation lease once the object data stream settles.
 *
 * An entry stream is a physical read exchange, so it holds its generation until
 * the reader finishes, errors, or cancels.
 */
async function unwrapObjectInfo(
  store: ObjectStore,
  key: string,
): Promise<ResultType<ObjectInfo, StoreError>> {
  try {
    const info = await store.info(key);
    if (info === null || info.deleted) {
      return Result.err(
        new StoreError({
          operation: "get",
          context: { key, reason: "not_found" },
        }),
      );
    }
    return Result.ok(info);
  } catch (cause) {
    return Result.err(
      new StoreError({ operation: "get", cause, context: { key } }),
    );
  }
}

type ExistingStoreStatus = {
  ttl: number;
};

/** Verifies that an existing physical store matches its effective binding. */
export async function ensureExistingStoreOptions(
  store: { status(): Promise<ExistingStoreStatus> },
  name: string,
  options: StoreOpenOptions,
): Promise<void> {
  const status = await store.status();
  const actualTtlMs = status.ttl > 0 ? Math.floor(status.ttl / 1_000_000) : 0;
  if (options.ttlMs !== undefined && actualTtlMs !== options.ttlMs) {
    throw new Error(`Store '${name}' TTL does not match its binding`);
  }
}

export class TypedStore {
  readonly #transport: TrellisTransportProvider;
  readonly #name: string;
  readonly #acquireTimeoutMs: number;
  /**
   * Normalized open options.
   *
   * `ttlMs` stays absent when the caller did not declare one, so binding to an
   * existing store (for example the Trellis-owned operation staging bucket,
   * whose server-side TTL is an implementation detail) does not fail the
   * binding-match check against an unstated default.
   */
  readonly #options: StoreOpenOptions;
  /**
   * One generation-local backend per physical connection.
   *
   * Keyed by the connection object so a public handle keeps working across an
   * automatic generation adoption without sharing an adapter between a drained
   * generation and its replacement.
   */
  readonly #adapters = new WeakMap<NatsConnection, ObjectStore>();

  private constructor(
    transport: TrellisTransportProvider,
    name: string,
    options: StoreOpenOptions,
  ) {
    this.#transport = transport;
    this.#name = name;
    this.#acquireTimeoutMs = options.acquireTimeoutMs ??
      DEFAULT_RESOURCE_ACQUIRE_TIMEOUT_MS;
    this.#options = {
      ...(options.ttlMs !== undefined ? { ttlMs: options.ttlMs } : {}),
      ...(options.maxObjectBytes !== undefined
        ? { maxObjectBytes: options.maxObjectBytes }
        : {}),
      ...(options.maxTotalBytes !== undefined
        ? { maxTotalBytes: options.maxTotalBytes }
        : {}),
      ...(options.bindOnly !== undefined ? { bindOnly: options.bindOnly } : {}),
    };
  }

  /** Opens or creates a typed object store on the current generation. */
  static open(
    transport: TrellisTransportProvider,
    name: string,
    options: StoreOpenOptions = {},
  ): AsyncResult<TypedStore, StoreError> {
    return AsyncResult.from((async () => {
      const handle = new TypedStore(transport, name, options);
      try {
        await handle.#materialize();
        return Result.ok(handle);
      } catch (cause) {
        return Result.err(storeOpenError(name, cause));
      }
    })());
  }

  /**
   * Binds a typed object store without opening it.
   *
   * The backing store is materialized against the generation acquired for the
   * first operation, after that operation's exact transport requirement is
   * admitted, so a resource whose broker subjects are not yet adopted never
   * performs an unauthorized NATS request.
   * @internal
   */
  static bind(
    transport: TrellisTransportProvider,
    name: string,
    options: StoreOpenOptions = {},
  ): TypedStore {
    return new TypedStore(transport, name, options);
  }

  /** Exact broker subjects one Store action needs on its generation. */
  #requirement(action: "read" | "write"): TransportRequirement {
    return action === "read"
      ? { publish: [`$JS.API.STREAM.INFO.OBJ_${this.#name}`] }
      : { publish: [`$O.${this.#name}.C.>`] };
  }

  /**
   * Acquire a generation covering `action`, or the current generation for an
   * unclassified open.
   */
  #lease(
    action?: "read" | "write",
    deadlineMs?: number,
    signal?: AbortSignal,
  ): Promise<TransportLease> {
    return this.#transport.acquireFor(
      action === undefined ? {} : this.#requirement(action),
      {
        deadlineMs: deadlineMs ?? Date.now() + this.#acquireTimeoutMs,
        ...(signal ? { signal } : {}),
      },
    );
  }

  /** Return the generation-local backend for `nc`, opening it once per nc. */
  async #adapter(nc: NatsConnection): Promise<ObjectStore> {
    let store = this.#adapters.get(nc);
    if (!store) {
      store = await openStore(nc, this.#name, this.#options);
      this.#adapters.set(nc, store);
    }
    return store;
  }

  /**
   * Acquire a generation and its backend for one finite operation.
   *
   * The caller must invoke `release` in a `finally` once the exchange ends. A
   * backend that fails to open releases the generation before rethrowing.
   */
  async #acquire(
    action?: "read" | "write",
    deadlineMs?: number,
    signal?: AbortSignal,
  ): Promise<{ store: ObjectStore; nc: NatsConnection; release: () => void }> {
    const lease = await this.#lease(action, deadlineMs, signal);
    try {
      const store = await this.#adapter(lease.nc);
      return { store, nc: lease.nc, release: () => lease.release() };
    } catch (cause) {
      lease.release();
      throw cause;
    }
  }

  /** Eagerly open the store against the current generation. */
  async #materialize(): Promise<void> {
    const lease = await this.#lease();
    try {
      await this.#adapter(lease.nc);
    } finally {
      lease.release();
    }
  }

  create(
    key: string,
    body: StoreBody,
    options?: StorePutOptions,
  ): AsyncResult<void, StoreOperationError> {
    return AsyncResult.from(this.#putInternal("create", key, body, options, 0));
  }

  put(
    key: string,
    body: StoreBody,
    options?: StorePutOptions,
  ): AsyncResult<void, StoreOperationError> {
    return AsyncResult.from(this.#putInternal("put", key, body, options));
  }

  get(key: string): AsyncResult<TypedStoreEntry, StoreOperationError> {
    return AsyncResult.from((async (): Promise<
      ResultType<TypedStoreEntry, StoreOperationError>
    > => {
      return await this.#getEntry(key, Date.now() + this.#acquireTimeoutMs);
    })());
  }

  /**
   * Acquire a read generation and return the entry metadata for `key`.
   *
   * The deadline bounds the whole attempt, not just acquisition: backend open
   * and the metadata read are enforced against the remaining budget at this
   * boundary because the object API cannot time them out per call.
   */
  async #getEntry(
    key: string,
    deadlineMs: number,
    signal?: AbortSignal,
  ): Promise<ResultType<TypedStoreEntry, StoreOperationError>> {
    if (!this.#isCurrent()) return this.#stale("get", key);
    try {
      return await withinDeadline(
        this.#readMetadata(key, deadlineMs, signal),
        deadlineMs,
        signal,
        () => storeTimeoutError("get", key),
        () => storeAbortedError("get", key, signal?.reason),
      );
    } catch (cause) {
      return Result.err(
        cause instanceof StoreError || cause instanceof TransportError
          ? cause
          : new StoreError({ operation: "get", cause, context: { key } }),
      );
    }
  }

  async #readMetadata(
    key: string,
    deadlineMs: number,
    signal?: AbortSignal,
  ): Promise<ResultType<TypedStoreEntry, StoreOperationError>> {
    let acquired: { store: ObjectStore; release: () => void };
    try {
      acquired = await this.#acquire("read", deadlineMs, signal);
    } catch (cause) {
      return Result.err(
        cause instanceof TransportError
          ? cause
          : new StoreError({ operation: "get", cause, context: { key } }),
      );
    }
    try {
      const info = await unwrapObjectInfo(acquired.store, key);
      return info.map((objectInfo) =>
        new TypedStoreEntry(
          this.#entryStore(),
          storeInfoFromObjectInfo(objectInfo),
        )
      );
    } finally {
      acquired.release();
    }
  }

  /**
   * ObjectStore-shaped read surface for a returned entry.
   *
   * Every call re-acquires a generation, so an entry stays valid after an
   * automatic adoption instead of pinning the generation it was read from.
   */
  #entryStore(): ObjectStoreLike {
    return {
      get: (key) => this.#entryGet(key),
    };
  }

  async #entryGet(key: string): Promise<ReadableStream<Uint8Array> | null> {
    const acquired = await this.#acquire("read");
    try {
      const info = await acquired.store.info(key);
      if (info === null || info.deleted) {
        acquired.release();
        return null;
      }
      if (info.name !== key) {
        throw new Error(
          "ObjectStore metadata does not match the requested key",
        );
      }
      return boundedObjectStream(
        acquired.nc,
        this.#name,
        info,
        acquired.release,
      );
    } catch (cause) {
      acquired.release();
      throw cause;
    }
  }

  /**
   * Waits for an object key to appear in the store and returns the resulting entry.
   */
  waitFor(
    key: string,
    options: StoreWaitOptions = {},
  ): AsyncResult<TypedStoreEntry, StoreOperationError> {
    return AsyncResult.from((async (): Promise<
      ResultType<TypedStoreEntry, StoreOperationError>
    > => {
      const startedAt = Date.now();
      const pollIntervalMs = options.pollIntervalMs ??
        DEFAULT_STORE_WAIT_POLL_INTERVAL_MS;

      while (true) {
        if (options.signal?.aborted) {
          return Result.err(
            storeAbortedError("waitFor", key, options.signal.reason),
          );
        }

        // Transport acquisition counts against the caller's wait budget.
        const deadlineMs = options.timeoutMs === undefined
          ? Date.now() + this.#acquireTimeoutMs
          : Math.min(
            Date.now() + this.#acquireTimeoutMs,
            startedAt + options.timeoutMs,
          );
        const entry = await this.#getEntry(key, deadlineMs, options.signal);
        if (entry.isOk()) {
          return entry;
        }
        if (
          !isNotFoundStoreError(entry.error) || isTransportError(entry.error)
        ) {
          return entry;
        }

        const remainingTimeoutMs = options.timeoutMs === undefined
          ? undefined
          : options.timeoutMs - (Date.now() - startedAt);
        if (remainingTimeoutMs !== undefined && remainingTimeoutMs <= 0) {
          return Result.err(
            new StoreError({
              operation: "waitFor",
              context: { key, reason: "timeout", timeoutMs: options.timeoutMs },
            }),
          );
        }

        try {
          await sleepWithSignal(
            remainingTimeoutMs === undefined
              ? pollIntervalMs
              : Math.min(pollIntervalMs, remainingTimeoutMs),
            options.signal,
          );
        } catch (cause) {
          return Result.err(storeAbortedError("waitFor", key, cause));
        }
      }
    })());
  }

  delete(key: string): AsyncResult<void, StoreOperationError> {
    return AsyncResult.from((async () => {
      if (!this.#isCurrent()) return this.#stale("delete", key);
      let acquired: { store: ObjectStore; release: () => void };
      try {
        acquired = await this.#acquire("write");
      } catch (cause) {
        return Result.err(
          cause instanceof TransportError
            ? cause
            : new StoreError({ operation: "delete", cause, context: { key } }),
        );
      }
      try {
        await acquired.store.delete(key);
        return Result.ok(undefined);
      } catch (cause) {
        return Result.err(
          new StoreError({ operation: "delete", cause, context: { key } }),
        );
      } finally {
        acquired.release();
      }
    })());
  }

  list(
    opts: StoreListOptions = {},
  ): AsyncResult<StoreListPage, StoreOperationError> {
    return AsyncResult.from((async () => {
      if (!this.#isCurrent()) return this.#stale("list");
      const query = validateStoreListOptions(opts);
      if (query.isErr()) return Result.err(query.error);

      const { prefix, cursor, limit } = query.unwrapOrElse(() => {
        throw new Error("unreachable");
      });
      let acquired: { store: ObjectStore; release: () => void };
      try {
        acquired = await this.#acquire("read");
      } catch (cause) {
        return Result.err(
          cause instanceof TransportError
            ? cause
            : new StoreError({ operation: "list", cause, context: { prefix } }),
        );
      }
      try {
        const queryDigest = await paginationQueryDigest(
          STORE_LIST_CURSOR_ENDPOINT,
          { prefix },
        );
        let after = "";
        if (cursor) {
          const decoded = await decodePaginationCursor<unknown>(
            cursor,
            queryDigest,
          );
          if (typeof decoded !== "string") {
            throw new Error("invalid pagination cursor");
          }
          after = decoded;
        }
        const objects = await acquired.store.list();
        const filtered = objects
          .filter((info) => !info.deleted && info.name.startsWith(prefix))
          .map(storeInfoFromObjectInfo)
          .sort((left, right) => compareStoreKeys(left.key, right.key))
          .filter((info) => compareStoreKeys(info.key, after) > 0);
        const entries = filtered.slice(0, limit);

        return Result.ok({
          entries,
          ...(filtered.length > limit
            ? {
              nextCursor: await encodePaginationCursor(
                queryDigest,
                entries.at(-1)?.key,
              ),
            }
            : {}),
        });
      } catch (cause) {
        return Result.err(
          new StoreError({ operation: "list", cause, context: { prefix } }),
        );
      } finally {
        acquired.release();
      }
    })());
  }

  status(): AsyncResult<StoreStatus, StoreOperationError> {
    return AsyncResult.from((async () => {
      if (!this.#isCurrent()) return this.#stale("status");
      let acquired: { store: ObjectStore; release: () => void };
      try {
        acquired = await this.#acquire("read");
      } catch (cause) {
        return Result.err(
          cause instanceof TransportError
            ? cause
            : new StoreError({ operation: "status", cause }),
        );
      }
      try {
        const status = await acquired.store.status();
        return Result.ok(
          storeStatusFromObjectStoreStatus(status, this.#options),
        );
      } catch (cause) {
        return Result.err(new StoreError({ operation: "status", cause }));
      } finally {
        acquired.release();
      }
    })());
  }

  async #putInternal(
    operation: "create" | "put",
    key: string,
    body: StoreBody,
    options?: StorePutOptions,
    previousRevision?: number,
  ): Promise<ResultType<void, StoreOperationError>> {
    try {
      if (!this.#isCurrent()) return this.#stale(operation, key);
      const metadata = metadataWithContentType(options);
      if (body instanceof Uint8Array) {
        if (
          this.#options.maxObjectBytes !== undefined &&
          body.length > this.#options.maxObjectBytes
        ) {
          return Result.err(
            new StoreError({
              operation,
              context: {
                key,
                reason: "max_object_bytes_exceeded",
                maxObjectBytes: this.#options.maxObjectBytes,
                attemptedBytes: body.length,
              },
            }),
          );
        }
      }

      let acquired: { store: ObjectStore; release: () => void };
      try {
        acquired = await this.#acquire("write");
      } catch (cause) {
        return Result.err(
          cause instanceof TransportError
            ? cause
            : new StoreError({ operation, cause, context: { key } }),
        );
      }
      try {
        if (body instanceof Uint8Array) {
          await acquired.store.putBlob(
            { name: key, ...(metadata ? { metadata } : {}) },
            body,
            previousRevision === undefined ? undefined : { previousRevision },
          );
        } else {
          const limitedStream = enforceMaxObjectBytes(
            streamFromBody(body),
            this.#options.maxObjectBytes,
          );
          await acquired.store.put(
            { name: key, ...(metadata ? { metadata } : {}) },
            limitedStream,
            previousRevision === undefined ? undefined : { previousRevision },
          );
        }
        return Result.ok(undefined);
      } finally {
        acquired.release();
      }
    } catch (cause) {
      return Result.err(
        cause instanceof StoreError
          ? cause
          : new StoreError({ operation, cause, context: { key } }),
      );
    }
  }

  #isCurrent(): boolean {
    return this.#options.isCurrent?.() ?? true;
  }

  #stale(operation: string, key?: string): ResultType<never, StoreError> {
    return Result.err(
      new StoreError({
        operation,
        context: { ...(key ? { key } : {}), reason: "stale_binding" },
      }),
    );
  }
}

function storeStatusFromObjectStoreStatus(
  status: ObjectStoreStatus,
  options: StoreOpenOptions,
): StoreStatus {
  return {
    size: status.size,
    sealed: status.sealed,
    ttlMs: status.ttl > 0 ? Math.floor(status.ttl / 1_000_000) : 0,
    ...(options.maxObjectBytes !== undefined
      ? { maxObjectBytes: options.maxObjectBytes }
      : {}),
    ...(status.streamInfo.config.max_bytes > 0
      ? { maxTotalBytes: status.streamInfo.config.max_bytes }
      : {}),
  };
}

export function bytesFromStoreStream(
  stream: ReadableStream<Uint8Array>,
): Promise<Uint8Array> {
  return bytesFromStream(stream);
}
