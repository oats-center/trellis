import assert from "node:assert/strict";
import { test } from "node:test";
import { discoverNativeOAuth } from "../native.ts";

test("native discovery rejects insecure non-loopback issuers and URLs carrying credentials", async () => {
  for (const origin of [
    "http://example.com",
    "https://user:password@example.com",
  ]) {
    const result = await discoverNativeOAuth(origin);
    assert.equal(result.error?.reason, "InvalidIssuer");
  }
});
