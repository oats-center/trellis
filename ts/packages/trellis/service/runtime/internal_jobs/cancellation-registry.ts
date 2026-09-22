import type { JobCancellationToken } from "./active-job.ts";

export class ActiveJobCancellationGuard {
  readonly #key: string;
  readonly #token: JobCancellationToken;
  readonly #registry: ActiveJobCancellationRegistry;
  #disposed = false;

  constructor(
    key: string,
    token: JobCancellationToken,
    registry: ActiveJobCancellationRegistry,
  ) {
    this.#key = key;
    this.#token = token;
    this.#registry = registry;
  }

  dispose(): void {
    if (this.#disposed) {
      return;
    }
    this.#disposed = true;
    this.#registry.unregister(this.#key, this.#token);
  }
}

export class ActiveJobCancellationRegistry {
  readonly #tokens = new Map<string, Set<JobCancellationToken>>();
  readonly #pending = new Set<string>();

  register(
    key: string,
    token: JobCancellationToken,
  ): ActiveJobCancellationGuard {
    const tokens = this.#tokens.get(key) ?? new Set<JobCancellationToken>();
    tokens.add(token);
    this.#tokens.set(key, tokens);
    if (this.#pending.delete(key)) {
      token.cancel();
    }
    return new ActiveJobCancellationGuard(key, token, this);
  }

  cancel(key: string): boolean {
    const tokens = this.#tokens.get(key);
    if (!tokens || tokens.size === 0) {
      this.#pending.add(key);
      return false;
    }
    for (const token of tokens) {
      token.cancel();
    }
    return true;
  }

  clearPending(key: string): void {
    this.#pending.delete(key);
  }

  unregister(key: string, token: JobCancellationToken): void {
    const tokens = this.#tokens.get(key);
    if (!tokens) {
      return;
    }
    tokens.delete(token);
    if (tokens.size === 0) {
      this.#tokens.delete(key);
    }
  }
}
