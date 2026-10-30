/** Capability growth adopts fresh transport authority without replaying accepted RPCs. */
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

test("a grown finite capability adopts a new generation without disrupting in-flight work", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const identity = await runtime.registerService({
        deployment: "finite-growth-target",
        name: "finite-growth-target",
        contract: participants.TransportGrowthTarget.participant,
      });
      const target = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: participants.TransportGrowthTarget.participant,
        seed: identity.seed,
      }).orThrow();
      const targetExit = target.wait().catch(() => undefined);
      const entered = Promise.withResolvers<void>();
      const release = Promise.withResolvers<void>();
      let advances = 0;
      let advanceDigest: string | undefined;
      let extensions = 0;
      let block = false;
      await target.handleAdvance(async ({ context }) => {
        assert.equal(context.caller.type, "verified");
        if (context.caller.type === "verified")
          advanceDigest = context.caller.authorityDigest;
        advances++;
        if (block) {
          entered.resolve();
          await release.promise;
        }
        return Result.ok({});
      });
      await target.handleExtend(() => {
        extensions++;
        return Result.ok({});
      });
      try {
        const caller = await runtime.connectClient({
          name: "finite-growth-caller",
          contract: apps.TransportGrowthCaller.request,
          timeout: 120_000,
        });
        let pending: Promise<unknown> | undefined;
        let hold:
          | ReturnType<
              ReturnType<typeof runtime.nativeTransportGate>["armResponseHold"]
            >
          | undefined;
        try {
          const capabilityId =
            "runtime-trellis.transport_growth.v1:growth.extend";
          const grants = await runtime.callAdminRpc(
            "authCapabilityGrantsList",
            { capabilityId },
          );
          assert.equal(grants.items.length, 1);
          const grant = grants.items[0];
          await runtime.callAdminRpc("authCapabilityGrantsRevoke", {
            grantId: grant.grantId,
            expectedRevision: grant.revision,
            idempotencyKey: ulid(),
          });
          await runtime.waitFor(
            () => caller.availability().capabilities[capabilityId] === false,
          );
          assert((await caller.extend({})).isErr());
          assert.equal(extensions, 0);
          assert.deepEqual(await caller.advance({}).orThrow(), {});
          const gate = runtime.nativeTransportGate();
          const advanceSubject =
            apis.transport_growth.API.actions["rpc:Advance"].subject;
          const original = gate
            .connections()
            .findLast(
              (connection) =>
                !connection.closed &&
                connection.outboundContexts.some(
                  (message) => message.subject === advanceSubject,
                ),
            );
          assert(original);
          const beforeRenewal = advanceDigest;
          assert(beforeRenewal);
          const admission = original.outboundContexts.find((message) =>
            message.subject.startsWith("trellis.auth.admission."),
          );
          assert(admission);
          hold = gate.armResponseHold(admission.subject, original.id);
          await deadline(hold.held, 90_000);
          await hold.release();
          hold = undefined;
          await runtime.waitFor(async () => {
            await caller.advance({}).orThrow();
            return advanceDigest !== beforeRenewal;
          });
          assert.equal(gate.connection(original.id)?.closed, false);
          const beforePark = advances;
          block = true;
          pending = caller.advance({}).orThrow();
          void pending.catch(() => undefined);
          await deadline(entered.promise, 10_000);
          hold = gate.armResponseHold(
            "$JS.API.CONSUMER.INFO.KV_TRELLIS_AUTH_REVOCATIONS.",
          );
          await runtime.callAdminRpc("authCapabilityGrantsGrant", {
            principalId: grant.principalId,
            capabilityId,
            expectedRevision: 0n,
            idempotencyKey: ulid(),
          });
          const candidate = await deadline(hold.held, 90_000);
          assert.notEqual(candidate.connectionId, original.id);
          assert.equal(advances, beforePark + 1);
          assert.equal(gate.connection(original.id)?.closed, false);
          await hold.release();
          hold = undefined;
          await runtime.waitFor(
            () => caller.availability().capabilities[capabilityId],
          );
          assert.deepEqual(await caller.extend({}).orThrow(), {});
          assert.equal(extensions, 1);
          assert(
            gate
              .connection(candidate.connectionId)
              ?.outboundContexts.some(
                (message) =>
                  message.subject ===
                  apis.transport_growth.API.actions["rpc:Extend"].subject,
              ),
          );
          release.resolve();
          assert.deepEqual(await deadline(pending, 10_000), {});
          assert.equal(advances, beforePark + 1);
          await deadline(gate.waitForClose(original.id), 15_000);
          assert.equal(gate.connection(candidate.connectionId)?.closed, false);
          block = false;
          assert.deepEqual(await caller.advance({}).orThrow(), {});
          assert.equal(advances, beforePark + 2);
        } finally {
          release.resolve();
          await hold?.release();
          await pending?.catch(() => undefined);
          await caller.connection.close();
        }
      } finally {
        await target.stop();
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
