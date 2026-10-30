/** Failed candidate mirrors cannot withdraw a healthy predecessor or disable revocation. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { deadline } from "@nats-io/nats-core";
import { jetstreamManager } from "@nats-io/jetstream";
import { Result } from "@oatscenter/trellis";
import { codecs } from "@oatscenter/trellis/generated";
import { TrellisService } from "@oatscenter/trellis/service";
import { ulid } from "ulid";
import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

test("failed provisional mirror preserves live delivery and real revocation enforcement", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const targetIdentity = await runtime.registerService({
        deployment: "failure-target",
        name: "failure-target",
        contract: participants.TransportGrowthTarget.participant,
      });
      const target = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: participants.TransportGrowthTarget.participant,
        seed: targetIdentity.seed,
      }).orThrow();
      await target.handleAdvance(() => Result.ok({}));
      await target.handleExtend(() => Result.ok({}));
      const targetExit = target.wait().catch((error: unknown) => error);
      const contract = participants.TransportGrowthSubject.participant;
      const identity = await runtime.registerService({
        deployment: "failure-subject",
        name: "failure-subject",
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
        trellisUrl: runtime.trellisUrl,
        participant: contract,
        seed: identity.seed,
      }).orThrow();
      const serviceExit = service.wait().catch((error: unknown) => error);
      const broker = await runtime.connectNats();
      const manager = await jetstreamManager(broker);
      const caller = await runtime.connectClient({
        name: "failure-observer",
        contract: apps.LiveProbeCaller.request,
      });
      const sendAfterFailure = Promise.withResolvers<void>();
      let cleanups = 0;
      await service.handleWatch(async ({ input, emit, signal }) => {
        try {
          await emit({
            ...input,
            sourceGeneration: 1n,
            index: 1n,
            payload: new Uint8Array([1]),
            padding: "before",
          }).orThrow();
          await Promise.race([
            sendAfterFailure.promise,
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
            sourceGeneration: 1n,
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
      const gate = runtime.nativeTransportGate();
      let hold: ReturnType<typeof gate.armResponseHold> | undefined;
      try {
        const feed = await caller
          .watch({ runId: ulid(), streamId: "candidate-failure" })
          .orThrow();
        const frames: bigint[] = [];
        const drain = (async () => {
          for await (const frame of feed) frames.push(frame.index);
        })().catch((error: unknown) => error);
        await runtime.waitFor(() => frames.length === 1);
        const infoPrefix = "$JS.API.CONSUMER.INFO.KV_TRELLIS_AUTH_REVOCATIONS.";
        hold = gate.armResponseHold(infoPrefix);
        await runtime.callAdminRpc("authCapabilityGrantsGrant", {
          principalId: grant.principalId,
          capabilityId,
          expectedRevision: 0n,
          idempotencyKey: ulid(),
        });
        const candidate = await deadline(hold.held, 90_000);
        const consumer = candidate.requestSubject.slice(infoPrefix.length);
        assert(consumer);
        // Delete only this ephemeral candidate watcher, not committed application
        // infrastructure. Its real INFO reply stays withheld during the failure.
        assert(
          await manager.consumers.delete(
            "KV_TRELLIS_AUTH_REVOCATIONS",
            consumer,
          ),
        );
        await deadline(gate.waitForClose(candidate.connectionId), 15_000);
        await hold.release();
        hold = undefined;
        sendAfterFailure.resolve();
        await runtime.waitFor(() => frames.length === 2);
        assert.deepEqual(frames, [1n, 2n]);
        assert.equal(cleanups, 0);
        await runtime.waitFor(async () => (await service.extend({})).isOk(), {
          timeoutMs: 90_000,
        });
        assert.equal(cleanups, 0);
        const loginSessionId = caller.ownAuthority.current?.loginSessionId;
        assert(loginSessionId);
        await runtime.callAdminRpc("authSessionsRevoke", {
          loginSessionId: codecs.ulid.decode(loginSessionId),
          idempotencyKey: ulid(),
        });
        await deadline(drain, 20_000);
        await runtime.waitFor(() => cleanups === 1);
        assert.deepEqual(frames, [1n, 2n]);
      } finally {
        sendAfterFailure.resolve();
        await hold?.release();
        await caller.connection.close();
        await service.stop();
        await target.stop();
        await serviceExit;
        await targetExit;
        await broker.close();
      }
    },
    {
      authorization: {
        contextLifetimeSeconds: 30,
        allowedClockSkewSeconds: 2,
      },
      interruptibleNativeProxy: true,
    },
  );
});
