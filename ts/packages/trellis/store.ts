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
} from "@qlever-llc/result";
import { StoreError } from "./errors/index.ts";
import {
  decodePaginationCursor,
  encodePaginationCursor,
  paginationQueryDigest,
} from "./auth/protocol_wasm.ts";
import { TypedStoreEntry } from "./store_entry.ts";
export { TypedStoreEntry } from "./store_entry.ts";

const INTERNAL_CONTENT_TYPE_METADATA_KEY = "__trellis_content_type";
const DEFAULT_STORE_WAIT_POLL_INTERVAL_MS = 250;
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
};

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

function isNotFoundStoreError(error: StoreError): boolean {
  return error.getContext().reason === "not_found";
}

function abortedStoreError(key: string, cause: unknown): StoreError {
  return new StoreError({
    operation: "waitFor",
    cause,
    context: { key, reason: "aborted" },
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
  if (actualTtlMs !== (options.ttlMs ?? 0)) {
    throw new Error(`Store '${name}' TTL does not match its binding`);
  }
}

export class TypedStore {
  readonly #store: ObjectStore;
  readonly #options:
    & Required<Pick<StoreOpenOptions, "ttlMs">>
    & Omit<StoreOpenOptions, "ttlMs">;

  private constructor(store: ObjectStore, options: StoreOpenOptions) {
    this.#store = store;
    this.#options = {
      ttlMs: options.ttlMs ?? 0,
      ...(options.maxObjectBytes !== undefined
        ? { maxObjectBytes: options.maxObjectBytes }
        : {}),
      ...(options.maxTotalBytes !== undefined
        ? { maxTotalBytes: options.maxTotalBytes }
        : {}),
      ...(options.bindOnly !== undefined ? { bindOnly: options.bindOnly } : {}),
    };
  }

  static open(
    nats: NatsConnection,
    name: string,
    options: StoreOpenOptions = {},
  ): AsyncResult<TypedStore, StoreError> {
    return AsyncResult.from((async () => {
      try {
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
        return Result.ok(new TypedStore(store, options));
      } catch (cause) {
        return Result.err(
          new StoreError({ operation: "open", cause, context: { name } }),
        );
      }
    })());
  }

  create(
    key: string,
    body: StoreBody,
    options?: StorePutOptions,
  ): AsyncResult<void, StoreError> {
    return AsyncResult.from(this.#putInternal("create", key, body, options, 0));
  }

  put(
    key: string,
    body: StoreBody,
    options?: StorePutOptions,
  ): AsyncResult<void, StoreError> {
    return AsyncResult.from(this.#putInternal("put", key, body, options));
  }

  get(key: string): AsyncResult<TypedStoreEntry, StoreError> {
    return AsyncResult.from((async () => {
      if (!this.#isCurrent()) return this.#stale("get", key);
      const info = await unwrapObjectInfo(this.#store, key);
      return info.map((objectInfo) =>
        new TypedStoreEntry(this.#store, storeInfoFromObjectInfo(objectInfo))
      );
    })());
  }

  /**
   * Waits for an object key to appear in the store and returns the resulting entry.
   */
  waitFor(
    key: string,
    options: StoreWaitOptions = {},
  ): AsyncResult<TypedStoreEntry, StoreError> {
    return AsyncResult.from((async () => {
      const startedAt = Date.now();
      const pollIntervalMs = options.pollIntervalMs ??
        DEFAULT_STORE_WAIT_POLL_INTERVAL_MS;

      while (true) {
        if (options.signal?.aborted) {
          return Result.err(abortedStoreError(key, options.signal.reason));
        }

        const entry = await this.get(key);
        if (entry.isOk()) {
          return entry;
        }
        if (!isNotFoundStoreError(entry.error)) {
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
          return Result.err(abortedStoreError(key, cause));
        }
      }
    })());
  }

  delete(key: string): AsyncResult<void, StoreError> {
    return AsyncResult.from((async () => {
      if (!this.#isCurrent()) return this.#stale("delete", key);
      try {
        await this.#store.delete(key);
        return Result.ok(undefined);
      } catch (cause) {
        return Result.err(
          new StoreError({ operation: "delete", cause, context: { key } }),
        );
      }
    })());
  }

  list(
    opts: StoreListOptions = {},
  ): AsyncResult<StoreListPage, StoreError> {
    return AsyncResult.from((async () => {
      if (!this.#isCurrent()) return this.#stale("list");
      const query = validateStoreListOptions(opts);
      if (query.isErr()) return Result.err(query.error);

      const { prefix, cursor, limit } = query.unwrapOrElse(() => {
        throw new Error("unreachable");
      });
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
        const objects = await this.#store.list();
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
      }
    })());
  }

  status(): AsyncResult<StoreStatus, StoreError> {
    return AsyncResult.from((async () => {
      if (!this.#isCurrent()) return this.#stale("status");
      try {
        const status = await this.#store.status();
        return Result.ok(
          storeStatusFromObjectStoreStatus(status, this.#options),
        );
      } catch (cause) {
        return Result.err(new StoreError({ operation: "status", cause }));
      }
    })());
  }

  async #putInternal(
    operation: "create" | "put",
    key: string,
    body: StoreBody,
    options?: StorePutOptions,
    previousRevision?: number,
  ): Promise<ResultType<void, StoreError>> {
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

        await this.#store.putBlob(
          { name: key, ...(metadata ? { metadata } : {}) },
          body,
          previousRevision === undefined ? undefined : { previousRevision },
        );
        return Result.ok(undefined);
      }

      const limitedStream = enforceMaxObjectBytes(
        streamFromBody(body),
        this.#options.maxObjectBytes,
      );

      await this.#store.put(
        { name: key, ...(metadata ? { metadata } : {}) },
        limitedStream,
        previousRevision === undefined ? undefined : { previousRevision },
      );
      return Result.ok(undefined);
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
  options:
    & Required<Pick<StoreOpenOptions, "ttlMs">>
    & Omit<StoreOpenOptions, "ttlMs">,
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
