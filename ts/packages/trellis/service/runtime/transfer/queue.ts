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
      this.#values.push(value);
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

export class AsyncChunkQueue implements AsyncIterable<Uint8Array> {
  #values: Array<{ chunk: Uint8Array; consumed: () => void }> = [];
  #pending: Array<{
    resolve: (result: IteratorResult<Uint8Array>) => void;
    reject: (error: unknown) => void;
  }> = [];
  #closed = false;
  #error: unknown;

  async push(chunk: Uint8Array): Promise<void> {
    if (this.#closed) return;
    const pending = this.#pending.shift();
    if (pending) {
      pending.resolve({ value: chunk, done: false });
      return;
    }
    await new Promise<void>((consumed) => {
      this.#values.push({ chunk, consumed });
    });
  }

  close(): void {
    if (this.#closed) return;
    this.#closed = true;
    for (const pending of this.#pending.splice(0)) {
      pending.resolve({ value: undefined, done: true });
    }
  }

  fail(error: unknown): void {
    if (this.#closed) return;
    this.#error = error;
    this.#closed = true;
    for (const value of this.#values.splice(0)) value.consumed();
    for (const pending of this.#pending.splice(0)) pending.reject(error);
  }

  async next(): Promise<IteratorResult<Uint8Array>> {
    const value = this.#values.shift();
    if (value) {
      value.consumed();
      return { value: value.chunk, done: false };
    }
    if (this.#error) throw this.#error;
    if (this.#closed) return { value: undefined, done: true };
    return await new Promise<IteratorResult<Uint8Array>>((resolve, reject) => {
      this.#pending.push({ resolve, reject });
    });
  }

  [Symbol.asyncIterator](): AsyncIterator<Uint8Array> {
    return { next: () => this.next() };
  }
}
