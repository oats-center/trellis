import assert from "node:assert/strict";
import { test } from "node:test";
import { CreditScheduler } from "./credit.ts";

test("credit coalesces without postponing and thresholds force delivery", () => {
  const scheduler = new CreditScheduler({
    frameStep: 8,
    byteStep: 100,
    maxDelayMs: 25,
  });
  scheduler.notePending(0, 1, 10);
  scheduler.notePending(20, 2, 20);
  assert.deepEqual(scheduler.isDue(24), false);
  assert.deepEqual(scheduler.isDue(25), true);
  scheduler.clear();
  scheduler.notePending(30, 8, 80);
  assert.deepEqual(scheduler.isDue(30), true);
  scheduler.clear();
  scheduler.notePending(40, 1, 100);
  assert.deepEqual(scheduler.isDue(40), true);
  scheduler.clear();
  scheduler.notePending(50, 1, 10);
  scheduler.force(50);
  assert.deepEqual(scheduler.isDue(50), true);
});
