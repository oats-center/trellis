/** RPC leases survive authority growth, then ordinary renewal reuses the new socket. */
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { connect, credsAuthenticator } from "@nats-io/transport-node";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import { ulid } from "ulid";
import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import {
  admittedConnections,
  brokerConnectionKey,
  completeBrokerInventory,
} from "./_support/broker_inventory.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

test("a service drains accepted RPCs across growth and renews in place after reaping", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const contract =
        participants.TransportGrowthOperationProvider.participant;
      const instance = await runtime.registerService({
        name: "rpc-generation",
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
        seed: instance.seed,
      }).orThrow();
      const serviceExit = service.wait().catch((error: unknown) => error);
      const caller = await runtime.connectClient({
        name: "rpc-generation-caller",
        contract: apps.TransportGrowthCaller.request,
        timeout: 120_000,
      });
      const system = await connect({
        servers: runtime.natsUrl,
        authenticator: credsAuthenticator(
          await readFile(
            join(runtime.workdir, "config/trellis/nats/creds/system.creds"),
          ),
        ),
      });
      const release = Promise.withResolvers<void>();
      let held: Promise<unknown> | undefined;
      let executions = 0;
      let selfDigest: string | undefined;
      await service.handleExtend(({ context }) => {
        assert(
          context.caller.type === "verified",
          "expected an authenticated self-call",
        );
        selfDigest = context.caller.authorityDigest;
        return Result.ok({});
      });
      await service.handlePark(async () => {
        const execution = ++executions;
        if (execution === 1) await release.promise;
        return Result.ok({ value: execution === 1 ? "held" : "fresh" });
      });
      const attachments = () =>
        runtime.callAdminRpc("authConnectionsList", {
          principalId: grant.principalId,
          state: "admitted",
        });
      try {
        const initial = (await attachments()).items;
        assert.equal(initial.length, 1);
        const [original] = admittedConnections(
          await completeBrokerInventory(system),
          new Set([initial[0].attachmentId]),
        );
        assert(original);
        const oldKey = brokerConnectionKey(original);
        const inventory = () =>
          completeBrokerInventory(system, {
            requiredServerIds: [original.server],
            onlyRequiredServers: true,
          });
        held = caller.park({}).orThrow();
        void held.catch(() => undefined);
        await runtime.waitFor(() => executions === 1);
        await runtime.callAdminRpc("authCapabilityGrantsGrant", {
          principalId: grant.principalId,
          capabilityId,
          expectedRevision: 0n,
          idempotencyKey: ulid(),
        });
        const grown = await runtime.waitFor(
          async () => {
            const current = (await attachments()).items;
            return current.length >= 2 ? current : undefined;
          },
          { timeoutMs: 90_000 },
        );
        assert(
          grown.some((item) => item.attachmentId === initial[0].attachmentId),
        );
        await runtime.waitFor(async () => (await service.extend({})).isOk());
        assert.deepEqual(await caller.park({}).orThrow(), { value: "fresh" });
        release.resolve();
        assert.deepEqual(await held, { value: "held" });
        assert.equal(executions, 2);
        await runtime.waitFor(
          async () =>
            !(await inventory()).some(
              (item) => brokerConnectionKey(item) === oldKey,
            ),
          { timeoutMs: 30_000 },
        );
        const current = (await attachments()).items;
        assert.equal(current.length, 1);
        assert.notEqual(current[0].attachmentId, initial[0].attachmentId);
        const digestBeforeRenewal = selfDigest;
        assert(digestBeforeRenewal);
        await runtime.waitFor(
          async () => {
            await service.extend({}).orThrow();
            return selfDigest !== digestBeforeRenewal;
          },
          { timeoutMs: 90_000 },
        );
        assert.deepEqual(
          (await attachments()).items.map((item) => item.attachmentId),
          [current[0].attachmentId],
        );
        assert.equal(
          admittedConnections(
            await inventory(),
            new Set([current[0].attachmentId]),
          ).length,
          1,
        );
      } finally {
        release.resolve();
        await held?.catch(() => undefined);
        await caller.connection.close();
        await service.stop();
        await serviceExit;
        await system.close();
      }
    },
    {
      authorization: {
        contextLifetimeSeconds: 60,
        allowedClockSkewSeconds: 2,
      },
    },
  );
});
