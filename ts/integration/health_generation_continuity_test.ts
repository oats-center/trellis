import { assert, assertEquals } from "@std/assert";
import { join } from "@std/path";
import { connect, credsAuthenticator } from "@nats-io/transport-node";
import { TrellisService } from "@oatscenter/trellis/service";
import { participants as webParticipants } from "trellis-web-generated";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import {
  admittedConnections,
  brokerConnectionKey,
  completeBrokerInventory,
} from "./_support/broker_inventory.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

Deno.test("service health survives generation reaping and stop fences sampling", async () => {
  await withTrellisRuntime(async (runtime) => {
    const contract = participants.Provider.participant;
    await runtime.contracts.install({ contract });
    const requested = await runtime.contracts.requestApply({ contract });
    if (requested.status === "approval_required") {
      await runtime.contracts.approveApply(requested.pendingId, {
        excludeResources: ["extras"],
      });
    }
    const instance = await runtime.services.createInstance({
      name: "health-generation-provider",
      contract,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: contract,
      name: "health-generation-provider",
      seed: instance.seed,
      runtime: { health: { publishIntervalMs: 1000 } },
    }).orThrow();
    const system = await connect({
      servers: runtime.natsUrl,
      authenticator: credsAuthenticator(
        await Deno.readFile(
          join(runtime.workdir, "config/trellis/nats/creds/system.creds"),
        ),
      ),
    });
    let releaseSample = Promise.withResolvers<void>();
    try {
      const consoleClient = await runtime.connectClient({
        name: "health-generation-console",
        contract: webParticipants.Console.participant,
      });
      const inspect = async () => {
        const result = await consoleClient.healthInspect({
          contractId: contract.identity,
          participantKind: "service",
        }).orThrow();
        return result.instances.find((item) =>
          item.instanceId === instance.instanceId
        )?.latestSample;
      };
      const baseline = await runtime.waitFor(async () =>
        await inspect() ?? false
      );
      const attachments = await runtime.callAdminRpc(
        "authConnectionsList",
        {},
      ) as {
        items: { participantId: string; contextDigest: string }[];
      };
      const original = attachments.items.find((item) =>
        item.participantId === contract.identity
      );
      assert(original);
      const originalConnections = admittedConnections(
        await completeBrokerInventory(system),
        new Set([original.contextDigest]),
      );
      assertEquals(originalConnections.length, 1);
      const [g1] = originalConnections;
      assert(g1);
      assertEquals(g1.server, system.info?.server_id);
      const g1Key = brokerConnectionKey(g1);
      const inventory = () =>
        completeBrokerInventory(system, {
          requiredServerIds: [g1.server],
        });

      // An actual application health snapshot holds the finite publishing lease
      // while real consent growth supersedes its generation.
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
      await runtime.contracts.apply({ contract });
      await runtime.waitFor(async () => {
        const current = await runtime.callAdminRpc(
          "authConnectionsList",
          {},
        ) as {
          items: { participantId: string; contextDigest: string }[];
        };
        const connections = await inventory();
        return connections.some((item) =>
          brokerConnectionKey(item) === g1Key
        ) &&
          admittedConnections(
              connections,
              new Set(
                current.items.filter((item) =>
                  item.participantId === contract.identity &&
                  item.contextDigest !== original.contextDigest
                ).map((item) => item.contextDigest),
              ),
            ).length > 0;
      }, { timeoutMs: 90_000 });
      releaseSample.resolve();
      await runtime.waitFor(
        async () =>
          !(await inventory()).some((item) =>
            brokerConnectionKey(item) === g1Key
          ),
        { timeoutMs: 30_000 },
      );

      // This marker is created only after the exact server:cid is absent.
      const marker = `after-reap-${crypto.randomUUID()}`;
      service.health.setInfo({ info: { marker } });
      const fresh = await runtime.waitFor(async () => {
        const sample = await inspect();
        return sample?.participant.info?.marker === marker ? sample : false;
      }, { timeoutMs: 10_000 });
      assert(fresh.sample.id !== baseline.sample.id);
      assertEquals(
        fresh.checks.find((check) => check.name === "nats")?.status,
        "ok",
      );
      assertEquals(maxActiveSamples, 1);

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
      await consoleClient.healthQuery({}).orThrow();
      assertEquals(stopped, false);
      releaseSample.resolve();
      await stop;
      const stoppedSample = await inspect();
      assert(stoppedSample);
      const observationDeadline = Date.now() + 3000;
      await runtime.waitFor(async () => {
        assertEquals((await inspect())?.sample.id, stoppedSample.sample.id);
        return Date.now() >= observationDeadline;
      });
      assertEquals(stoppedSample.participant.info?.marker, marker);
    } finally {
      releaseSample.resolve();
      await service.stop();
      await system.close();
    }
  }, {
    authorization: {
      contextLifetimeSeconds: 76,
      refreshLeadSeconds: 15,
      refreshJitterSeconds: 0,
      minimumContextLifetimeSeconds: 46,
    },
  });
});
