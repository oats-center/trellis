import type {
  LiveEndReasonWire,
  LiveErrorCodeWire,
} from "../auth/protocol_wasm.ts";

/** Bounded live failure envelope. */
export class LiveStreamError extends Error {
  readonly code: LiveErrorCodeWire;
  readonly traceId: string | undefined;

  constructor(
    code: LiveErrorCodeWire,
    message: string,
    traceId?: string,
  ) {
    super(message);
    this.name = "LiveStreamError";
    this.code = code;
    this.traceId = traceId;
  }

  codeString(): string {
    return `trellis.live.${this.code}`;
  }
}

/** Committed terminal outcome for one live observation. */
export class LiveEnd {
  readonly reason: LiveEndReasonWire;
  readonly error: LiveStreamError | undefined;

  constructor(reason: LiveEndReasonWire, error?: LiveStreamError) {
    this.reason = reason;
    this.error = error;
  }

  isComplete(): boolean {
    return this.reason === "complete";
  }
}

/** Bounded receipt describing the outcome of an explicit close. */
export type LiveCloseReceipt = {
  readonly end: LiveEnd;
  readonly remote: "confirmed" | "unconfirmed" | "not-required";
  readonly cleanup: "complete" | "incomplete" | "unknown";
};

/** Cloneable cancellation for one live source scope. */
export class LiveCancellation {
  #aborted = false;
  readonly #waiters: Array<() => void> = [];
  readonly signal: AbortSignal;

  constructor() {
    const controller = new AbortController();
    this.signal = controller.signal;
    this.#waiters.push(() => controller.abort());
  }

  get aborted(): boolean {
    return this.#aborted;
  }

  cancel(): void {
    if (this.#aborted) return;
    this.#aborted = true;
    for (const waiter of this.#waiters) waiter();
    this.#waiters.length = 0;
  }

  cancelled(): Promise<void> {
    if (this.#aborted) return Promise.resolve();
    return new Promise((resolve) => this.#waiters.push(resolve));
  }
}
