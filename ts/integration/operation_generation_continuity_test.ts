/** Accepted Operation execution and observation survive ordinary authority growth. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { ulid } from "ulid";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

test("accepted and fresh Operations retain control and observation across capability growth", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const contract =
        participants.TransportGrowthOperationProvider.participant;
      const instance = await runtime.registerService({
        name: "operation-growth",
        contract,
      });
      const capabilityId = "runtime-trellis.transport_growth@v1::extend";
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
        seed: instance.seed,
      }).orThrow();
      const serviceExit = service.wait().catch((error: unknown) => error);
      const release = Promise.withResolvers<void>();
      let executions = 0;
      await service.handleExtend(() => Result.ok({}));
      await service.handleHold(async ({ op }) => {
        const execution = ++executions;
        await op.started().orThrow();
        if (execution === 1) await release.promise;
        const signal = await op.nextSignal("Continue").orThrow();
        await op.acknowledgeSignal(signal.sequence).orThrow();
        return await op
          .complete({
            value: execution === 1 ? "held-complete" : "fresh-complete",
          })
          .orThrow();
      });
      const caller = await runtime.connectClient({
        name: "operation-growth-caller",
        contract: apps.TransportGrowthCaller.request,
      });
      try {
        assert((await service.extend({})).isErr());
        const held = await caller.hold({}).start().orThrow();
        const heldObservation = await held.live().orThrow();
        let heldOutput: string | undefined;
        const heldPump = (async () => {
          for await (const event of heldObservation) {
            if (event.type === "completed")
              heldOutput = event.snapshot.output?.value;
            assert(event.type !== "failed" && event.type !== "cancelled");
          }
        })().catch((error: unknown) => error);
        await runtime.waitFor(() => executions === 1);
        await runtime.callAdminRpc("authCapabilityGrantsGrant", {
          principalId: grant.principalId,
          capabilityId,
          expectedRevision: 0n,
          idempotencyKey: ulid(),
        });
        await runtime.waitFor(async () => (await service.extend({})).isOk(), {
          timeoutMs: 90_000,
        });
        await held.signal("Continue", { value: "held-complete" }).orThrow();
        const fresh = await caller.hold({}).start().orThrow();
        const freshObservation = await fresh.live().orThrow();
        let freshOutput: string | undefined;
        const freshPump = (async () => {
          for await (const event of freshObservation) {
            if (event.type === "completed")
              freshOutput = event.snapshot.output?.value;
            assert(event.type !== "failed" && event.type !== "cancelled");
          }
        })().catch((error: unknown) => error);
        await fresh.signal("Continue", { value: "fresh-complete" }).orThrow();
        assert.equal(
          (await fresh.wait().orThrow()).output?.value,
          "fresh-complete",
        );
        assert.equal(await freshPump, undefined);
        assert.equal(freshOutput, "fresh-complete");
        release.resolve();
        assert.equal(
          (await held.wait().orThrow()).output?.value,
          "held-complete",
        );
        assert.equal(await heldPump, undefined);
        assert.equal(heldOutput, "held-complete");
        assert.equal(executions, 2);
      } finally {
        release.resolve();
        await caller.connection.close();
        await service.stop();
        await serviceExit;
      }
    },
    {
      authorization: {
        // Keep a positive scheduling window before the genuine native renewal.
        contextLifetimeSeconds: 60,
        allowedClockSkewSeconds: 2,
      },
    },
  );
});
