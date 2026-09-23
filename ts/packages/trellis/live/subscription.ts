// Consumer-side live subscription core and public owned handle.

import { AsyncResult, UnexpectedError } from "@oatscenter/result";
import { liveConstants } from "../auth/protocol_wasm.ts";
import {
  LiveCancellation,
  type LiveCloseReceipt,
  LiveEnd,
  LiveStreamError,
} from "./types.ts";
import { type LiveTelemetryKind, LiveTelemetryOwner } from "./telemetry.ts";

const C = liveConstants();

/** One verified and admitted application item. */
export type Admitted<T> = { value: T; encodedLen: number };

/** One ordered receive slot: an application value or a filtered marker. */
type Slot<T> = { kind: "value"; item: Admitted<T> } | { kind: "filtered" };

/** Consumer-visible session phase. */
export type ConsumerPhase =
  | "prepared"
  | "activating"
  | "active"
  | "draining"
  | "closing"
  | "closed";

/** Local usage error for an unsupported iterator call. */
export class LiveUsageError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "LiveUsageError";
  }
}

/** Bounded receipt describing the outcome of an explicit close. */
export type { LiveCloseReceipt };

/**
 * Queue and cursors for one consumer session.
 *
 * The queue is the single bounded ingress; count and encoded bytes stay within
 * the negotiated window. Cursors are monotonic `bigint` values, never JS
 * numbers, so they cannot wrap at 65,535.
 */
export class ConsumerCore<T> {
  readonly sessionId: string;
  readonly start = Promise.withResolvers<void>();
  readonly #started = Promise.withResolvers<void>();
  #slots: Slot<T>[] = [];
  #queuedBytes = 0;
  #end: LiveEnd | undefined;
  #pendingEnd: LiveEnd | undefined;
  #consumed = 0n;
  #received = 0n;
  #phase: ConsumerPhase = "prepared";
  #cancelled = false;
  #errorReported = false;
  #waiter: (() => void) | undefined;
  #creditWaiter: (() => void) | undefined;
  readonly #endWaiters: Array<(end: LiveEnd) => void> = [];
  readonly #telemetry: LiveTelemetryOwner;

  constructor(sessionId: string, kind: LiveTelemetryKind) {
    this.sessionId = sessionId;
    this.#telemetry = new LiveTelemetryOwner(kind, "consumer");
  }

  /** This endpoint's single telemetry owner. */
  get telemetry(): LiveTelemetryOwner {
    return this.#telemetry;
  }

  /** Resolve the internal start gate; the first `next()` calls this. */
  notifyStart(): void {
    if (this.#startedResolved) return;
    this.#startedResolved = true;
    this.#started.resolve();
    this.start.resolve();
  }

  #startedResolved = false;

  /** Wait until the first iteration installs the activation path. */
  waitStart(): Promise<void> {
    return this.#started.promise;
  }

  get phase(): ConsumerPhase {
    return this.#phase;
  }

  setPhase(phase: ConsumerPhase): void {
    if (this.#phase === phase) return;
    this.#phase = phase;
    switch (phase) {
      case "prepared":
        this.#telemetry.prepared();
        break;
      case "activating":
        this.#telemetry.activating();
        break;
      case "active":
        this.#telemetry.active();
        break;
      case "draining":
        this.#telemetry.draining();
        break;
      case "closing":
        this.#telemetry.closing();
        break;
      case "closed":
        break;
    }
    this.wake();
  }

  get cancelled(): boolean {
    return this.#cancelled;
  }

  /** Admit one verified application item within the wire window. */
  admit(item: Admitted<T>): boolean {
    if (this.#slots.length + 1 > C.windowFrames) return false;
    if (this.#queuedBytes + item.encodedLen > C.windowBytes) return false;
    this.#slots.push({ kind: "value", item });
    this.#queuedBytes += item.encodedLen;
    this.#received += 1n;
    this.#telemetry.buffered(item.encodedLen);
    this.wake();
    this.wakeCredit();
    return true;
  }

  /**
   * Record one verified frame deliberately filtered from the application.
   *
   * The frame keeps its ordered slot, so the consumed prefix cannot advance
   * past an earlier unread value. Returns `false` when the bounded slot window
   * cannot admit the marker.
   */
  releaseFiltered(): boolean {
    if (this.#slots.length + 1 > C.windowFrames) return false;
    this.#slots.push({ kind: "filtered" });
    this.#received += 1n;
    this.wakeCredit();
    this.wake();
    return true;
  }

  hasQueued(): boolean {
    return this.#slots.length > 0;
  }

  /**
   * Take the next application item, advancing the contiguous consumed prefix
   * across any filtered slots in front of it.
   */
  consume(): Admitted<T> | undefined {
    while (true) {
      const slot = this.#slots.shift();
      if (!slot) return undefined;
      if (slot.kind === "filtered") {
        this.#consumed += 1n;
        this.wakeCredit();
        continue;
      }
      this.#queuedBytes -= slot.item.encodedLen;
      this.#telemetry.buffered(-slot.item.encodedLen);
      this.#consumed += 1n;
      // Filtered markers queued behind this value need no application
      // handoff, so the contiguous consumed prefix may advance across them
      // immediately (G09: value 1 + filtered 2 reports consumed 2).
      while (this.#slots[0]?.kind === "filtered") {
        this.#slots.shift();
        this.#consumed += 1n;
      }
      this.wakeCredit();
      return slot.item;
    }
  }

  consumedSeq(): bigint {
    return this.#consumed;
  }

  receivedSeq(): bigint {
    return this.#received;
  }

  queuedBytes(): number {
    return this.#queuedBytes;
  }

  /** Record one verified remote normal end, pending until the queue drains. */
  setPendingEnd(end: LiveEnd): void {
    this.#pendingEnd ??= end;
    this.wake();
  }

  /** Register a one-shot terminal observer, called immediately if closed. */
  onEnd(callback: (end: LiveEnd) => void): void {
    const existing = this.#end;
    if (existing) {
      callback(existing);
      return;
    }
    this.#endWaiters.push(callback);
  }

  /** Commit one terminal outcome once; later calls are ignored. */
  commitEnd(end: LiveEnd): void {
    if (this.#end) return;
    this.#end = end;
    this.#phase = "closed";
    this.#telemetry.end(end);
    this.#pendingEnd = undefined;
    this.wake();
    this.start.resolve();
    this.#started.resolve();
    const waiters = this.#endWaiters.splice(0);
    for (const waiter of waiters) waiter(end);
  }

  committedEnd(): LiveEnd | undefined {
    return this.#end;
  }

  pendingEnd(): LiveEnd | undefined {
    return this.#pendingEnd;
  }

  discardQueue(): void {
    this.#slots = [];
    if (this.#queuedBytes > 0) {
      this.#telemetry.buffered(-this.#queuedBytes);
    }
    this.#queuedBytes = 0;
  }

  /** Actual local cleanup completed; remove the session and pending gauge. */
  cleanupFinished(): void {
    this.#telemetry.cleanupFinished();
  }

  /** Retained local cleanup exceeded the shared grace. */
  cleanupExceededGrace(): void {
    this.#telemetry.cleanupExceededGrace();
  }

  /** Resolve a pending normal end once the queue is empty. */
  drainComplete(): boolean {
    if (this.#end?.isComplete() !== true) return false;
    return this.#slots.length === 0;
  }

  /** Promote a pending normal end to the committed outcome once drained. */
  commitDrainIfComplete(): void {
    if (this.#pendingEnd && this.#slots.length === 0) {
      const pending = this.#pendingEnd;
      this.#pendingEnd = undefined;
      this.commitEnd(pending);
    }
  }

  cancel(): void {
    this.#cancelled = true;
    this.wake();
    this.start.resolve();
    this.#started.resolve();
  }

  reportedError(): boolean {
    return this.#errorReported;
  }

  markErrorReported(): void {
    this.#errorReported = true;
  }

  /** Wake the application's pending poll, if any. */
  wake(): void {
    const waiter = this.#waiter;
    this.#waiter = undefined;
    waiter?.();
  }

  /** Register the single pending application waiter. */
  setWaiter(waiter: () => void): void {
    this.#waiter = waiter;
  }

  /** Wake the control pump when consumption advances. */
  wakeCredit(): void {
    const waiter = this.#creditWaiter;
    this.#creditWaiter = undefined;
    waiter?.();
  }

  /** Wait for the next consumption advance. */
  waitCredit(): Promise<void> {
    return new Promise((resolve) => {
      this.#creditWaiter = resolve;
    });
  }
}

/**
 * Public owned live subscription.
 *
 * Implements the single-consumer iterator contract: `[Symbol.asyncIterator]`
 * returns this, one `next()` may be pending, `closed` resolves once and never
 * rejects, and `close()` returns a bounded receipt.
 */
export class LiveSubscription<T> implements AsyncIterableIterator<T> {
  readonly #core: ConsumerCore<T>;
  readonly #cancellation: LiveCancellation;
  readonly #closeFn: () => Promise<LiveCloseReceipt>;
  readonly #fence: (() => LiveEnd | undefined) | undefined;
  readonly #closed = Promise.withResolvers<LiveEnd>();
  #closedResolved = false;
  #nextPending = false;
  #activated = false;
  #permit: { [Symbol.dispose](): void } | undefined;
  #closePromise: Promise<LiveCloseReceipt> | undefined;

  constructor(
    core: ConsumerCore<T>,
    cancellation: LiveCancellation,
    closeFn: () => Promise<LiveCloseReceipt>,
    permit?: { [Symbol.dispose](): void },
    fence?: () => LiveEnd | undefined,
  ) {
    this.#core = core;
    this.#cancellation = cancellation;
    this.#closeFn = closeFn;
    this.#permit = permit;
    this.#fence = fence;
    this.#core.onEnd((end) => this.#resolveClosed(end));
  }

  #resolveClosed(end: LiveEnd, releasePermit = true): void {
    if (this.#closedResolved) return;
    this.#closedResolved = true;
    if (releasePermit) this.#releasePermit();
    this.#closed.resolve(end);
  }

  #releasePermit(): void {
    this.#permit?.[Symbol.dispose]();
    this.#permit = undefined;
  }

  /** The terminal outcome; resolves once and never rejects. */
  get closed(): Promise<LiveEnd> {
    return this.#closed.promise;
  }

  get activated(): boolean {
    return this.#activated;
  }

  [Symbol.asyncIterator](): AsyncIterableIterator<T> {
    return this;
  }

  async next(): Promise<IteratorResult<T>> {
    if (this.#nextPending) {
      throw new LiveUsageError("concurrent next() is not supported");
    }
    this.#nextPending = true;
    try {
      if (!this.#activated) {
        this.#activated = true;
        this.#core.notifyStart();
      }
      while (true) {
        if (this.#cancellation.aborted || this.#core.cancelled) {
          this.#core.discardQueue();
          this.#core.cancel();
          return { done: true, value: undefined };
        }
        // A continuing current-authority check fences queued data before it is
        // handed to the application.
        const fenced = this.#fence?.();
        if (fenced) {
          this.#core.discardQueue();
          this.#core.commitEnd(fenced);
        }
        const end = this.#core.committedEnd();
        if (end && !end.isComplete()) {
          this.#core.discardQueue();
          if (end.error && !this.#core.reportedError()) {
            this.#core.markErrorReported();
            throw end.error;
          }
          return { done: true, value: undefined };
        }
        if (this.#activePhase()) {
          const item = this.#core.consume();
          if (item) {
            // Handing out the last queued value commits a pending normal end
            // immediately; the application need not poll once more.
            this.#core.commitDrainIfComplete();
            return { done: false, value: item.value };
          }
        }
        this.#core.commitDrainIfComplete();
        if (this.#core.drainComplete()) {
          return { done: true, value: undefined };
        }
        await new Promise<void>((resolve) => {
          this.#core.setWaiter(resolve);
          if (
            (this.#activePhase() && this.#core.hasQueued()) ||
            this.#core.committedEnd()
          ) {
            resolve();
          }
        });
      }
    } finally {
      this.#nextPending = false;
    }
  }

  #activePhase(): boolean {
    const phase = this.#core.phase;
    return phase === "active" || phase === "draining";
  }

  /** Run the one bounded close exchange, shared by every close path. */
  #runClose(): Promise<LiveCloseReceipt> {
    this.#closePromise ??= this.#closeFn();
    return this.#closePromise;
  }

  /** Synchronously fence this session; no new yields or publications. */
  fence(): void {
    this.#cancel();
  }

  #cancel(): void {
    this.#cancellation.cancel();
    this.#core.discardQueue();
    this.#core.cancel();
    // A pending normal end is not a successful committed result once queued
    // data is discarded; the permit stays until the close exchange settles.
    // The local terminal is committed into the core so the public `closed`
    // outcome, the iteration result and the telemetry owner share one source
    // of truth (a prepared cancellation must still record one live end).
    const end = this.#core.committedEnd() ?? new LiveEnd("cancelled");
    this.#core.commitEnd(end);
    this.#resolveClosed(end, false);
  }

  async #settleClose(): Promise<LiveCloseReceipt> {
    try {
      return await this.#runClose();
    } finally {
      this.#releasePermit();
    }
  }

  /** Cancellation-safe iterator return; never hangs behind a pending next. */
  async return(value?: unknown): Promise<IteratorResult<T>> {
    this.#cancel();
    try {
      await this.#settleClose();
    } catch {
      // Remote confirmation is best-effort and already bounded.
    }
    return { done: true, value: value as T };
  }

  async throw(error?: unknown): Promise<IteratorResult<T>> {
    return await this.return(error);
  }

  /**
   * Explicitly close the observation.
   *
   * Returns the bounded close receipt directly; use `.orThrow()` or await the
   * result. This is intentionally not an `async` wrapper.
   */
  close(): AsyncResult<LiveCloseReceipt, UnexpectedError> {
    this.#cancel();
    return AsyncResult.try(() => this.#settleClose());
  }

  async [Symbol.asyncDispose](): Promise<void> {
    await this.#settleClose().catch(() => {
      // Disposal is best-effort; the bounded close exchange already ran.
    });
  }
}

/** Build one abnormal consumer terminal outcome. */
export function consumerFailure(
  code: string,
  message: string,
): LiveEnd {
  const reason = code === "consumer_slow"
    ? "consumer_slow"
    : code === "delivery_gap"
    ? "delivery_gap"
    : code === "authorization_unavailable"
    ? "authorization_lost"
    : code === "permission_denied" || code === "authorization_revoked" ||
        code === "authorization_expired"
    ? "authorization_lost"
    : code === "setup_timeout"
    ? "setup_timeout"
    : code === "peer_lost"
    ? "peer_lost"
    : code === "disconnected"
    ? "disconnected"
    : "protocol_error";
  return new LiveEnd(reason, new LiveStreamError(code, message));
}
