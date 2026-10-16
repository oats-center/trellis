import assert from "node:assert/strict";
import { test } from "node:test";
import { type Sample, summarize } from "./model.ts";

test("latency summaries retain failures and separate session cohorts", () => {
  const observed: Sample[] = [10, 30].map((durationMs) => ({
    scenario: "idle-sessions",
    transport: "trellis",
    startedUnixMs: 0,
    durationMs,
    sessions: 1,
  }));
  observed.push({ ...observed[0], durationMs: 1000, error: "timeout" });
  observed.push({
    ...observed[0],
    durationMs: 0,
    error: "outstanding limit",
    loadGeneratorDrop: true,
  });
  observed.push({ ...observed[0], durationMs: 200, sessions: 10 });
  const [one, ten] = summarize(observed);
  assert.deepEqual(one.medianMs, 20);
  assert.deepEqual(one.attempts, 4);
  assert.deepEqual(one.errors, 2);
  assert.deepEqual(one.submitted, 3);
  assert.deepEqual(one.drops, 1);
  assert.deepEqual(one.p95Ms, null);
  assert.deepEqual(ten.medianMs, 200);
  assert.deepEqual(ten.attempts, 1);
});

test("warmups stay raw but do not skew measured percentiles", () => {
  const rows: Sample[] = Array.from({ length: 20 }, (_, index) => ({
    scenario: "download",
    transport: "trellis",
    startedUnixMs: index,
    durationMs: index + 1,
  }));
  rows.push({ ...rows[0], durationMs: 1000, warmup: true });
  const [summary] = summarize(rows);
  assert.deepEqual(summary.attempts, 20);
  assert.deepEqual(summary.medianMs, 10.5);
  assert.deepEqual(summary.p95Ms, 19);
});
