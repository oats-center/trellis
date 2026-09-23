import { assertEquals, assertThrows } from "@std/assert";

import { liveConstants } from "../auth/protocol_wasm.ts";
import { LiveSessionManager } from "./manager.ts";

const MAX_CONSUMERS = liveConstants().maxConsumerSessions;

Deno.test("NX09 same-epoch resume keeps generation; reconnect does not revive", () => {
  const live = new LiveSessionManager();
  assertEquals(live.generation(), 1);
  assertEquals(live.isAvailable(), true);
  const first = live.admitConsumer();
  const generation = live.generation();
  live.resume();
  assertEquals(live.generation(), generation, "resume is not a reconnect");
  assertEquals(live.isAvailable(), true);
  const second = live.admitConsumer();
  first[Symbol.dispose]();
  second[Symbol.dispose]();

  live.suspend();
  const afterDisconnect = live.generation();
  assertEquals(afterDisconnect > generation, true);
  assertEquals(live.isAvailable(), false);
  assertThrows(() => live.admitConsumer());
  live.resume();
  assertEquals(live.generation(), afterDisconnect);
  assertEquals(live.isAvailable(), true);
  const replacement = live.admitConsumer();
  replacement[Symbol.dispose]();
});

Deno.test("NX10 failed setup releases the consumer permit", () => {
  const live = new LiveSessionManager();
  const permits = Array.from(
    { length: MAX_CONSUMERS },
    () => live.admitConsumer(),
  );
  assertEquals(live.consumerCount(), MAX_CONSUMERS);
  assertThrows(() => live.admitConsumer());
  for (const permit of permits) permit[Symbol.dispose]();
  assertEquals(live.consumerCount(), 0);
  const again = live.admitConsumer();
  again[Symbol.dispose]();
  live.stop();
  assertEquals(live.isAvailable(), false);
  assertThrows(() => live.admitConsumer());
});

Deno.test("D4 manager fencing reaches every registered session and a late one", () => {
  const live = new LiveSessionManager();
  const fenced: string[] = [];
  const session = (id: string) => ({
    fence: () => fenced.push(id),
    close: () => Promise.resolve(),
  });
  live.registerSession(session("a"));
  live.registerSession(session("b"));
  live.suspend();
  assertEquals(fenced.sort(), ["a", "b"], "suspend fences every session");
  // A registration racing a suspended generation is fenced immediately.
  live.registerSession(session("late"));
  assertEquals(fenced.includes("late"), true);
  const generation = live.generation();
  live.resume();
  assertEquals(
    live.generation(),
    generation,
    "resume keeps the generation after a fence",
  );
});
