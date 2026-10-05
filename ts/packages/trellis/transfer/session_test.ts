import { assertRejects } from "@std/assert";
import { transferWait } from "./session.ts";

Deno.test("session cancellation releases a pending source read without waiting for its producer", async () => {
  const pendingRead = Promise.withResolvers<IteratorResult<Uint8Array>>();
  const abort = new AbortController();
  const waiting = transferWait(pendingRead.promise, abort.signal);
  abort.abort(new Error("physical disconnect"));
  await assertRejects(() => waiting, Error, "physical disconnect");
  // The application producer may still finish later; it cannot resurrect the
  // cancelled SDK wait or produce an unobserved rejection.
  pendingRead.reject(new Error("late source failure"));
  await Promise.resolve();
});
