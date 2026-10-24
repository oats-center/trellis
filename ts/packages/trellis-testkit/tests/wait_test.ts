import assert from "node:assert/strict";
import { test } from "node:test";
import { waitFor } from "../src/wait.ts";

test("waitFor returns first truthy value", async () => {
  let attempts = 0;

  const value = await waitFor(
    () => {
      attempts += 1;
      return attempts === 3 ? "ready" : null;
    },
    { timeoutMs: 1_000, intervalMs: 1 },
  );

  assert.equal(value, "ready");
});

test("waitFor preserves last thrown error in timeout", async () => {
  await assert.rejects(
    () =>
      waitFor(
        () => {
          throw new Error("still booting");
        },
        { timeoutMs: 5, intervalMs: 1 },
      ),
    /still booting/,
  );
});
