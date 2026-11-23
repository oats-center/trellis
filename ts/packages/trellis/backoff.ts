/**
 * The waits between attempts of work that keeps failing: `firstMs` after the
 * first failure, doubling with each further failure up to `longestMs`, and
 * back to `firstMs` once the work succeeds.
 *
 * Retry loops in the SDK share this so they all wait the same way (see
 * `design/core/engineering-patterns.md`). It is internal to the package.
 */
export class Backoff {
  readonly #firstMs: number;
  readonly #longestMs: number;
  #failures = 0;

  /** Waits that start at `firstMs` and double up to `longestMs`. */
  constructor(firstMs: number, longestMs: number) {
    this.#firstMs = firstMs;
    this.#longestMs = longestMs;
  }

  /** The wait the next failure returns, in milliseconds, without counting one. */
  get nextDelayMs(): number {
    return Math.min(
      this.#firstMs * 2 ** Math.min(this.#failures, 31),
      this.#longestMs,
    );
  }

  /** Counts a failure and returns how long to wait, in milliseconds, before trying again. */
  failed(): number {
    const delay = this.nextDelayMs;
    this.#failures += 1;
    return delay;
  }

  /** Starts the waits over after the work succeeded. */
  succeeded(): void {
    this.#failures = 0;
  }
}
