import { assertEquals } from "@std/assert";
import { type Sample, summarize } from "./model.ts";

Deno.test("latency summaries retain failures and separate session cohorts", () => {
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
  assertEquals(one.medianMs, 20);
  assertEquals(one.attempts, 4);
  assertEquals(one.errors, 2);
  assertEquals(one.submitted, 3);
  assertEquals(one.drops, 1);
  assertEquals(one.p95Ms, null);
  assertEquals(ten.medianMs, 200);
  assertEquals(ten.attempts, 1);
});

Deno.test("warmups stay raw but do not skew measured percentiles", () => {
  const rows: Sample[] = Array.from({ length: 20 }, (_, index) => ({
    scenario: "download",
    transport: "trellis",
    startedUnixMs: index,
    durationMs: index + 1,
  }));
  rows.push({ ...rows[0], durationMs: 1000, warmup: true });
  const [summary] = summarize(rows);
  assertEquals(summary.attempts, 20);
  assertEquals(summary.medianMs, 10.5);
  assertEquals(summary.p95Ms, 19);
});
