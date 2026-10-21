/** Reducing optional authority retires the wider socket without restarting accepted work. */
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

test("optional authority reduction reaps wider transport and revokes old Live while fresh base work recovers", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const targetIdentity = await runtime.registerService({
        deployment: "reduction-target",
        name: "reduction-target",
        contract: participants.TransportGrowthTarget.participant,
      });
      const target = await TrellisService.connect({
        trellisUrl: runtime.oauthIssuer,
        participant: participants.TransportGrowthTarget.participant,
        seed: targetIdentity.seed,
      }).orThrow();
      const targetExit = target.wait().catch((error: unknown) => error);
      try {
        await target.handleAdvance(() => Result.ok({}));
        let extensions = 0;
        await target.handleExtend(() => {
          extensions++;
          return Result.ok({});
        });
        const contract = participants.TransportGrowthSubject.participant;
        const identity = await runtime.registerService({
          deployment: "reduction-subject",
          name: "reduction-subject",
          contract,
        });
        const capabilityId = "runtime-trellis.transport_growth@v1::extend";
        const initial = await runtime.callAdminRpc("authCapabilityGrantsList", {
          capabilityId,
        });
        assert.equal(initial.items.length, 1);
        const grant = initial.items[0];
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
        await service.handleWatch(async ({ input, emit, signal }) => {
          const sourceGeneration = BigInt(++starts);
          await emit({
            ...input,
            sourceGeneration,
            index: 1n,
            payload: new Uint8Array(),
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
          if (!signal.aborted)
            await emit({
              ...input,
              sourceGeneration,
              index: 2n,
              payload: new Uint8Array(),
              padding: "after",
            }).orThrow();
        });
        const caller = await runtime.connectClient({
          name: "reduction-observer",
          contract: apps.LiveProbeCaller.request,
        });
        const abort = new AbortController();
        const gate = runtime.nativeTransportGate();
        try {
          const held = await caller
            .watch(
              { runId: ulid(), streamId: "retained" },
              { signal: abort.signal },
            )
            .orThrow();
          assert.equal((await deadline(held.next(), 10_000)).value?.index, 1n);
          await runtime.callAdminRpc("authCapabilityGrantsGrant", {
            principalId: grant.principalId,
            capabilityId,
            expectedRevision: 0n,
            idempotencyKey: ulid(),
          });
          await runtime.waitFor(
            () =>
              service.connection.availability().capabilities[capabilityId] ===
              true,
            { timeoutMs: 90_000 },
          );
          assert.deepEqual(await service.extend({}).orThrow(), {});
          const route = apis.transport_growth.API.actions["rpc:Extend"].subject;
          const wider = gate
            .connections()
            .filter((socket) =>
              socket.outboundContexts.some(
                (message) => message.subject === route,
              ),
            );
          assert.equal(wider.length, 1);
          const current = await runtime.callAdminRpc(
            "authCapabilityGrantsList",
            {
              capabilityId,
              principalId: grant.principalId,
            },
          );
          assert.equal(current.items.length, 1);
          await runtime.callAdminRpc("authCapabilityGrantsRevoke", {
            grantId: current.items[0].grantId,
            expectedRevision: current.items[0].revision,
            idempotencyKey: ulid(),
          });
          await deadline(gate.waitForClose(wider[0].id), 30_000);
          await runtime.waitFor(
            () =>
              service.connection.allowsAction(
                "runtime-trellis.transport_growth@v1",
                "rpc",
                "Extend",
                "call",
              ) === false,
          );
          assert((await service.extend({})).isErr());
          assert.equal(extensions, 1);
          assert.deepEqual(await service.advance({}).orThrow(), {});
          await assert.rejects(deadline(held.next(), 10_000), {
            code: "authorization_revoked",
          });
          const fresh = await caller
            .watch(
              { runId: ulid(), streamId: "after-reduction" },
              { signal: abort.signal },
            )
            .orThrow();
          assert.equal(
            (await deadline(fresh.next(), 10_000)).value?.sourceGeneration,
            2n,
          );
          release.resolve();
          const after = (await deadline(fresh.next(), 10_000)).value;
          assert.equal(after?.index, 2n);
          assert.equal(after?.sourceGeneration, 2n);
          assert.equal(
            starts,
            2,
            "fresh admission must not revive the revoked observation",
          );
        } finally {
          release.resolve();
          abort.abort();
          await caller.connection.close();
          await service.stop();
          await serviceExit;
        }
      } finally {
        await target.stop();
        await targetExit;
      }
    },
    {
      interruptibleNativeProxy: true,
      authorization: {
        contextLifetimeSeconds: 60,
        refreshLeadSeconds: 15,
        refreshJitterSeconds: 0,
        minimumContextLifetimeSeconds: 26,
        allowedClockSkewSeconds: 2,
      },
    },
  );
});
