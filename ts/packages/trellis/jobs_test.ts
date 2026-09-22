import { assertEquals } from "@std/assert";

import { ActiveJob, decodeJobUpdateEnvelope, JobRef } from "./jobs.ts";

Deno.test("decodeJobUpdateEnvelope accepts only complete ordered envelopes", () => {
  const encoder = new TextEncoder();
  assertEquals(
    decodeJobUpdateEnvelope(encoder.encode(JSON.stringify({
      jobId: "job-1",
      attempt: 2,
      sequence: 3,
      timestamp: "2026-07-10T12:00:00.000Z",
      update: { detail: "working" },
    }))),
    {
      jobId: "job-1",
      attempt: 2,
      sequence: 3,
      timestamp: "2026-07-10T12:00:00.000Z",
      update: { detail: "working" },
    },
  );
  assertEquals(
    decodeJobUpdateEnvelope(encoder.encode(JSON.stringify({
      jobId: "job-1",
      attempt: 0,
      sequence: 1,
      timestamp: "2026-07-10T12:00:00.000Z",
      update: {},
    }))),
    undefined,
  );
});

Deno.test("ActiveJob exposes its cancellation signal", () => {
  const ref = new JobRef<Record<string, never>, boolean>(
    { id: "job-1", service: "service", jobType: "work" },
    {
      get: () => {
        throw new Error("unused");
      },
      wait: () => {
        throw new Error("unused");
      },
      cancel: () => {
        throw new Error("unused");
      },
    },
  );
  const controller = new AbortController();
  const job: ActiveJob<Record<string, never>, boolean> = new ActiveJob(
    ref,
    {},
    {
      requestId: "request-1",
      traceId: "0".repeat(32),
      traceparent: `00-${"0".repeat(32)}-${"0".repeat(16)}-01`,
    },
    false,
    {
      heartbeat: () => {
        throw new Error("unused");
      },
      progress: () => {
        throw new Error("unused");
      },
      log: () => {
        throw new Error("unused");
      },
      waitFor: (_target, fn) => fn(),
      signal: controller.signal,
    },
  );

  assertEquals(job.signal, controller.signal);
  controller.abort("cancelled");
  assertEquals(job.signal.aborted, true);
  assertEquals(job.signal.reason, "cancelled");
});
