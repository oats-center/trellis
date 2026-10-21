/** A stale candidate snapshot must not block or corrupt a newer publication. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { deadline } from "@nats-io/nats-core";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import { ulid } from "ulid";
import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

test("a superseding capability publication does not wait for a stale mirror reply", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const contract =
        participants.TransportGrowthOperationProvider.participant;
      const identity = await runtime.registerService({
        name: "supersession",
        contract,
      });
      const firstCapability = "runtime-trellis.transport_growth@v1::extend";
      const secondCapability = "runtime-trellis.transport_growth@v1::extend2";
      const firstGrants = await runtime.callAdminRpc(
        "authCapabilityGrantsList",
        { capabilityId: firstCapability },
      );
      const secondGrants = await runtime.callAdminRpc(
        "authCapabilityGrantsList",
        { capabilityId: secondCapability },
      );
      assert.equal(firstGrants.items.length, 1);
      assert.equal(secondGrants.items.length, 1);
      const first = firstGrants.items[0];
      const second = secondGrants.items[0];
      for (const grant of [first, second])
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
      const caller = await runtime.connectClient({
        name: "supersession-caller",
        contract: apps.TransportGrowthCaller.request,
        timeout: 120_000,
      });
      const entered = Promise.withResolvers<void>();
      const release = Promise.withResolvers<void>();
      let calls = 0;
      await service.handleAdvance(() => Result.ok({}));
      await service.handleExtend(() => Result.ok({}));
      await service.handleExtend2(() => Result.ok({}));
      await service.handlePark(async () => {
        calls++;
        entered.resolve();
        await release.promise;
        return Result.ok({ value: "retained" });
      });
      const gate = runtime.nativeTransportGate();
      let hold: ReturnType<typeof gate.armResponseHold> | undefined;
      let parked: Promise<unknown> | undefined;
      try {
        parked = caller.park({}).orThrow();
        void parked.catch(() => undefined);
        await deadline(entered.promise, 10_000);
        const before = new Set(
          gate.connections().map((connection) => connection.id),
        );
        hold = gate.armResponseHold(
          "$JS.API.CONSUMER.INFO.KV_TRELLIS_AUTH_REVOCATIONS.",
        );
        await runtime.callAdminRpc("authCapabilityGrantsGrant", {
          principalId: first.principalId,
          capabilityId: firstCapability,
          expectedRevision: 0n,
          idempotencyKey: ulid(),
        });
        const candidate = await deadline(hold.held, 90_000);
        assert(!before.has(candidate.connectionId));
        assert.equal(calls, 1);
        await runtime.callAdminRpc("authCapabilityGrantsGrant", {
          principalId: second.principalId,
          capabilityId: secondCapability,
          expectedRevision: 0n,
          idempotencyKey: ulid(),
        });
        await runtime.waitFor(async () => (await service.extend2({})).isOk(), {
          timeoutMs: 90_000,
        });
        assert.equal(calls, 1);
        // The held reply is still unavailable: newer authority was installed
        // independently. Delivering it late cannot withdraw the successor.
        await hold.release();
        hold = undefined;
        assert.deepEqual(await service.extend({}).orThrow(), {});
        assert.deepEqual(await service.extend2({}).orThrow(), {});
        release.resolve();
        assert.deepEqual(await deadline(parked, 10_000), { value: "retained" });
        assert.equal(calls, 1);
        assert.deepEqual(await caller.advance({}).orThrow(), {});
        await deadline(gate.waitForClose(candidate.connectionId), 15_000);
      } finally {
        release.resolve();
        await hold?.release();
        await parked?.catch(() => undefined);
        await caller.connection.close();
        await service.stop();
        await serviceExit;
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
