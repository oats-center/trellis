import assert from "node:assert/strict";
import { test } from "node:test";
import { Objm } from "@nats-io/obj";
import {
  type ConsumerInfo,
  jetstream,
  jetstreamManager,
} from "@nats-io/jetstream";

import { startTrellisRuntime } from "../../integration/_support/runtime.ts";
import type { TrellisTestRuntime } from "@oatscenter/trellis-testkit";
import { StoreError } from "./errors/StoreError.ts";
import { TypedStore } from "./store.ts";
import { fixedTransportProvider } from "./transport/generations.ts";

test("Store verifies demand-driven reads, cancellation, corruption and wait budgets", async () => {
  let nats: TrellisTestRuntime | undefined;
  try {
    nats = await startTrellisRuntime();
    const nc = await nats.connectNats();
    const store = (
      await TypedStore.open(fixedTransportProvider(nc), "store_test", {})
    ).orThrow();

    const payload = new TextEncoder().encode("stored generation bytes");
    await store.put("k", payload).orThrow();

    const entry = (await store.get("k")).orThrow();
    assert.deepEqual(
      new TextDecoder().decode(await entry.bytes().orThrow()),
      "stored generation bytes",
    );

    const waited = (await store.waitFor("k", { timeoutMs: 2_000 })).orThrow();
    assert.deepEqual(waited.key, "k");

    const manager = await jetstreamManager(nc, { checkAPI: false });
    const native = await new Objm(nc).open("store_test");
    const large = new Uint8Array(
      Math.max(8 * 1024 * 1024, 2 * nc.info!.max_payload),
    );
    for (let i = 0; i < large.length; i++) large[i] = i % 251;
    await store.put("large", large).orThrow();
    const object = await native.info("large");
    assert(object);
    const subject = `$O.store_test.C.${object.nuid}`;
    const largeEntry = await store.get("large").orThrow();
    const reader = (await largeEntry.stream().orThrow()).getReader();
    try {
      const first = await reader.read();
      assert(!first.done);
      let held: ConsumerInfo | undefined;
      await nats.waitFor(async () => {
        held = (await manager.consumers.list("OBJ_store_test").next()).find(
          (item) => item.config.filter_subject === subject,
        );
        return (
          !!held && held.num_waiting === 0 && held.delivered.consumer_seq > 0
        );
      });
      assert(held);
      assert(held.config.max_bytes);
      // Actual first-chunk size and broker delivery cursor measure delivered
      // object bytes, rather than inspecting a private SDK queue or its defaults.
      assert(large.length > held.config.max_bytes);
      assert(held.delivered.consumer_seq < object.chunks);
      assert(
        held.delivered.consumer_seq * first.value.length <=
          held.config.max_bytes,
      );
      // A second independent read must complete without advancing this paused
      // reader. Real consumer deletion exercises the same resume path as native
      // inactive-threshold expiry, without waiting five minutes.
      assert.deepEqual(await largeEntry.bytes().orThrow(), large);
      const stillHeld = await manager.consumers.info(
        "OBJ_store_test",
        held.name,
      );
      assert.deepEqual(
        stillHeld.delivered.consumer_seq,
        held.delivered.consumer_seq,
      );
      await manager.consumers.delete("OBJ_store_test", held.name);
      const received = new Uint8Array(large.length);
      received.set(first.value);
      let offset = first.value.length;
      for (;;) {
        const next = await reader.read();
        if (next.done) break;
        received.set(next.value, offset);
        offset += next.value.length;
      }
      assert.deepEqual(offset, large.length);
      assert.deepEqual(received, large);
    } finally {
      await reader.cancel();
      reader.releaseLock();
    }
    assert.deepEqual(
      (await manager.streams.info("OBJ_store_test")).state.consumer_count,
      0,
    );

    const cancelled = (await largeEntry.stream().orThrow()).getReader();
    assert(!(await cancelled.read()).done);
    await cancelled.cancel();
    cancelled.releaseLock();
    assert.deepEqual(
      (await manager.streams.info("OBJ_store_test")).state.consumer_count,
      0,
    );

    await store.put("empty", new Uint8Array()).orThrow();
    assert.deepEqual(
      await (await store.get("empty").orThrow()).bytes().orThrow(),
      new Uint8Array(),
    );
    assert.deepEqual(
      (await manager.streams.info("OBJ_store_test")).state.consumer_count,
      0,
    );

    const original = new TextEncoder().encode("integrity checked bytes");
    await store.put("corrupt", original).orThrow();
    const corrupt = await native.info("corrupt");
    assert(corrupt);
    const corruptSubject = `$O.store_test.C.${corrupt.nuid}`;
    await manager.streams.purge("OBJ_store_test", { filter: corruptSubject });
    const changed = original.slice();
    changed[0] ^= 1;
    await jetstream(nc).publish(corruptSubject, changed);
    const rejected = await (await store.get("corrupt").orThrow()).bytes();
    assert(rejected.error instanceof StoreError);
    assert.deepEqual(
      (await manager.streams.info("OBJ_store_test")).state.consumer_count,
      0,
    );

    // The wait timeout bounds the whole attempt, not just key polling.
    const missing = await store.waitFor("missing", {
      timeoutMs: 250,
      pollIntervalMs: 25,
    });
    assert(missing.error instanceof StoreError);
    assert.deepEqual(missing.error.getContext().reason, "timeout");

    await store.delete("k").orThrow();
    const deleted = await store.get("k");
    assert(deleted.error instanceof StoreError);
    assert.deepEqual(deleted.error.getContext().reason, "not_found");
  } finally {
    await nats?.stop();
  }
});
