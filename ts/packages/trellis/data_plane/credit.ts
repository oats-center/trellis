/** Internal policy; undefined byteStep disables the byte threshold for Live. */
export type CreditPolicy = {
  frameStep: number;
  byteStep?: number;
  maxDelayMs: number;
};

/** One pending credit deadline; the endpoint owns its latest cumulative cursor. */
export class CreditScheduler {
  readonly #policy: CreditPolicy;
  #deadline: number | undefined;
  /** Use the owning protocol's unchanged scheduling policy. */
  constructor(policy: CreditPolicy) {
    this.#policy = { ...policy };
  }
  /** Schedule from cumulative unreported counts, not per-call consumption deltas. */
  notePending(nowMs: number, frames: number, bytes: number): void {
    if (
      frames >= this.#policy.frameStep ||
      (this.#policy.byteStep !== undefined && bytes >= this.#policy.byteStep)
    ) this.force(nowMs);
    else this.#deadline ??= nowMs + this.#policy.maxDelayMs;
  }
  /** Force terminal/context-renewal credit without postponing an earlier deadline. */
  force(nowMs: number): void {
    this.#deadline = Math.min(this.#deadline ?? nowMs, nowMs);
  }
  /** Earliest deadline for the owner's timer. */
  nextDue(): number | undefined {
    return this.#deadline;
  }
  /** Whether latest cumulative credit should be handed off now. */
  isDue(nowMs: number): boolean {
    return this.#deadline !== undefined && nowMs >= this.#deadline;
  }
  /** Clear after handoff or teardown; in-flight coalescing stays endpoint-owned. */
  clear(): void {
    this.#deadline = undefined;
  }
}
