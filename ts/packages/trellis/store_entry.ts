import { AsyncResult, Result } from "@oatscenter/result";
import { StoreError } from "./errors/index.ts";
import type { StoreInfo } from "./store.ts";

type ObjectStoreLike = {
  get(key: string): Promise<ReadableStream<Uint8Array> | null>;
};

/** A logical object entry whose reads acquire and retain their own transport. */
export class TypedStoreEntry {
  /** Logical object key used to acquire a fresh read. */
  readonly key: string;
  /** Metadata observed when obtained; subsequent reads validate fresh metadata. */
  readonly info: StoreInfo;
  readonly #store: ObjectStoreLike;

  /** Construct an entry backed by demand-driven, integrity-checked reads. */
  constructor(store: ObjectStoreLike, info: StoreInfo) {
    this.#store = store;
    this.key = info.key;
    this.info = info;
  }

  /** Read with bounded demand-driven pulls; successful EOF verifies size and digest. */
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

        return Result.ok(result);
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

  /** Collect the same verified stream, intentionally retaining the whole object. */
  bytes(): AsyncResult<Uint8Array, StoreError> {
    return AsyncResult.from((async () => {
      try {
        const stream = await this.#store.get(this.key);
        if (stream === null) {
          return Result.err(
            new StoreError({
              operation: "bytes",
              context: { key: this.key, reason: "not_found" },
            }),
          );
        }
        return Result.ok(
          new Uint8Array(await new Response(stream).arrayBuffer()),
        );
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
