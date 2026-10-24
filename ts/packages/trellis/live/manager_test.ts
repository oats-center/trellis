import assert from "node:assert/strict";
import { test } from "node:test";

import { liveConstants } from "../auth/protocol_wasm.ts";
import { LiveSessionManager } from "./manager.ts";

const MAX_CONSUMERS = liveConstants().maxConsumerSessions;

test("NX09 same-epoch resume keeps generation; reconnect does not revive", () => {
  const live = new LiveSessionManager();
  const first = live.admitConsumer();
  const generation = live.generation();
  live.resume();
  assert.deepEqual(live.generation(), generation, "resume is not a reconnect");
  assert.deepEqual(live.isAvailable(), true);
  const second = live.admitConsumer();
  first[Symbol.dispose]();
  second[Symbol.dispose]();

  live.suspend();
  const afterDisconnect = live.generation();
  assert.deepEqual(afterDisconnect > generation, true);
  assert.deepEqual(live.isAvailable(), false);
  assert.throws(() => live.admitConsumer(), Error);
  live.resume();
  assert.deepEqual(live.generation(), afterDisconnect);
  assert.deepEqual(live.isAvailable(), true);
  const replacement = live.admitConsumer();
  replacement[Symbol.dispose]();
});

test("NX10 failed setup releases the consumer permit", () => {
  const live = new LiveSessionManager();
  const permits = Array.from({ length: MAX_CONSUMERS }, () =>
    live.admitConsumer(),
  );
  assert.deepEqual(live.consumerCount(), MAX_CONSUMERS);
  assert.throws(() => live.admitConsumer(), Error);
  for (const permit of permits) permit[Symbol.dispose]();
  assert.deepEqual(live.consumerCount(), 0);
  const again = live.admitConsumer();
  again[Symbol.dispose]();
  live.stop();
  assert.deepEqual(live.isAvailable(), false);
  assert.throws(() => live.admitConsumer(), Error);
});

test("D4 manager fencing reaches every registered session and a late one", () => {
  const live = new LiveSessionManager();
  const fenced: string[] = [];
  const session = (id: string) => ({
    fence: () => fenced.push(id),
    close: () => Promise.resolve(),
  });
  const owner = {};
  live.registerSession("a", session("a"), owner, "route");
  live.registerSession("b", session("b"), owner, "route");
  live.suspend();
  assert.deepEqual(fenced.sort(), ["a", "b"], "suspend fences every session");
  // A registration racing a suspended generation is fenced immediately.
  live.registerSession("late", session("late"), owner, "route");
  assert.deepEqual(fenced.includes("late"), true);
  const generation = live.generation();
  live.resume();
  assert.deepEqual(
    live.generation(),
    generation,
    "resume keeps the generation after a fence",
  );
});

test("D5 session ownership spans active, terminal, and owner-loss windows", () => {
  const live = new LiveSessionManager();
  const session = { fence: () => {}, close: () => Promise.resolve() };
  const owner = { provider: "owner" };
  const survivor = { provider: "survivor" };
  const unregisterOwner = live.registerProvider(owner, "live.route.A");
  live.registerProvider(survivor, "live.route.A");

  assert.deepEqual(
    live.ownerOf("s1"),
    undefined,
    "a never-offered session has no authoritative provider",
  );

  const deregister = live.registerSession("s1", session, owner, "live.route.A");
  assert.deepEqual(
    live.ownerOf("s1"),
    owner,
    "the registering provider owns it",
  );

  live.insertReceipt({
    sessionId: "s1",
    ownerConnectionId: "owner-connection",
    ownerSessionKey: "owner-session",
    baseSubject: "live.watch",
    reason: "complete",
    cleanup: "complete",
    finalSeq: "1",
    receivedSeq: "0",
    consumedSeq: "0",
  });
  deregister();
  assert.deepEqual(
    live.ownerOf("s1"),
    owner,
    "the owner stays authoritative through the terminal receipt window",
  );

  // The physical owner is disposed: the retained receipt still has exactly one
  // live responder rather than every provider falling silent.
  unregisterOwner();
  assert.deepEqual(
    live.ownerOf("s1"),
    survivor,
    "ownership fails over to a surviving provider for a retained receipt",
  );

  assert.deepEqual(
    live.ownerOf("never-offered"),
    undefined,
    "a genuinely unknown session keeps the signed unknown-session contract",
  );
});

test("D6 owner failover stays on the session's exact route", () => {
  const live = new LiveSessionManager();
  const session = { fence: () => {}, close: () => Promise.resolve() };
  const owner = { provider: "owner" };
  const sameRoute = { provider: "same-route" };
  // Registered first, but on a different route: it never receives this
  // session's control frame and must never be elected.
  live.registerProvider({ provider: "unrelated" }, "live.route.B");
  live.registerProvider(sameRoute, "live.route.A");
  const unregisterOwner = live.registerProvider(owner, "live.route.A");

  live.registerSession("s1", session, owner, "live.route.A");
  live.insertReceipt({
    sessionId: "s1",
    ownerConnectionId: "owner-connection",
    ownerSessionKey: "owner-session",
    baseSubject: "live.route.A",
    reason: "complete",
    cleanup: "complete",
    finalSeq: "1",
    receivedSeq: "0",
    consumedSeq: "0",
  });
  unregisterOwner();
  assert.deepEqual(
    live.ownerOf("s1"),
    sameRoute,
    "failover elects only a provider on the session's exact route",
  );
});
