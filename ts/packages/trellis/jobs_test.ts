import assert from "node:assert/strict";
import { test } from "node:test";

import { decodeJobUpdateEnvelope } from "./jobs.ts";

test("decodeJobUpdateEnvelope accepts only complete ordered envelopes", () => {
  const encoder = new TextEncoder();
  assert.deepEqual(
    decodeJobUpdateEnvelope(
      encoder.encode(
        JSON.stringify({
          jobId: "job-1",
          attempt: 2,
          sequence: 3,
          timestamp: "2026-07-10T12:00:00.000Z",
          update: { detail: "working" },
        }),
      ),
    ),
    {
      jobId: "job-1",
      attempt: 2,
      sequence: 3,
      timestamp: "2026-07-10T12:00:00.000Z",
      update: { detail: "working" },
    },
  );
  assert.deepEqual(
    decodeJobUpdateEnvelope(
      encoder.encode(
        JSON.stringify({
          jobId: "job-1",
          attempt: 0,
          sequence: 1,
          timestamp: "2026-07-10T12:00:00.000Z",
          update: {},
        }),
      ),
    ),
    undefined,
  );
});
