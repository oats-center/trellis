import assert from "node:assert/strict";
import { test } from "node:test";
import { transferWait } from "./session.ts";

test("an already-cancelled wait still observes its started source failure", async () => {
  const cancelled = new Error("physical disconnect");
  await assert.rejects(
    transferWait(
      Promise.reject(new Error("late source failure")),
      AbortSignal.abort(cancelled),
    ),
    (error) => error === cancelled,
  );
  await new Promise<void>((resolve) => setImmediate(resolve));
});

test("session cancellation releases a pending source read without waiting for its producer", async () => {
  const pendingRead = Promise.withResolvers<IteratorResult<Uint8Array>>();
  const abort = new AbortController();
  const waiting = transferWait(pendingRead.promise, abort.signal);
  abort.abort(new Error("physical disconnect"));
  await assert.rejects(() => waiting, { message: "physical disconnect" });
  // The application producer may still finish later; it cannot resurrect the
  // cancelled SDK wait or produce an unobserved rejection.
  pendingRead.reject(new Error("late source failure"));
  await Promise.resolve();
});
