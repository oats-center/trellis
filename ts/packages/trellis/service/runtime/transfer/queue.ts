export function deferred<T>(): {
  promise: Promise<T>;
  resolve(value: T): void;
} {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

class AsyncValueQueue<T> implements AsyncIterable<T> {
  #values: T[] = [];
  #resolvers: Array<(result: IteratorResult<T>) => void> = [];
  #closed = false;

  push(value: T): void {
    if (this.#closed) return;
    const resolver = this.#resolvers.shift();
    if (resolver) {
      resolver({ value, done: false });
    } else {
      // Progress is a latest-value observation, not an unbounded event log.
      this.#values = [value];
    }
  }

  close(): void {
    if (this.#closed) return;
    this.#closed = true;
    for (const resolver of this.#resolvers.splice(0)) {
      resolver({ value: undefined as T, done: true });
    }
  }

  [Symbol.asyncIterator](): AsyncIterator<T> {
    return {
      next: async (): Promise<IteratorResult<T>> => {
        const value = this.#values.shift();
        if (value !== undefined) return { value, done: false };
        if (this.#closed) return { value: undefined as T, done: true };
        return await new Promise<IteratorResult<T>>((resolve) => {
          this.#resolvers.push(resolve);
        });
      },
    };
  }
}

export class AsyncValueBroadcaster<T> {
  #subscribers = new Set<AsyncValueQueue<T>>();
  #closed = false;

  push(value: T): void {
    if (this.#closed) return;
    for (const subscriber of this.#subscribers) subscriber.push(value);
  }

  close(): void {
    if (this.#closed) return;
    this.#closed = true;
    for (const subscriber of this.#subscribers) subscriber.close();
    this.#subscribers.clear();
  }

  subscribe(): AsyncIterable<T> {
    const subscriber = new AsyncValueQueue<T>();
    if (this.#closed) {
      subscriber.close();
    } else {
      this.#subscribers.add(subscriber);
    }
    const subscribers = this.#subscribers;
    return {
      async *[Symbol.asyncIterator]() {
        try {
          yield* subscriber;
        } finally {
          subscribers.delete(subscriber);
        }
      },
    };
  }
}

/** Frame-aware ingress. Credit follows ownership handoff to the storage reader. */
export class TransferIngress implements AsyncIterable<Uint8Array> {
  #values: Array<{ seq: bigint; chunk: Uint8Array }> = [];
  #bytes = 0;
  #received = 0n;
  #closed = false;
  #error: unknown;
  #wake: (() => void) | undefined;

  constructor(
    readonly maxFrameBytes: number,
    readonly windowFrames: number,
    readonly windowBytes: number,
    readonly consumed: (seq: bigint, bytes: number) => void,
    readonly direction: "upload" | "download" = "upload",
  ) {}

  /** Bytes retained by the downstream ingress, excluding the verification lane. */
  get bufferedBytes(): number {
    return this.#bytes;
  }

  /** Whole frames retained by the downstream ingress. */
  get bufferedFrames(): number {
    return this.#values.length;
  }

  /** Admit an authenticated contiguous frame without blocking the wire reader. */
  push(seq: bigint, chunk: Uint8Array): void {
    if (this.#closed) throw new Error("transfer ingress is closed");
    if (seq !== this.#received + 1n) throw new Error("transfer sequence gap");
    if (
      chunk.length === 0 || chunk.length > this.maxFrameBytes ||
      this.#values.length >= this.windowFrames ||
      this.#bytes + chunk.length > this.windowBytes
    ) throw new Error("transfer receive window exceeded");
    this.#received = seq;
    this.#values.push({ seq, chunk });
    this.#bytes += chunk.length;
    recordCatalogUpDown("trellis.transfer.buffered.bytes", chunk.length, {
      "trellis.direction": this.direction,
    });
    this.#wake?.();
  }

  /** Close only after the authenticated completion declared every admitted frame. */
  close(finalSeq: bigint): void {
    if (finalSeq !== this.#received) throw new Error("transfer completion gap");
    this.#closed = true;
    this.#wake?.();
  }

  /** Abort queued and pending backend reads; aborted bytes never earn credit. */
  fail(error: unknown): void {
    this.#error = error;
    this.#closed = true;
    this.#values = [];
    recordCatalogUpDown("trellis.transfer.buffered.bytes", -this.#bytes, {
      "trellis.direction": this.direction,
    });
    this.#bytes = 0;
    this.#wake?.();
  }

  async *[Symbol.asyncIterator](): AsyncIterator<Uint8Array> {
    while (true) {
      if (this.#error !== undefined) throw this.#error;
      const value = this.#values.shift();
      if (value) {
        this.#bytes -= value.chunk.length;
        recordCatalogUpDown(
          "trellis.transfer.buffered.bytes",
          -value.chunk.length,
          { "trellis.direction": this.direction },
        );
        this.consumed(value.seq, value.chunk.length);
        yield value.chunk;
      } else if (this.#closed) {
        return;
      } else {
        await new Promise<void>((resolve) => this.#wake = resolve);
        this.#wake = undefined;
      }
    }
  }
}
import { recordCatalogUpDown } from "../../../telemetry/metrics.ts";
