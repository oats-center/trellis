import assert from "node:assert/strict";
import { test } from "node:test";
import { Backoff } from "./backoff.ts";

test("backoff doubles to its bound, never overflows, and restarts after success", () => {
  const backoff = new Backoff(250, 5_000);
  const waits = Array.from({ length: 7 }, () => backoff.failed());
  assert.deepEqual(waits, [250, 500, 1_000, 2_000, 4_000, 5_000, 5_000]);
  for (let i = 0; i < 100; i += 1) backoff.failed();
  assert.equal(backoff.nextDelayMs, 5_000);
  backoff.succeeded();
  assert.equal(backoff.failed(), 250);
});
