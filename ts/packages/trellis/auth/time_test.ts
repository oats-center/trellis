import { assertEquals } from "@std/assert";

import { correctedIatSeconds, estimateMidpointClockOffsetMs } from "./time.ts";

Deno.test("estimateMidpointClockOffsetMs uses the request midpoint", () => {
  assertEquals(
    estimateMidpointClockOffsetMs({
      requestStartedAtMs: 1_000,
      responseReceivedAtMs: 1_400,
      serverNowSeconds: 2,
    }),
    800,
  );
});

Deno.test("correctedIatSeconds applies the estimated offset", () => {
  assertEquals(correctedIatSeconds(1_700_000_000_250, 900), 1_700_000_001);
});
