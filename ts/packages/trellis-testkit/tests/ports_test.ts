import assert from "node:assert/strict";
import { test } from "node:test";
import { reserveLocalPort, type ReservedPort } from "../src/ports.ts";

test("port reservations exclude other listeners until their release completes", async () => {
  const first = await reserveLocalPort();
  let second: ReservedPort | undefined;
  try {
    await assert.rejects(() => reserveLocalPort(first.port), {
      code: "EADDRINUSE",
    });
    await Promise.all([first.releaseForSpawn(), first.release()]);
    second = await reserveLocalPort(first.port);
  } finally {
    await first.release();
    if (second) await second.release();
  }
});
