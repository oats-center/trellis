// Monotonic deadline owner shared by the live provider and consumer drivers.
//
// This is the single source of truth for live-session timing policy in the
// TypeScript SDK. It is a pure reducer over a monotonically increasing
// millisecond value, so the same production state machine runs under the
// production clock or a manual test clock. Durations come from the shared Rust
// protocol constants (through the WASM bridge), never from TS literals.
//
// There is no total ACTIVE age limit. The reservation deadline runs only from
// allocation to activation. An ACTIVE session uses independent peer,
// consumption and (provider) challenge deadlines; a healthy idle session never
// expires.

import { liveConstants } from "../auth/protocol_wasm.ts";

const C = liveConstants();

/** Live-session phase tracked by the deadline owner. */
export type LiveDeadlinePhase =
  | "offered"
  | "prepared"
  | "activating"
  | "active"
  | "draining"
  | "closing"
  | "closed";

/** Typed action returned by {@link LiveDeadlines.evaluate}. */
export type DeadlineAction =
  | "reservation_expired"
  | "challenge_due"
  | "challenge_retry"
  | "peer_inactive"
  | "consumer_stalled"
  | "credit_due"
  | "close_exchange_elapsed"
  | "cleanup_grace_elapsed";

/**
 * Internal monotonic clock dependency.
 *
 * Ordinary connection constructors always select {@link productionLiveClock};
 * test modules may inject a manual clock. It is never a user-facing option.
 */
export interface LiveClock {
  nowMs(): number;
  scheduleAt(deadlineMs: number, run: () => void): () => void;
}

/** Production clock: monotonic `performance.now()` and rechecked `setTimeout`. */
export const productionLiveClock: LiveClock = {
  nowMs: () => performance.now(),
  scheduleAt(deadlineMs, run) {
    const handle = setTimeout(run, Math.max(0, deadlineMs - performance.now()));
    return () => clearTimeout(handle);
  },
};

/** Production live-session deadline state. */
export class LiveDeadlines {
  #phase: LiveDeadlinePhase;
  #reservationUntil: number | undefined;
  #lastFreshRoundTripAt: number | undefined;
  #nextChallengeAt: number | undefined;
  #challengeRetryAt: number | undefined;
  #outstandingChallenge: string | undefined;
  #unconsumedSince: number | undefined;
  #lastConsumptionAt: number | undefined;
  #nextCreditAt: number | undefined;
  #closeUntil: number | undefined;
  #cleanupUntil: number | undefined;
  #tombstoneUntil: number | undefined;
  #generation = 0;

  private constructor(nowMs: number, phase: LiveDeadlinePhase) {
    this.#phase = phase;
    this.#reservationUntil = nowMs + C.openReservationMs;
  }

  /** Create one offered provider reservation with an absolute deadline. */
  static reserved(nowMs: number): LiveDeadlines {
    return new LiveDeadlines(nowMs, "offered");
  }

  /** Create one prepared consumer handle with an absolute deadline. */
  static prepared(nowMs: number): LiveDeadlines {
    return new LiveDeadlines(nowMs, "prepared");
  }

  /**
   * Create one prepared consumer handle from an already-started reservation.
   *
   * The consumer's opening budget begins before the opening exchange, so the
   * pump installs the same absolute deadline rather than restarting it.
   */
  static preparedUntil(deadlineMs: number): LiveDeadlines {
    const deadlines = new LiveDeadlines(deadlineMs, "prepared");
    deadlines.#reservationUntil = deadlineMs;
    return deadlines;
  }

  get phase(): LiveDeadlinePhase {
    return this.#phase;
  }

  get generation(): number {
    return this.#generation;
  }

  outstandingChallenge(): string | undefined {
    return this.#outstandingChallenge;
  }

  /** Enter ACTIVATING and issue one challenge to retry until answered. */
  beginActivating(nowMs: number, challengeId: string): void {
    this.#phase = "activating";
    this.#outstandingChallenge = challengeId;
    this.#challengeRetryAt = nowMs + C.challengeRetryMs;
  }

  /** Commit ACTIVE on the first valid fresh round trip. */
  commitActive(nowMs: number, provider: boolean): void {
    this.#phase = "active";
    this.#reservationUntil = undefined;
    this.#lastFreshRoundTripAt = nowMs;
    this.#outstandingChallenge = undefined;
    this.#challengeRetryAt = undefined;
    this.#nextChallengeAt = provider
      ? nowMs + C.heartbeatIntervalMs
      : undefined;
  }

  /** Record one fresh challenge answer / matching acknowledgement. */
  freshRoundTrip(nowMs: number, provider: boolean): void {
    this.#lastFreshRoundTripAt = nowMs;
    this.#outstandingChallenge = undefined;
    this.#challengeRetryAt = undefined;
    if (provider) {
      this.#nextChallengeAt = nowMs + C.heartbeatIntervalMs;
    }
  }

  /** Install one new challenge nonce and arm its retry. */
  beginChallenge(nowMs: number, challengeId: string): void {
    this.#outstandingChallenge = challengeId;
    this.#challengeRetryAt = nowMs + C.challengeRetryMs;
    this.#nextChallengeAt = undefined;
  }

  /** Re-arm the retry for the same outstanding challenge. */
  rearmChallengeRetry(nowMs: number): void {
    if (this.#outstandingChallenge !== undefined) {
      this.#challengeRetryAt = nowMs + C.challengeRetryMs;
    }
  }

  /** Record that application data became outstanding after an empty period. */
  dataAdmitted(nowMs: number): void {
    if (this.#unconsumedSince === undefined) {
      this.#unconsumedSince = nowMs;
    }
  }

  /** Record real consumption progress and arm the credit schedule. */
  noteConsumption(nowMs: number, newlyConsumed: number): void {
    this.#lastConsumptionAt = nowMs;
    if (newlyConsumed >= C.ackFrameThreshold) {
      this.#nextCreditAt = nowMs;
    } else if (this.#nextCreditAt === undefined) {
      this.#nextCreditAt = nowMs + C.ackMaxDelayMs;
    }
  }

  /** Reset the consumption-stall clock without scheduling consumer credit. */
  noteStallReset(nowMs: number): void {
    this.#lastConsumptionAt = nowMs;
  }

  /** Disable the consumption deadline once nothing is outstanding. */
  outstandingCleared(): void {
    this.#unconsumedSince = undefined;
  }

  /** Record that accumulated credit was handed off. */
  creditSent(): void {
    this.#nextCreditAt = undefined;
  }

  /** Consume the cleanup-grace deadline once it fired. */
  markCleanupGraceElapsed(): void {
    this.#cleanupUntil = undefined;
  }

  /** Enter DRAINING after a verified normal end with queued items. */
  beginDraining(): void {
    this.#phase = "draining";
    this.#reservationUntil = undefined;
    this.#nextChallengeAt = undefined;
    this.#challengeRetryAt = undefined;
    this.#outstandingChallenge = undefined;
  }

  /** Enter CLOSING with one absolute close exchange and one cleanup grace. */
  beginClosing(nowMs: number): void {
    this.#phase = "closing";
    this.#nextCreditAt = undefined;
    this.#nextChallengeAt = undefined;
    this.#challengeRetryAt = undefined;
    this.#closeUntil = nowMs + C.closeExchangeMs;
    this.#cleanupUntil = nowMs + C.cleanupGraceMs;
  }

  /** Enter CLOSED, bump the generation and retain only the receipt window. */
  closed(nowMs: number): void {
    this.#phase = "closed";
    this.#reservationUntil = undefined;
    this.#lastFreshRoundTripAt = undefined;
    this.#nextChallengeAt = undefined;
    this.#challengeRetryAt = undefined;
    this.#outstandingChallenge = undefined;
    this.#unconsumedSince = undefined;
    this.#nextCreditAt = undefined;
    this.#closeUntil = undefined;
    this.#cleanupUntil = undefined;
    this.#tombstoneUntil = nowMs + C.tombstoneMs;
    this.#generation += 1;
  }

  /** Return whether the bounded receipt has expired. */
  tombstoneExpired(nowMs: number): boolean {
    return this.#tombstoneUntil !== undefined && nowMs >= this.#tombstoneUntil;
  }

  /** Return the earliest pending deadline, if any. */
  nextDue(): number | undefined {
    let due: number | undefined;
    const consider = (candidate: number | undefined) => {
      if (candidate === undefined) return;
      due = due === undefined ? candidate : Math.min(due, candidate);
    };
    switch (this.#phase) {
      case "offered":
      case "prepared":
      case "activating":
        consider(this.#reservationUntil);
        consider(this.#challengeRetryAt);
        break;
      case "active":
        consider(this.#challengeRetryAt);
        consider(this.#nextChallengeAt);
        consider(this.#peerDeadline());
        consider(this.#consumptionDeadline());
        consider(this.#nextCreditAt);
        break;
      case "draining":
        consider(this.#consumptionDeadline());
        break;
      case "closing":
        consider(this.#closeUntil);
        consider(this.#cleanupUntil);
        break;
      case "closed":
        break;
    }
    return due;
  }

  /** Evaluate the next due action at `nowMs`. */
  evaluate(nowMs: number): DeadlineAction | undefined {
    switch (this.#phase) {
      case "offered":
      case "prepared":
      case "activating":
        if (due(this.#reservationUntil, nowMs)) return "reservation_expired";
        if (due(this.#challengeRetryAt, nowMs)) return "challenge_retry";
        break;
      case "active":
        if (due(this.#peerDeadline(), nowMs)) return "peer_inactive";
        if (due(this.#consumptionDeadline(), nowMs)) return "consumer_stalled";
        if (due(this.#challengeRetryAt, nowMs)) return "challenge_retry";
        if (due(this.#nextChallengeAt, nowMs)) return "challenge_due";
        if (due(this.#nextCreditAt, nowMs)) return "credit_due";
        break;
      case "draining":
        if (due(this.#consumptionDeadline(), nowMs)) return "consumer_stalled";
        break;
      case "closing":
        if (due(this.#closeUntil, nowMs)) return "close_exchange_elapsed";
        if (due(this.#cleanupUntil, nowMs)) return "cleanup_grace_elapsed";
        break;
      case "closed":
        break;
    }
    return undefined;
  }

  #peerDeadline(): number | undefined {
    return this.#lastFreshRoundTripAt === undefined
      ? undefined
      : this.#lastFreshRoundTripAt + C.peerInactivityMs;
  }

  #consumptionDeadline(): number | undefined {
    if (this.#unconsumedSince === undefined) return undefined;
    const last = this.#lastConsumptionAt ?? this.#unconsumedSince;
    const start = Math.max(this.#unconsumedSince, last);
    return start + C.consumerStallMs;
  }
}

function due(deadlineMs: number | undefined, nowMs: number): boolean {
  return deadlineMs !== undefined && nowMs >= deadlineMs;
}

/**
 * One owned timer that invokes `onDue` at the next scheduled deadline.
 *
 * `arm` replaces any pending schedule; `dispose` invalidates queued callbacks
 * by generation and clears the timer. A callback that wakes early re-arms
 * itself until the real monotonic deadline is reached.
 */
export class LiveTimer {
  readonly #clock: LiveClock;
  readonly #onDue: () => void;
  #clear: (() => void) | undefined;
  #generation = 0;

  constructor(clock: LiveClock, onDue: () => void) {
    this.#clock = clock;
    this.#onDue = onDue;
  }

  /** Schedule the next wake for `deadlineMs`, replacing any pending one. */
  arm(deadlineMs: number | undefined): void {
    this.#clear?.();
    this.#clear = undefined;
    if (deadlineMs === undefined) return;
    const generation = this.#generation;
    this.#clear = this.#clock.scheduleAt(deadlineMs, () => {
      if (generation !== this.#generation) return;
      if (this.#clock.nowMs() < deadlineMs) {
        this.arm(deadlineMs);
        return;
      }
      this.#onDue();
    });
  }

  /** Invalidate queued callbacks and clear the timer. */
  dispose(): void {
    this.#generation += 1;
    this.#clear?.();
    this.#clear = undefined;
  }
}
