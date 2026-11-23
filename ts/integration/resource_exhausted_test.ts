/**
 * Writes JetStream refuses for lack of room come back as a typed
 * `ResourceExhaustedError` naming the resource, from KV, store, State, jobs
 * and events alike, rather than as a generic or unexpected failure.
 */
import { jetstreamManager } from "@nats-io/jetstream";
import assert from "node:assert/strict";
import { test } from "node:test";
import { ResourceExhaustedError } from "@oatscenter/trellis/errors";
import { TrellisService } from "@oatscenter/trellis/service";

import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

/** Asserts `error` is a refusal for room naming the `kind` resource `name`. */
function assertExhausted(
  error: unknown,
  kind: ResourceExhaustedError["kind"],
  name: (resource: string) => boolean,
): void {
  assert(error instanceof ResourceExhaustedError, String(error));
  assert.equal(error.kind, kind, error.message);
  assert(name(error.resource), error.message);
  assert(error.reason.length > 0, error.message);
}

test("writes JetStream refuses for lack of room name the resource", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const contract = participants.Provider.participant;
      const identity = await runtime.registerService({
        name: "full-broker",
        contract,
      });
      const client = await runtime.connectClient({
        name: "full-broker-state",
        contract: apps.Caller.request,
      });
      const service = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: contract,
        seed: identity.seed,
      }).orThrow();
      const exit = service.wait().catch((error: unknown) => error);
      try {
        // State registers on first use, before the broker is full.
        await client.state.saved.get().orThrow();
        const store = await service.store.files.open().orThrow();

        // Half-megabyte values until JetStream has no room for the next one.
        const large = { value: "x".repeat(512 * 1024) };
        let refused: unknown;
        for (let index = 0; index < 2_000 && refused === undefined; index++) {
          const written = await service.kv.records.put(`fill.${index}`, large);
          if (written.isErr()) refused = written.error;
        }
        assertExhausted(refused, "kv", (name) => name === "records");

        const stored = await store.put(
          "full/object",
          new Uint8Array(64 * 1024).fill(7),
        );
        assert(stored.isErr());
        assertExhausted(stored.error, "store", (name) => name === "files");

        const job = await service.jobs.work.create({ value: "no room" });
        assert(job.isErr());
        assertExhausted(job.error, "jobs", (name) => name === "work");

        const published = await service.publishChanged({ value: "full" });
        assert(published.isErr());
        assertExhausted(published.error, "events", (name) =>
          name.includes(".Changed"),
        );

        const saved = await client.state.saved.set({ value: "no room" });
        assert(saved.isErr());
        assertExhausted(saved.error, "state", (name) => name === "saved");

        // Making room, as an operator would by purging the filled bucket,
        // lets the same handle write again.
        const nats = await runtime.connectNats();
        try {
          const manager = await jetstreamManager(nats);
          for await (const stream of manager.streams.list()) {
            if (
              stream.config.name.startsWith("KV_") &&
              stream.state.bytes > 64 * 1024 * 1024
            ) {
              await manager.streams.purge(stream.config.name);
            }
          }
        } finally {
          await nats.close();
        }
        await service.kv.records.put("after", large).orThrow();
      } finally {
        await service.stop();
        await exit;
      }
    },
    { jetstreamMaxFileStore: 256 * 1024 * 1024 },
  );
});
