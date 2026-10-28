/** Retained Live work and fresh intake survive an automatic authority rollover. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { deadline } from "@nats-io/nats-core";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import { ulid } from "ulid";
import {
  apis,
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

test("accepted Live survives growth while fresh intake uses the new attachment", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const targetIdentity = await runtime.registerService({
        deployment: "live-growth-target",
        name: "live-growth-target",
        contract: participants.TransportGrowthTarget.participant,
      });
      const target = await TrellisService.connect({
        trellisUrl: runtime.oauthIssuer,
        participant: participants.TransportGrowthTarget.participant,
        seed: targetIdentity.seed,
      }).orThrow();
      const targetExit = target.wait().catch((error: unknown) => error);
      await target.handleAdvance(() => Result.ok({}));
      await target.handleExtend(() => Result.ok({}));
      const contract = participants.TransportGrowthSubject.participant;
      const identity = await runtime.registerService({
        deployment: "live-growth-subject",
        name: "live-growth-subject",
        contract,
      });
      const capabilityId = "runtime-trellis.transport_growth.v1:growth.extend";
      const grants = await runtime.callAdminRpc("authCapabilityGrantsList", {
        capabilityId,
      });
      assert.equal(grants.items.length, 1);
      const grant = grants.items[0];
      await runtime.callAdminRpc("authCapabilityGrantsRevoke", {
        grantId: grant.grantId,
        expectedRevision: grant.revision,
        idempotencyKey: ulid(),
      });
      const service = await TrellisService.connect({
        trellisUrl: runtime.oauthIssuer,
        participant: contract,
        seed: identity.seed,
      }).orThrow();
      const serviceExit = service.wait().catch((error: unknown) => error);
      const release = Promise.withResolvers<void>();
      let starts = 0;
      let cleanups = 0;
      await service.handleWatch(async ({ input, emit, signal }) => {
        const source = BigInt(++starts);
        try {
          await emit({
            ...input,
            sourceGeneration: source,
            index: 1n,
            payload: new Uint8Array([1]),
            padding: "before",
          }).orThrow();
          await Promise.race([
            release.promise,
            new Promise<void>((resolve) => {
              if (signal.aborted) resolve();
              else
                signal.addEventListener("abort", () => resolve(), {
                  once: true,
                });
            }),
          ]);
          if (signal.aborted) return;
          await emit({
            ...input,
            sourceGeneration: source,
            index: 2n,
            payload: new Uint8Array([2]),
            padding: "after",
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
        name: "live-growth-caller",
        contract: apps.LiveProbeCaller.request,
      });
      const firstAbort = new AbortController();
      const secondAbort = new AbortController();
      const gate = runtime.nativeTransportGate();
      const route = apis.liveprobe.API.actions["live:Watch"].subject;
      try {
        const first = await caller
          .watch(
            { runId: ulid(), streamId: "retained" },
            { signal: firstAbort.signal },
          )
          .orThrow();
        assert.equal((await deadline(first.next(), 10_000)).value?.index, 1n);
        const originals = gate
          .connections()
          .filter((connection) =>
            connection.deliveries.some((message) => message.subject === route),
          );
        assert.equal(originals.length, 1);
        const original = originals[0];
        await runtime.callAdminRpc("authCapabilityGrantsGrant", {
          principalId: grant.principalId,
          capabilityId,
          expectedRevision: 0n,
          idempotencyKey: ulid(),
        });
        await runtime.waitFor(async () => (await service.extend({})).isOk(), {
          timeoutMs: 90_000,
        });
        assert.equal(cleanups, 0);
        const second = await caller
          .watch(
            { runId: ulid(), streamId: "fresh" },
            { signal: secondAbort.signal },
          )
          .orThrow();
        assert.equal((await deadline(second.next(), 10_000)).value?.index, 1n);
        const successor = gate
          .connections()
          .find(
            (connection) =>
              connection.id !== original.id &&
              connection.deliveries.some(
                (message) => message.subject === route,
              ),
          );
        assert(successor, "fresh Live intake must use the promoted attachment");
        assert.equal(gate.connection(original.id)?.closed, false);
        release.resolve();
        assert.equal((await deadline(first.next(), 10_000)).value?.index, 2n);
        assert.equal((await deadline(second.next(), 10_000)).value?.index, 2n);
        assert.equal(starts, 2);
        assert.equal(cleanups, 0);
        firstAbort.abort();
        await runtime.waitFor(() => cleanups === 1);
        await deadline(gate.waitForClose(original.id), 15_000);
        assert.equal(gate.connection(successor.id)?.closed, false);
        secondAbort.abort();
        await runtime.waitFor(() => cleanups === 2);
      } finally {
        release.resolve();
        firstAbort.abort();
        secondAbort.abort();
        await caller.connection.close();
        await service.stop();
        await target.stop();
        await serviceExit;
        await targetExit;
      }
    },
    {
      interruptibleNativeProxy: true,
      authorization: {
        contextLifetimeSeconds: 30,
        allowedClockSkewSeconds: 2,
      },
    },
  );
});
