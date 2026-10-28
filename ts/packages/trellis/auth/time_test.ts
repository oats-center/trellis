import assert from "node:assert/strict";
import { test } from "node:test";

import { correctedIatSeconds, estimateMidpointClockOffsetMs } from "./time.ts";

test("estimateMidpointClockOffsetMs uses the request midpoint", () => {
  assert.deepEqual(
    estimateMidpointClockOffsetMs({
      requestStartedAtMs: 1_000,
      responseReceivedAtMs: 1_400,
      serverNowSeconds: 2,
    }),
    800,
  );
});

test("correctedIatSeconds applies the estimated offset", () => {
  assert.deepEqual(correctedIatSeconds(1_700_000_000_250, 900), 1_700_000_001);
});
