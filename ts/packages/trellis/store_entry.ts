import { AsyncResult, Result } from "@qlever-llc/result";
import { StoreError } from "./errors/index.ts";
import type { StoreInfo } from "./store.ts";

type ObjectResultLike = {
  data: ReadableStream<Uint8Array>;
  error: Promise<unknown>;
};

type ObjectStoreLike = {
  get(key: string): Promise<ObjectResultLike | null>;
  getBlob(key: string): Promise<Uint8Array | null>;
};

export class TypedStoreEntry {
  readonly key: string;
  readonly info: StoreInfo;
  readonly #store: ObjectStoreLike;

  constructor(store: ObjectStoreLike, info: StoreInfo) {
    this.#store = store;
    this.key = info.key;
    this.info = info;
  }

  stream(): AsyncResult<ReadableStream<Uint8Array>, StoreError> {
    return AsyncResult.from((async () => {
      try {
        const result = await this.#store.get(this.key);
        if (result === null) {
          return Result.err(
            new StoreError({
              operation: "stream",
              context: { key: this.key, reason: "not_found" },
            }),
          );
        }

        return Result.ok(streamWithErrorCheck(result));
      } catch (cause) {
        return Result.err(
          new StoreError({
            operation: "stream",
            cause,
            context: { key: this.key },
          }),
        );
      }
    })());
  }

  bytes(): AsyncResult<Uint8Array, StoreError> {
    return AsyncResult.from((async () => {
      try {
        const bytes = await this.#store.getBlob(this.key);
        if (bytes === null) {
          return Result.err(
            new StoreError({
              operation: "bytes",
              context: { key: this.key, reason: "not_found" },
            }),
          );
        }
        return Result.ok(bytes);
      } catch (cause) {
        return Result.err(
          new StoreError({
            operation: "bytes",
            cause,
            context: { key: this.key },
          }),
        );
      }
    })());
  }
}

function streamWithErrorCheck(
  result: ObjectResultLike,
): ReadableStream<Uint8Array> {
  const reader = result.data.getReader();

  return new ReadableStream<Uint8Array>({
    async pull(controller) {
      const next = await reader.read();
      if (next.done) {
        const error = await result.error;
        if (error) {
          controller.error(error);
          return;
        }
        controller.close();
        return;
      }

      controller.enqueue(next.value);
    },
    async cancel(reason) {
      await reader.cancel(reason);
    },
  });
}
