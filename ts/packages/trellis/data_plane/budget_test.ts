import assert from "node:assert/strict";
import { test } from "node:test";
import { transferConstants } from "../auth/protocol_wasm.ts";
import { ReceiveBudget } from "./budget.ts";

test("receive admission shrinks offers, rejects overcommit and releases once", () => {
  const C = transferConstants();
  const budget = new ReceiveBudget(C.windowBytes * 2 + 1024);
  const frame = 256 * 1024;
  assert.deepEqual(budget.offer(frame), [C.windowFrames, C.windowBytes]);
  const first = budget.reserve(C.windowBytes);
  const second = budget.reserve(C.windowBytes);
  assert(first && second);
  // 1 KiB remains: smaller than a frame, so nothing is offered or reserved.
  assert.equal(budget.offer(frame), undefined);
  assert.equal(budget.reserve(C.windowBytes), undefined);
  first();
  first();
  assert.deepEqual(budget.offer(frame), [C.windowFrames, C.windowBytes]);
  second();
  const half = new ReceiveBudget(C.windowBytes / 2);
  assert.deepEqual(half.offer(64 * 1024), [
    Math.ceil(C.windowFrames / 2),
    C.windowBytes / 2,
  ]);
});
