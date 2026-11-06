/** A Live cancelled as its caller's authority renews still closes promptly. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { deadline } from "@nats-io/nats-core";
import { TrellisService } from "@oatscenter/trellis/service";
import { ulid } from "ulid";
import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

test("a Live cancelled during its caller's renewal closes without waiting out the peer", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const contract = participants.TransportGrowthSubject.participant;
      const identity = await runtime.registerService({
        deployment: "live-renewal-subject",
        name: "live-renewal-subject",
        contract,
      });
      const service = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: contract,
        seed: identity.seed,
      }).orThrow();
      const serviceExit = service.wait().catch((error: unknown) => error);
      let cleanups = 0;
      await service.handleWatch(async ({ input, emit, signal }) => {
        try {
          await emit({
            ...input,
            sourceGeneration: 1n,
            index: 1n,
            payload: new Uint8Array([1]),
            padding: "",
          }).orThrow();
          if (!signal.aborted)
            await new Promise<void>((resolve) =>
              signal.addEventListener("abort", () => resolve(), { once: true }),
            );
        } finally {
          cleanups++;
        }
      });
      const caller = await runtime.connectClient({
        name: "live-renewal-caller",
        contract: apps.LiveProbeCaller.request,
      });
      const abort = new AbortController();
      try {
        const watch = await caller
          .watch(
            { runId: ulid(), streamId: "renewal" },
            { signal: abort.signal },
          )
          .orThrow();
        assert.equal((await deadline(watch.next(), 10_000)).value?.index, 1n);
        // Cancel in the same turn the caller's authority changes, where the
        // Live re-sends credit under the renewed context.
        const initial = caller.ownAuthority.current?.authorityDigest;
        assert(initial);
        const renewed = Promise.withResolvers<void>();
        const unsubscribe = caller.ownAuthority.subscribe((snapshot) => {
          if (snapshot?.authorityDigest === initial) return;
          setTimeout(() => {
            abort.abort();
            renewed.resolve();
          }, 0);
        });
        try {
          await deadline(renewed.promise, 40_000);
        } finally {
          unsubscribe();
        }
        // The close exchange takes at most five seconds; a lost close would
        // leave the provider waiting out its 35-second inactivity bound.
        await runtime.waitFor(() => cleanups === 1, { timeoutMs: 8_000 });
      } finally {
        abort.abort();
        await caller.connection.close();
        await service.stop();
        await serviceExit;
      }
    },
    {
      authorization: {
        contextLifetimeSeconds: 30,
        allowedClockSkewSeconds: 2,
      },
    },
  );
});
