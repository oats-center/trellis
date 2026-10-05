import { assertEquals } from "@std/assert";
import { SenderWindow } from "./flow.ts";

Deno.test("byte credit must match consumed prefix before capacity release", () => {
  const window = new SenderWindow({
    maxFrameBytes: 5,
    windowFrames: 2,
    windowBytes: 8,
  });
  window.commitFrame(1n, 3);
  window.commitFrame(2n, 5);
  for (const bytes of [0n, 2n, 4n, 8n]) {
    assertEquals(window.validateCredit(2n, 1n, bytes), "invalid_cursor");
    assertEquals(window.applyCredit(2n, 1n, bytes), "invalid_cursor");
    assertEquals(window.validateFrameSlot(1), "window_full");
  }
  assertEquals(window.applyCredit(2n, 1n, 3n), undefined);
  assertEquals(window.commitFrame(3n, 3), undefined);
  assertEquals(window.applyCredit(3n, 2n, 5n), "invalid_cursor");
  assertEquals(window.validateFrameSlot(1), "window_full");
  assertEquals(window.applyCredit(3n, 2n, 8n), undefined);
  assertEquals(window.applyCredit(3n, 2n, 8n), undefined);
  assertEquals(window.applyCredit(3n, 2n, 9n), "invalid_cursor");
  assertEquals(window.applyCredit(3n, 3n, 11n), undefined);
  assertEquals(window.validateComplete(3n), undefined);
});

Deno.test("consumed credit releases bounded capacity and wakes sender", async () => {
  const window = new SenderWindow({
    maxFrameBytes: 6,
    windowFrames: 2,
    windowBytes: 8,
  });
  assertEquals(window.commitFrame(1n, 3), undefined);
  assertEquals(window.commitFrame(2n, 5), undefined);
  assertEquals(window.validateFrameSlot(1), "window_full");
  assertEquals(window.applyCredit(2n, 0n), undefined);
  assertEquals(window.validateFrameSlot(1), "window_full");
  const notified = window.waitCredit();
  assertEquals(window.applyCredit(2n, 1n), undefined);
  await notified;
  assertEquals(window.commitFrame(3n, 3), undefined);
  assertEquals(window.validateFrameSlot(1), "window_full");
  assertEquals(window.applyCredit(1n, 1n), "invalid_cursor");
  assertEquals(window.applyCredit(3n, 0n), "invalid_cursor");
  assertEquals(window.applyCredit(4n, 3n), "invalid_cursor");
  assertEquals(window.applyCredit(2n, 3n), "invalid_cursor");
  assertEquals(window.validateComplete(3n), "invalid_cursor");
  assertEquals(window.applyCredit(3n, 3n), undefined);
  assertEquals(window.applyCredit(3n, 3n), undefined);
  assertEquals(window.validateComplete(3n), undefined);
  assertEquals(window.validateComplete(4n), "invalid_cursor");
  assertEquals(window.commitFrame(5n, 1), "invalid_cursor");
  assertEquals(window.validateFrameSlot(7), "payload_too_large");
  assertEquals(window.commitFrame(4n, 6), undefined);
  assertEquals(window.validateFrameSlot(3), "window_full");
});
