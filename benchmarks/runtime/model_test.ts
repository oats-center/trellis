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
