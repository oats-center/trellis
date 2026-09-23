import { assertEquals } from "@std/assert";

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

Deno.test("NX07 TS abnormal end discards queued items", () => {
  const core = new ConsumerCore<number>("session", "standalone");
  assertEquals(core.admit({ value: 1, encodedLen: 1 }), true);
  core.commitEnd(
    new LiveEnd(
      "authorization_lost",
      new LiveStreamError("revoked", "revoked"),
    ),
  );
  core.discardQueue();
  assertEquals(core.hasQueued(), false);
  assertEquals(core.consumedSeq(), 0n);
});

Deno.test("G09 a filtered frame cannot advance the prefix past an unread value", () => {
  const core = new ConsumerCore<number>("session", "standalone");
  assertEquals(core.admit({ value: 1, encodedLen: 10 }), true);
  assertEquals(core.releaseFiltered(), true);
  assertEquals(core.consumedSeq(), 0n);
  assertEquals(core.consume()?.value, 1);
  assertEquals(core.consumedSeq(), 2n);
  assertEquals(core.consume(), undefined);
  assertEquals(core.queuedBytes(), 0);
});

Deno.test("G10 final handoff commits complete and resolves closed", async () => {
  const core = new ConsumerCore<number>("session", "standalone");
  const sub = new LiveSubscription(
    core,
    new LiveCancellation(),
    noopClose,
  );
  core.setPhase("draining");
  assertEquals(core.admit({ value: 4, encodedLen: 3 }), true);
  core.setPendingEnd(new LiveEnd("complete"));
  const iterator = sub[Symbol.asyncIterator]();
  const first = await iterator.next();
  assertEquals(first.done, false);
  // No further next() call: the final handoff alone must commit and close.
  const closed = await sub.closed;
  assertEquals(closed.reason, "complete");
  assertEquals(core.committedEnd()?.reason, "complete");
});

Deno.test("T01 70,001 production transitions cross the old count boundary", () => {
  const core = new ConsumerCore<number>("session", "standalone");
  for (let index = 1; index <= 70_001; index++) {
    assertEquals(core.admit({ value: index, encodedLen: 8 }), true);
    if (index % 10 === 1) assertEquals(core.releaseFiltered(), true);
    assertEquals(core.consume()?.value, index);
  }
  assertEquals(core.consume(), undefined);
  // Every admitted frame plus every filtered marker is accounted exactly once.
  assertEquals(core.receivedSeq(), BigInt(70_001 + 7001));
  assertEquals(core.consumedSeq(), BigInt(70_001 + 7001));
  assertEquals(core.queuedBytes(), 0);
});

Deno.test("an active complete end drains queued items then completes", async () => {
  const core = new ConsumerCore<number>("session", "standalone");
  const sub = new LiveSubscription(
    core,
    new LiveCancellation(),
    noopClose,
  );
  assertEquals(sub.activated, false);
  core.setPhase("draining");
  assertEquals(core.admit({ value: 9, encodedLen: 1 }), true);
  core.setPendingEnd(new LiveEnd("complete"));
  const seen: number[] = [];
  for await (const value of sub) seen.push(value);
  assertEquals(sub.activated, true);
  assertEquals(seen, [9]);
});

Deno.test("prepared handle does not yield before activation", async () => {
  const core = new ConsumerCore<number>("session", "standalone");
  const sub = new LiveSubscription(
    core,
    new LiveCancellation(),
    noopClose,
  );
  assertEquals(core.admit({ value: 7, encodedLen: 1 }), true);
  const iterator = sub[Symbol.asyncIterator]();
  const pending = iterator.next();
  core.setPhase("active");
  const first = await pending;
  assertEquals(first.done, false);
  assertEquals(first.value, 7);
});

Deno.test("consumer permit releases on dispose", () => {
  const manager = new LiveSessionManager();
  const first = manager.admitConsumer();
  const second = manager.admitConsumer();
  assertEquals(manager.consumerCount(), 2);
  first[Symbol.dispose]();
  assertEquals(manager.consumerCount(), 1);
  second[Symbol.dispose]();
  assertEquals(manager.consumerCount(), 0);
});
