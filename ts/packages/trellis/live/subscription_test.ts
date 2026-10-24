import assert from "node:assert/strict";
import { test } from "node:test";

import { LiveSessionManager } from "./manager.ts";
import { ConsumerCore, LiveSubscription } from "./subscription.ts";
import {
  LiveCancellation,
  type LiveCloseReceipt,
  LiveEnd,
  LiveStreamError,
} from "./types.ts";

function noopClose(): Promise<LiveCloseReceipt> {
  return Promise.resolve({
    end: new LiveEnd("cancelled"),
    remote: "not-required",
    cleanup: "unknown",
  });
}

test("NX07 TS abnormal end discards queued items", () => {
  const core = new ConsumerCore<number>("session", "standalone");
  assert.deepEqual(core.admit({ value: 1, encodedLen: 1 }), true);
  core.commitEnd(
    new LiveEnd(
      "authorization_lost",
      new LiveStreamError("revoked", "revoked"),
    ),
  );
  core.discardQueue();
  assert.deepEqual(core.hasQueued(), false);
  assert.deepEqual(core.consumedSeq(), 0n);
});

test("G09 a filtered frame cannot advance the prefix past an unread value", () => {
  const core = new ConsumerCore<number>("session", "standalone");
  assert.deepEqual(core.admit({ value: 1, encodedLen: 10 }), true);
  assert.deepEqual(core.releaseFiltered(), true);
  assert.deepEqual(core.consumedSeq(), 0n);
  assert.deepEqual(core.consume()?.value, 1);
  assert.deepEqual(core.consumedSeq(), 2n);
  assert.deepEqual(core.consume(), undefined);
  assert.deepEqual(core.queuedBytes(), 0);
});

test("G10 final handoff commits complete and resolves closed", async () => {
  const core = new ConsumerCore<number>("session", "standalone");
  const sub = new LiveSubscription(core, new LiveCancellation(), noopClose);
  core.setPhase("draining");
  assert.deepEqual(core.admit({ value: 4, encodedLen: 3 }), true);
  core.setPendingEnd(new LiveEnd("complete"));
  const iterator = sub[Symbol.asyncIterator]();
  const first = await iterator.next();
  assert.deepEqual(first.done, false);
  // No further next() call: the final handoff alone must commit and close.
  const closed = await sub.closed;
  assert.deepEqual(closed.reason, "complete");
});

test("long streams preserve exact frame and byte accounting", () => {
  const core = new ConsumerCore<number>("session", "standalone");
  for (let index = 1; index <= 70_001; index++) {
    assert.deepEqual(core.admit({ value: index, encodedLen: 8 }), true);
    if (index % 10 === 1) assert.deepEqual(core.releaseFiltered(), true);
    assert.deepEqual(core.consume()?.value, index);
  }
  assert.deepEqual(core.consume(), undefined);
  // Every admitted frame plus every filtered marker is accounted exactly once.
  assert.deepEqual(core.receivedSeq(), BigInt(70_001 + 7001));
  assert.deepEqual(core.consumedSeq(), BigInt(70_001 + 7001));
  assert.deepEqual(core.queuedBytes(), 0);
});

test("an active complete end drains queued items then completes", async () => {
  const core = new ConsumerCore<number>("session", "standalone");
  const sub = new LiveSubscription(core, new LiveCancellation(), noopClose);
  core.setPhase("draining");
  assert.deepEqual(core.admit({ value: 9, encodedLen: 1 }), true);
  core.setPendingEnd(new LiveEnd("complete"));
  const seen: number[] = [];
  for await (const value of sub) seen.push(value);
  assert.deepEqual(sub.activated, true);
  assert.deepEqual(seen, [9]);
});

test("activation releases an admitted value", async () => {
  const core = new ConsumerCore<number>("session", "standalone");
  const sub = new LiveSubscription(core, new LiveCancellation(), noopClose);
  assert.deepEqual(core.admit({ value: 7, encodedLen: 1 }), true);
  const iterator = sub[Symbol.asyncIterator]();
  const pending = iterator.next();
  core.setPhase("active");
  const first = await pending;
  assert.deepEqual(first.done, false);
  assert.deepEqual(first.value, 7);
});

test("consumer permit releases on dispose", () => {
  const manager = new LiveSessionManager();
  const first = manager.admitConsumer();
  const second = manager.admitConsumer();
  assert.deepEqual(manager.consumerCount(), 2);
  first[Symbol.dispose]();
  assert.deepEqual(manager.consumerCount(), 1);
  second[Symbol.dispose]();
  assert.deepEqual(manager.consumerCount(), 0);
});
