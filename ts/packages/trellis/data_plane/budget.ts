import {
  transferConstants,
  transferReceiveWindow,
} from "../auth/protocol_wasm.ts";

/**
 * Receive capacity shared by every transfer of one process. A receiver
 * reserves the exact window it offers before frames can arrive and releases
 * it when the transfer ends; admission never overcommits. @internal
 */
export class ReceiveBudget {
  #reserved = 0;

  constructor(readonly total: number) {}

  /** Window `[frames, bytes]` to offer now, or `undefined` when exhausted. */
  offer(maxFrameBytes: number): [number, number] | undefined {
    return transferReceiveWindow(
      Math.max(0, this.total - this.#reserved),
      maxFrameBytes,
    );
  }

  /** Reserve exactly `bytes`; returns its idempotent release, or `undefined`. */
  reserve(bytes: number): (() => void) | undefined {
    if (this.#reserved + bytes > this.total) return undefined;
    this.#reserved += bytes;
    let released = false;
    return () => {
      if (released) return;
      released = true;
      this.#reserved -= bytes;
    };
  }
}

let process: ReceiveBudget | undefined;

/** The process-wide receive budget. @internal */
export function processReceiveBudget(): ReceiveBudget {
  process ??= new ReceiveBudget(transferConstants().processReceiveBytes);
  return process;
}
