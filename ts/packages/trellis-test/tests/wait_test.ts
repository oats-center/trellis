import { assertEquals, assertRejects } from "@std/assert";
import { waitFor } from "../src/wait.ts";

Deno.test("waitFor returns first truthy value", async () => {
  let attempts = 0;

  const value = await waitFor(() => {
    attempts += 1;
    return attempts === 3 ? "ready" : null;
  }, { timeoutMs: 1_000, intervalMs: 1 });

  assertEquals(value, "ready");
});

Deno.test("waitFor preserves last thrown error in timeout", async () => {
  await assertRejects(
    () =>
      waitFor(() => {
        throw new Error("still booting");
      }, { timeoutMs: 5, intervalMs: 1 }),
    Error,
    "still booting",
  );
});
