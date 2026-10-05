import { assertEquals } from "@std/assert";
import { CreditScheduler } from "./credit.ts";

Deno.test("credit coalesces without postponing and thresholds force delivery", () => {
  const scheduler = new CreditScheduler({
    frameStep: 8,
    byteStep: 100,
    maxDelayMs: 25,
  });
  scheduler.notePending(0, 1, 10);
  scheduler.notePending(20, 2, 20);
  assertEquals(scheduler.isDue(24), false);
  assertEquals(scheduler.isDue(25), true);
  scheduler.clear();
  scheduler.notePending(30, 8, 80);
  assertEquals(scheduler.isDue(30), true);
  scheduler.clear();
  scheduler.notePending(40, 1, 100);
  assertEquals(scheduler.isDue(40), true);
  scheduler.clear();
  scheduler.notePending(50, 1, 10);
  scheduler.force(50);
  assertEquals(scheduler.isDue(50), true);
});
