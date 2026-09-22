/**
 * Generation guard for asynchronous console reads and disposal.
 *
 * A scope belongs to one page read. Its generation advances whenever the
 * semantic target/filter key changes, a newer request starts, or the owner is
 * invalidated/disposed. A token from `begin()` is current only while the scope
 * is alive and no newer request has superseded it, so a late response from an
 * old target can never commit over a newer one.
 *
 * Ownership is semantic target and component lifetime. An advisory
 * `Sessions.Me` refresh does not invalidate a read; Trellis authorizes it.
 */

/** Opaque proof that a read still belongs to the scope's current generation. */
export type RequestToken = {
  readonly generation: number;
};

/** Pure generation guard; owns no data and performs no reads. */
export class RequestScope {
  #key: string;
  #generation = 0;
  #disposed = false;
  #pending = false;

  constructor(key: string) {
    this.#key = key;
  }

  /** Current semantic target/filter key the page is reading. */
  get key(): string {
    return this.#key;
  }

  /** True once `dispose()` ran; no token can be current afterwards. */
  get disposed(): boolean {
    return this.#disposed;
  }

  /** True while the most recent `begin()` token has not settled. */
  get pending(): boolean {
    return this.#pending;
  }

  /**
   * Replaces the semantic key. Returns true when it actually changed, so the
   * caller can clear data that belonged to the previous target.
   */
  setKey(key: string): boolean {
    if (key === this.#key) return false;
    this.#key = key;
    this.#generation += 1;
    return true;
  }

  /** Starts a new request generation, superseding any outstanding token. */
  begin(): RequestToken {
    this.#generation += 1;
    this.#pending = true;
    return { generation: this.#generation };
  }

  /** True only while the token belongs to the live scope generation. */
  isCurrent(token: RequestToken): boolean {
    return !this.#disposed && token.generation === this.#generation;
  }

  /**
   * Marks the token's work as finished. Later tokens cannot be affected, so
   * callers clear loading state only when this returns true.
   */
  settle(token: RequestToken): boolean {
    if (!this.isCurrent(token)) return false;
    this.#pending = false;
    return true;
  }

  /** Invalidates outstanding tokens without ending the scope's life. */
  invalidate(): void {
    this.#generation += 1;
    this.#pending = false;
  }

  /** Ends the scope permanently; outstanding tokens are no longer current. */
  dispose(): void {
    this.#disposed = true;
    this.#generation += 1;
    this.#pending = false;
  }
}
