/** Internal negotiated frame size and outstanding capacity. */
export type FlowLimits = {
  maxFrameBytes: number;
  windowFrames: number;
  windowBytes: number;
};

/** Accounting failures mapped by the owning protocol, not wire error codes. */
export type FlowError =
  | "payload_too_large"
  | "window_full"
  | "invalid_cursor"
  | "sequence_exhausted";
const MAX_SEQUENCE = (1n << 64n) - 1n;

/** Bounded sender ledger; the owner serializes validate/handoff/commit. */
export class SenderWindow {
  readonly #limits: FlowLimits;
  readonly #outstanding: { seq: bigint; bytes: number }[] = [];
  #outstandingBytes = 0;
  #highestSent = 0n;
  #highestReceived = 0n;
  #highestConsumed = 0n;
  #consumedBytes = 0n;
  #waiter: (() => void) | undefined;

  /** Create an empty window with protocol-owned limits. */
  constructor(limits: FlowLimits) {
    this.#limits = { ...limits };
  }
  /** Last successfully handed-off sequence. */
  get highestSent(): bigint {
    return this.#highestSent;
  }
  /** Last authenticated received cursor. */
  get highestReceived(): bigint {
    return this.#highestReceived;
  }
  /** Last authenticated consumed cursor. */
  get highestConsumed(): bigint {
    return this.#highestConsumed;
  }
  /** Next sequence without advancing the watermark; undefined on u64 exhaustion. */
  nextFrameSeq(): bigint | undefined {
    return this.#highestSent < MAX_SEQUENCE
      ? this.#highestSent + 1n
      : undefined;
  }
  /** Validate capacity without consuming it. Hold the output lane through commit. */
  validateFrameSlot(
    bytes: number,
    maxFrameBytes = this.#limits.maxFrameBytes,
  ): FlowError | undefined {
    if (
      !Number.isSafeInteger(bytes) || bytes < 0 ||
      bytes > Math.min(maxFrameBytes, this.#limits.maxFrameBytes)
    ) return "payload_too_large";
    if (
      this.#outstanding.length >= this.#limits.windowFrames ||
      bytes > this.#limits.windowBytes - this.#outstandingBytes
    ) return "window_full";
    return undefined;
  }
  /** Record successful handoff; never call this after a failed publication. */
  commitFrame(seq: bigint, bytes: number): FlowError | undefined {
    const error = this.validateFrameSlot(bytes);
    if (error) return error;
    const next = this.nextFrameSeq();
    if (next === undefined) return "sequence_exhausted";
    if (seq !== next) return "invalid_cursor";
    this.#outstanding.push({ seq, bytes });
    this.#outstandingBytes += bytes;
    this.#highestSent = seq;
    return undefined;
  }
  /**
   * Reject impossible/regressing cursors and, when supplied, an inexact
   * cumulative consumed byte total before mutation. Live omits the byte total.
   */
  validateCredit(
    received: bigint,
    consumed: bigint,
    consumedBytes?: bigint,
  ): FlowError | undefined {
    if (
      consumed > received || received > this.#highestSent ||
      consumed < this.#highestConsumed || received < this.#highestReceived
    ) return "invalid_cursor";
    if (consumedBytes !== undefined) {
      let expected = this.#consumedBytes;
      for (const frame of this.#outstanding) {
        if (frame.seq > consumed) break;
        expected += BigInt(frame.bytes);
      }
      if (expected !== consumedBytes) return "invalid_cursor";
    }
    return undefined;
  }
  /** Apply consumed credit and wake the blocked sender; received alone frees nothing. */
  applyCredit(
    received: bigint,
    consumed: bigint,
    consumedBytes?: bigint,
  ): FlowError | undefined {
    const error = this.validateCredit(received, consumed, consumedBytes);
    if (error) return error;
    while (this.#outstanding.length && this.#outstanding[0].seq <= consumed) {
      const bytes = this.#outstanding.shift()!.bytes;
      this.#outstandingBytes -= bytes;
      this.#consumedBytes += BigInt(bytes);
    }
    this.#highestReceived = received;
    this.#highestConsumed = consumed;
    this.wakeCredit();
    return undefined;
  }
  /** Whether every handed-off frame has been consumed. */
  outstandingEmpty(): boolean {
    return this.#outstanding.length === 0;
  }
  /** Reject incomplete consumed delivery or a missing declared sequence. */
  validateComplete(finalSeq: bigint): FlowError | undefined {
    return finalSeq === this.#highestSent &&
        finalSeq === this.#highestConsumed && this.outstandingEmpty()
      ? undefined
      : "invalid_cursor";
  }
  /** Register one capacity waiter; the session owns cancellation via wakeCredit. */
  waitCredit(): Promise<void> {
    return new Promise((resolve) => {
      this.#waiter = resolve;
    });
  }
  /** Wake on released capacity or lifecycle cancellation. */
  wakeCredit(): void {
    const waiter = this.#waiter;
    this.#waiter = undefined;
    waiter?.();
  }
}
