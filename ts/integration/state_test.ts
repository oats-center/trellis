import { jetstreamManager } from "@nats-io/jetstream";
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { StateConflictError } from "../packages/trellis/session.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

test("State and authentication survive restart with retained KV history", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.registerService({
      name: "state-provider",
      contract: participants.Provider.participant,
    });
    // Operators may retain older KV revisions. Prove ordinary startup, auth,
    // reads, compare-and-swap, and deletion still work instead of checking a
    // configuration literal or requiring a special runtime for this test.
    const nats = await runtime.connectNats();
    const manager = await jetstreamManager(nats);
    const client = await runtime.connectClient({
      name: "state-restart",
      contract: apps.Caller.request,
    });
    try {
      // State owns one shared store. Do not change unrelated Auth-managed resources.
      const info = await manager.streams.info("KV_trellis_state");
      await manager.streams.update(info.config.name, {
        ...info.config,
        max_msgs_per_subject: 3,
      });
    } finally {
      await nats.close();
    }

    const previous = await client.state.saved
      .set({ value: "superseded" })
      .orThrow();
    await client.state.saved.set({ value: "durable" }).orThrow();
    await runtime.restart();
    const stored = await client.state.saved.get().orThrow();

    assert(stored);
    assert.deepEqual(stored.value.value, "durable");
    const stale = await client.state.saved.replace(previous.revision, {
      value: "stale",
    });
    assert(stale.isErr());
    assert(stale.error instanceof StateConflictError);
    assert.deepEqual(stale.error.current?.value, { value: "durable" });
    await client.state.saved.delete(stored.revision).orThrow();
    assert.deepEqual(await client.state.saved.get().orThrow(), undefined);
  });
});

test("generated State preserves create and replace semantics", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.registerService({
      name: "state-provider",
      contract: participants.Provider.participant,
    });
    const client = await runtime.connectClient({
      name: "state-write-modes",
      contract: apps.Caller.request,
    });

    const created = await client.state.saved
      .create({ value: "created" })
      .orThrow();
    const duplicate = await client.state.saved.create({ value: "duplicate" });
    assert(duplicate.isErr());
    assert(duplicate.error instanceof StateConflictError);
    assert.deepEqual(duplicate.error.current?.value, { value: "created" });

    const replaced = await client.state.saved
      .replace(created.revision, { value: "replaced" })
      .orThrow();
    assert.deepEqual(replaced.value, { value: "replaced" });

    const stale = await client.state.saved.replace(created.revision, {
      value: "stale",
    });
    assert(stale.isErr());
    assert(stale.error instanceof StateConflictError);
    assert.deepEqual(stale.error.current?.value, { value: "replaced" });

    const staleDelete = await client.state.saved.delete(created.revision);
    assert(staleDelete.isErr());
    assert(staleDelete.error instanceof StateConflictError);
    assert.deepEqual(staleDelete.error.current?.value, { value: "replaced" });

    await client.state.saved.delete(replaced.revision).orThrow();
    const concurrent = await Promise.all([
      client.state.saved.create({ value: "first" }),
      client.state.saved.create({ value: "second" }),
    ]);
    const winner = concurrent.find((result) => result.isOk());
    const loser = concurrent.find((result) => result.isErr());
    assert(winner?.isOk());
    assert(loser?.isErr());
    assert(loser.error instanceof StateConflictError);
    assert.deepEqual(
      loser.error.current?.value,
      winner.unwrapOrElse(() => {
        throw new Error("concurrent create winner missing");
      }).value,
    );
  });
});
