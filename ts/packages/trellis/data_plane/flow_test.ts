import assert from "node:assert/strict";
import { test } from "node:test";
import { SenderWindow } from "./flow.ts";

test("byte credit must match consumed prefix before capacity release", () => {
  const window = new SenderWindow({
    maxFrameBytes: 5,
    windowFrames: 2,
    windowBytes: 8,
  });
  window.commitFrame(1n, 3);
  window.commitFrame(2n, 5);
  for (const bytes of [0n, 2n, 4n, 8n]) {
    assert.deepEqual(window.validateCredit(2n, 1n, bytes), "invalid_cursor");
    assert.deepEqual(window.applyCredit(2n, 1n, bytes), "invalid_cursor");
    assert.deepEqual(window.validateFrameSlot(1), "window_full");
  }
  assert.deepEqual(window.applyCredit(2n, 1n, 3n), undefined);
  assert.deepEqual(window.commitFrame(3n, 3), undefined);
  assert.deepEqual(window.applyCredit(3n, 2n, 5n), "invalid_cursor");
  assert.deepEqual(window.validateFrameSlot(1), "window_full");
  assert.deepEqual(window.applyCredit(3n, 2n, 8n), undefined);
  assert.deepEqual(window.applyCredit(3n, 2n, 8n), undefined);
  assert.deepEqual(window.applyCredit(3n, 2n, 9n), "invalid_cursor");
  assert.deepEqual(window.applyCredit(3n, 3n, 11n), undefined);
  assert.deepEqual(window.validateComplete(3n), undefined);
});

test("consumed credit releases bounded capacity and wakes sender", async () => {
  const window = new SenderWindow({
    maxFrameBytes: 6,
    windowFrames: 2,
    windowBytes: 8,
  });
  assert.deepEqual(window.commitFrame(1n, 3), undefined);
  assert.deepEqual(window.commitFrame(2n, 5), undefined);
  assert.deepEqual(window.validateFrameSlot(1), "window_full");
  assert.deepEqual(window.applyCredit(2n, 0n), undefined);
  assert.deepEqual(window.validateFrameSlot(1), "window_full");
  const notified = window.waitCredit();
  assert.deepEqual(window.applyCredit(2n, 1n), undefined);
  await notified;
  assert.deepEqual(window.commitFrame(3n, 3), undefined);
  assert.deepEqual(window.validateFrameSlot(1), "window_full");
  assert.deepEqual(window.applyCredit(1n, 1n), "invalid_cursor");
  assert.deepEqual(window.applyCredit(3n, 0n), "invalid_cursor");
  assert.deepEqual(window.applyCredit(4n, 3n), "invalid_cursor");
  assert.deepEqual(window.applyCredit(2n, 3n), "invalid_cursor");
  assert.deepEqual(window.validateComplete(3n), "invalid_cursor");
  assert.deepEqual(window.applyCredit(3n, 3n), undefined);
  assert.deepEqual(window.applyCredit(3n, 3n), undefined);
  assert.deepEqual(window.validateComplete(3n), undefined);
  assert.deepEqual(window.validateComplete(4n), "invalid_cursor");
  assert.deepEqual(window.commitFrame(5n, 1), "invalid_cursor");
  assert.deepEqual(window.validateFrameSlot(7), "payload_too_large");
  assert.deepEqual(window.commitFrame(4n, 6), undefined);
  assert.deepEqual(window.validateFrameSlot(3), "window_full");
});
