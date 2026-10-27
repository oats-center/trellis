import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { join } from "node:path";
import { connect, credsAuthenticator } from "@nats-io/transport-node";
import { TrellisService } from "@oatscenter/trellis/service";
import { Result } from "@oatscenter/trellis";
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

test("service health survives generation reaping and stop fences sampling", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const contract =
        participants.TransportGrowthOperationProvider.participant;
      const instance = await runtime.registerService({
        name: "health-generation-provider",
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
        name: "health-generation-provider",
        seed: instance.seed,
        runtime: { health: { publishIntervalMs: 1000 } },
      }).orThrow();
      await service.handleExtend(() => Result.ok({}));
      const attachments = () =>
        runtime.callAdminRpc("authConnectionsList", {
          principalId: grant.principalId,
          state: "admitted",
        });
      const system = await connect({
        servers: runtime.natsUrl,
        authenticator: credsAuthenticator(
          await readFile(
            join(runtime.workdir, "config/trellis/nats/creds/system.creds"),
          ),
        ),
      });
      let releaseSample = Promise.withResolvers<void>();
      try {
        const consoleClient = await runtime.connectClient({
          name: "health-generation-console",
          contract: apps.Observer.request,
        });
        const inspect = async () => {
          const result = await consoleClient
            .healthInspect({
              contractId: contract.identity,
              participantKind: "service",
            })
            .orThrow();
          return result.instances.find(
            (item) => item.instanceId === instance.instanceId,
          )?.latestSample;
        };
        const baseline = await runtime.waitFor(
          async () => (await inspect()) ?? false,
        );
        const [original] = (await attachments()).items;
        assert(original);
        const originalConnections = admittedConnections(
          await completeBrokerInventory(system),
          new Set([original.attachmentId]),
        );
        assert.deepEqual(originalConnections.length, 1);
        const [g1] = originalConnections;
        assert(g1);
        assert.deepEqual(g1.server, system.info?.server_id);
        const g1Key = brokerConnectionKey(g1);
        const inventory = () =>
          completeBrokerInventory(system, {
            requiredServerIds: [g1.server],
            onlyRequiredServers: true,
          });

        // An actual application health snapshot holds the finite publishing lease
        // while an ordinary capability grant supersedes its generation.
        let activeSamples = 0;
        let maxActiveSamples = 0;
        service.health.setInfo(async () => {
          activeSamples++;
          maxActiveSamples = Math.max(maxActiveSamples, activeSamples);
          await releaseSample.promise;
          activeSamples--;
          return { info: { marker: "held-baseline" } };
        });
        await runtime.waitFor(() => activeSamples > 0);
        await runtime.callAdminRpc("authCapabilityGrantsGrant", {
          principalId: grant.principalId,
          capabilityId,
          expectedRevision: 0n,
          idempotencyKey: ulid(),
        });
        await runtime.waitFor(
          async () => {
            const current = await attachments();
            const connections = await inventory();
            return (
              connections.some((item) => brokerConnectionKey(item) === g1Key) &&
              admittedConnections(
                connections,
                new Set(
                  current.items
                    .filter(
                      (item) => item.attachmentId !== original.attachmentId,
                    )
                    .map((item) => item.attachmentId),
                ),
              ).length > 0
            );
          },
          { timeoutMs: 90_000 },
        );
        releaseSample.resolve();
        await runtime.waitFor(
          async () =>
            !(await inventory()).some(
              (item) => brokerConnectionKey(item) === g1Key,
            ),
          { timeoutMs: 30_000 },
        );

        // This marker is created only after the exact server:cid is absent.
        const marker = `after-reap-${ulid()}`;
        service.health.setInfo({ info: { marker } });
        const fresh = await runtime.waitFor(
          async () => {
            const sample = await inspect();
            return sample?.participant.info?.marker === marker ? sample : false;
          },
          { timeoutMs: 10_000 },
        );
        assert(fresh.sample.id !== baseline.sample.id);
        assert.deepEqual(
          fresh.checks.find((check) => check.name === "nats")?.status,
          "ok",
        );
        assert.deepEqual(maxActiveSamples, 1);

        // Stop must wait for the existing snapshot, but must not publish it or
        // begin another one. Read persisted projection repeatedly past several
        // configured heartbeat periods rather than trusting timer cancellation.
        releaseSample = Promise.withResolvers<void>();
        let stoppingSampleStarted = false;
        service.health.setInfo(async () => {
          stoppingSampleStarted = true;
          await releaseSample.promise;
          return { info: { marker: "must-not-publish-after-stop" } };
        });
        await runtime.waitFor(() => stoppingSampleStarted);
        let stopped = false;
        const stop = service.stop().then(() => {
          stopped = true;
        });
        await inspect();
        assert.deepEqual(stopped, false);
        releaseSample.resolve();
        await stop;
        const stoppedSample = await inspect();
        assert(stoppedSample);
        const observationDeadline = Date.now() + 3000;
        await runtime.waitFor(async () => {
          assert.deepEqual(
            (await inspect())?.sample.id,
            stoppedSample.sample.id,
          );
          return Date.now() >= observationDeadline;
        });
        assert.deepEqual(stoppedSample.participant.info?.marker, marker);
      } finally {
        releaseSample.resolve();
        await service.stop();
        await system.close();
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
