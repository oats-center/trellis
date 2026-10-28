import assert from "node:assert/strict";
import { test } from "node:test";

import { type LiveClock, LiveDeadlines, LiveTimer } from "./deadlines.ts";
import { liveConstants } from "../auth/protocol_wasm.ts";

const C = liveConstants();

/** Deterministic monotonic clock with a bounded ordered callback set. */
class ManualLiveClock implements LiveClock {
  #now = 0;
  #order = 0;
  #entries: {
    deadline: number;
    order: number;
    run: () => void;
    cancelled: boolean;
  }[] = [];

  nowMs(): number {
    return this.#now;
  }

  scheduleAt(deadlineMs: number, run: () => void): () => void {
    const entry = {
      deadline: deadlineMs,
      order: this.#order++,
      run,
      cancelled: false,
    };
    this.#entries.push(entry);
    return () => {
      entry.cancelled = true;
    };
  }

  /** Execute every due callback in (deadline, insertion-order) order. */
  advanceTo(nowMs: number): void {
    this.#now = nowMs;
    let guard = 0;
    while (true) {
      const due = this.#entries
        .filter((entry) => !entry.cancelled && entry.deadline <= this.#now)
        .sort((a, b) => a.deadline - b.deadline || a.order - b.order);
      const next = due[0];
      if (!next) return;
      next.cancelled = true;
      next.run();
      if (++guard > 10_000) {
        throw new Error("manual clock scheduler did not settle");
      }
    }
  }
}

test("VT01 reservation expires exactly at the deadline", () => {
  const d = LiveDeadlines.reserved(0);
  assert.deepEqual(d.evaluate(C.openReservationMs - 1), undefined);
  assert.deepEqual(d.evaluate(C.openReservationMs), "reservation_expired");
});

test("VT02 activation before the deadline survives past it", () => {
  const d = LiveDeadlines.reserved(0);
  d.beginActivating(1_000, "challenge");
  d.commitActive(1_000, false);
  assert.deepEqual(d.evaluate(C.openReservationMs + 1), undefined);
  assert.deepEqual(d.phase, "active");
});

test("VT03 a healthy active session has no age cap", () => {
  const d = LiveDeadlines.reserved(0);
  d.beginActivating(0, "c");
  d.commitActive(0, true);
  let now = 0;
  let generation = 0;
  while (now < 24 * 60 * 60 * 1_000) {
    const action = d.evaluate(now);
    if (action === "challenge_due") {
      d.beginChallenge(now, `challenge-${generation++}`);
      d.freshRoundTrip(now, true);
    } else if (action === "challenge_retry") {
      d.freshRoundTrip(now, true);
    } else {
      assert.deepEqual(action, undefined);
    }
    now += 1_000;
  }
  assert.deepEqual(d.phase, "active");
});

test("VT04 peer inactivity is exact and not postponed by data", () => {
  const d = LiveDeadlines.reserved(0);
  d.beginActivating(0, "c");
  d.commitActive(100, false);
  d.dataAdmitted(200);
  assert.deepEqual(d.evaluate(100 + C.peerInactivityMs - 1), undefined);
  assert.deepEqual(d.evaluate(100 + C.peerInactivityMs), "peer_inactive");
});

test("VT05 one unanswered challenge retries and never renews", () => {
  const d = LiveDeadlines.reserved(0);
  d.beginActivating(0, "nonce");
  assert.deepEqual(d.outstandingChallenge(), "nonce");
  assert.deepEqual(d.evaluate(C.challengeRetryMs), "challenge_retry");
  d.rearmChallengeRetry(C.challengeRetryMs);
  assert.deepEqual(d.outstandingChallenge(), "nonce");
});

test("VT06 consumption stall starts at first unconsumed data", () => {
  const d = LiveDeadlines.reserved(0);
  d.beginActivating(0, "c");
  d.commitActive(0, false);
  d.dataAdmitted(5_000);
  d.freshRoundTrip(30_000, false);
  assert.deepEqual(d.evaluate(5_000 + C.consumerStallMs - 1), undefined);
  assert.deepEqual(d.evaluate(5_000 + C.consumerStallMs), "consumer_stalled");
});

test("VT07 consumption progress resets and empty disables the stall", () => {
  const d = LiveDeadlines.reserved(0);
  d.beginActivating(0, "c");
  d.commitActive(0, false);
  d.dataAdmitted(0);
  d.noteStallReset(30_000);
  d.freshRoundTrip(35_000, false);
  assert.deepEqual(d.evaluate(30_000 + C.consumerStallMs - 1), undefined);
  assert.deepEqual(d.evaluate(30_000 + C.consumerStallMs), "consumer_stalled");
  d.outstandingCleared();
  assert.deepEqual(d.evaluate(40_000), undefined);
});

test("VT08 credit is due after the frame threshold or the delay", () => {
  const d = LiveDeadlines.reserved(0);
  d.beginActivating(0, "c");
  d.commitActive(0, false);
  d.noteConsumption(0, C.ackFrameThreshold);
  assert.deepEqual(d.evaluate(0), "credit_due");
  d.creditSent();
  assert.deepEqual(d.evaluate(0), undefined);
  d.noteConsumption(100, 1);
  assert.deepEqual(d.evaluate(100 + C.ackMaxDelayMs), "credit_due");
});

test("VT12 the bounded receipt expires and never reactivates", () => {
  const d = LiveDeadlines.reserved(0);
  d.closed(0);
  assert.deepEqual(d.tombstoneExpired(C.tombstoneMs - 1), false);
  assert.deepEqual(d.tombstoneExpired(C.tombstoneMs), true);
  assert.deepEqual(d.phase, "closed");
});

test("VT-timer the production timer adapter fires the same transitions", () => {
  const clock = new ManualLiveClock();
  const d = LiveDeadlines.reserved(clock.nowMs());
  let fired = 0;
  const timer = new LiveTimer(clock, () => {
    fired += 1;
    if (d.evaluate(clock.nowMs()) === "reservation_expired") {
      d.closed(clock.nowMs());
      timer.dispose();
      return;
    }
    timer.arm(d.nextDue());
  });
  timer.arm(d.nextDue());
  clock.advanceTo(C.openReservationMs - 1);
  assert.deepEqual(fired, 0);
  clock.advanceTo(C.openReservationMs);
  assert.deepEqual(fired, 1);
  assert.deepEqual(d.phase, "closed");
  clock.advanceTo(C.openReservationMs + C.tombstoneMs);
  assert.deepEqual(fired, 1);
});

test("VT-timer dispose invalidates queued callbacks", () => {
  const clock = new ManualLiveClock();
  const d = LiveDeadlines.reserved(clock.nowMs());
  let fired = 0;
  const timer = new LiveTimer(clock, () => {
    fired += 1;
  });
  timer.arm(d.nextDue());
  timer.dispose();
  clock.advanceTo(C.openReservationMs);
  assert.deepEqual(fired, 0);
});

test("T02 two distinct post-activation challenges are acknowledged in order", () => {
  const d = LiveDeadlines.reserved(0);
  d.beginActivating(0, "activation");
  d.commitActive(0, true);
  const nonces: string[] = [];
  let now = 0;
  // Walk logical time until two separate heartbeat challenges have been issued
  // and answered; each carries a distinct nonce identity.
  while (nonces.length < 2 && now < C.heartbeatIntervalMs * 4 + 1_000) {
    if (d.evaluate(now) === "challenge_due") {
      const nonce = `challenge-${nonces.length}`;
      d.beginChallenge(now, nonce);
      const outstanding = d.outstandingChallenge();
      assert.deepEqual(
        outstanding,
        nonce,
        "the outstanding challenge identity",
      );
      nonces.push(outstanding!);
      d.freshRoundTrip(now, true);
      assert.deepEqual(
        d.outstandingChallenge(),
        undefined,
        "answering clears the outstanding challenge",
      );
    }
    now += 1_000;
  }
  assert.deepEqual(nonces, ["challenge-0", "challenge-1"]);
  assert.deepEqual(d.phase, "active");
});
