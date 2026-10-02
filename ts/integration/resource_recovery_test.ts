import { assert, assertEquals } from "@std/assert";
import { jetstreamManager } from "@nats-io/jetstream";
import { credsAuthenticator } from "@nats-io/nats-core";
import { connect } from "@nats-io/transport-node";
import { join } from "@std/path";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import { ulid } from "ulid";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import {
  parsePidFile,
  processIsGone,
  processMatchesIdentity,
} from "../packages/trellis-testkit/src/cleanup.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

for (const recovery of ["apply", "restart"] as const) {
  Deno.test(`${recovery} repairs a deleted owned job consumer without resetting retained data`, async () => {
    await withTrellisRuntime(async (runtime) => {
      const contract = participants.Provider.participant;
      const identity = await runtime.registerService({
        name: "repair",
        contract,
      });
      let service = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: contract,
        seed: identity.seed,
      }).orThrow();
      let exit = service.wait().catch((error: unknown) => error);
      const nc = await connect({
        servers: runtime.natsUrl,
        authenticator: credsAuthenticator(
          await Deno.readFile(
            join(runtime.workdir, "nats/creds/trellis-auth.creds"),
          ),
        ),
      });
      try {
        await service.kv.records.put("retained", { value: "before repair" })
          .orThrow();
        await service.stop();
        await exit;
        const detail = await runtime.callAdminRpc("authDeploymentsGet", {
          deploymentId: identity.deploymentId,
        });
        const work = detail.resources.find((item) =>
          item.localName === "work" && item.resourceKind === "jobQueue"
        );
        assert(work);
        const manager = await jetstreamManager(nc);
        const consumers = await manager.consumers.list("JOBS_WORK").next();
        const consumer = consumers.find((item) =>
          item.config.metadata?.["trellis.resource_id"] === work.bindingId
        );
        assert(consumer);
        assert(await manager.consumers.delete("JOBS_WORK", consumer.name));
        if (recovery === "restart") await runtime.restartControlPlane();
        else await runtime.contracts.apply({ contract });
        await runtime.waitFor(async () => {
          const current = await manager.consumers.list("JOBS_WORK").next();
          return current.some((item) => item.name === consumer.name);
        }, { timeoutMs: 20_000 });
        service = await TrellisService.connect({
          trellisUrl: runtime.trellisUrl,
          participant: contract,
          seed: identity.seed,
        }).orThrow();
        service.jobs.work.handle(({ job }) =>
          Promise.resolve(Result.ok(job.payload))
        );
        exit = service.wait().catch((error: unknown) => error);
        assertEquals(await service.kv.records.get("retained").orThrow(), {
          value: "before repair",
        });
        const job = await service.jobs.work.create({ value: "after repair" })
          .orThrow();
        const terminal = await job.wait().orThrow();
        assertEquals(terminal.state, "completed", terminal.lastError);
        assertEquals(terminal.result, { value: "after repair" });
      } finally {
        await service.stop();
        await exit;
        await nc.close();
      }
    });
  });
}

Deno.test("resource reapproval progresses after broker restart with historical transport enforcement pending", async () => {
  await withTrellisRuntime(async (runtime) => {
    const contract = participants.Provider.participant;
    const identity = await runtime.registerService({
      name: "broker-repair",
      contract,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: contract,
      seed: identity.seed,
    }).orThrow();
    const exit = service.wait().catch((error: unknown) => error);
    assert(service.kv.extras);
    await service.kv.extras.put("retained", { value: "original" }).orThrow();
    const selector = {
      ownerKind: "deployment",
      ownerId: identity.deploymentId,
      participantId: contract.identity,
    };
    const { binding } = await runtime.callAdminRpc("authGrantsGet", selector);
    assert(binding);
    const before = await runtime.callAdminRpc("authConnectionsList", {});
    const historical = before.items.filter((item) =>
      item.participantId === contract.identity
    );
    assert(historical.length > 0);
    const natsDir = join(runtime.workdir, "nats");
    let replacement: Deno.ChildProcess | undefined;
    let replacementOutput: Promise<Deno.CommandOutput> | undefined;
    let repaired: typeof service | undefined;
    // This fixture owns the exact child named by its process-identity file.
    // SIGKILL prevents a broker disconnect event from erasing the real presence.
    const pidFiles: string[] = [];
    for await (const entry of Deno.readDir(natsDir)) {
      if (entry.name.startsWith("nats-") && entry.name.endsWith(".pid")) {
        pidFiles.push(entry.name);
      }
    }
    assertEquals(pidFiles.length, 1);
    const broker = parsePidFile(
      await Deno.readTextFile(join(natsDir, pidFiles[0])),
    );
    assert(broker);
    assert(await processMatchesIdentity(broker));
    try {
      Deno.kill(broker.pid, "SIGKILL");
      await runtime.waitFor(() => processIsGone(broker.pid));
      await service.stop();
      await exit;
      replacement = new Deno.Command(broker.executable, {
        args: ["-c", join(natsDir, "nats.conf")],
        cwd: natsDir,
        stdin: "null",
        stdout: "piped",
        stderr: "piped",
      }).spawn();
      replacementOutput = replacement.output();
      // A normal admin RPC is the readiness barrier; no test-only runtime path.
      await runtime.waitFor(async () => {
        try {
          return await runtime.callAdminRpc("authConnectionsList", {});
        } catch {
          return false;
        }
      }, { timeoutMs: 30_000 });
      await runtime.restartControlPlane();
      const present = await runtime.callAdminRpc("authConnectionsList", {});
      assert(
        historical.some((old) =>
          present.items.some((item) => item.connectionId === old.connectionId)
        ),
      );
      const reduced = await runtime.callAdminRpc("authGrantsSet", {
        ...selector,
        expectedRevision: binding.revision,
        installedRevision: binding.installedRevision,
        expiresAt: binding.expiresAt,
        platformPrivileges: binding.platformPrivileges,
        idempotencyKey: ulid(),
        grants: {
          ...binding.grants,
          permissions: binding.grants.permissions.filter((atom) => {
            const target: unknown = JSON.parse(
              new TextDecoder().decode(atom.target),
            );
            return !(typeof target === "object" && target !== null &&
              "kind" in target &&
              target.kind === "participantResource" && "name" in target &&
              target.name === "extras");
          }),
        },
      });
      assert(
        reduced.binding.grants.permissions.length <
          binding.grants.permissions.length,
      );
      await runtime.waitFor(() => {
        const output = runtime.controlPlaneOutput().toLowerCase();
        return output.includes("transportreevaluate") &&
          output.includes("no responders");
      }, { timeoutMs: 20_000 });
      await runtime.callAdminRpc("authGrantsSet", {
        ...selector,
        expectedRevision: reduced.binding.revision,
        installedRevision: binding.installedRevision,
        expiresAt: binding.expiresAt,
        platformPrivileges: binding.platformPrivileges,
        idempotencyKey: ulid(),
        grants: binding.grants,
      });
      await runtime.waitFor(async () => {
        const detail = await runtime.callAdminRpc("authDeploymentsGet", {
          deploymentId: identity.deploymentId,
        });
        return detail.resources.some((resource) =>
          resource.localName === "extras" && resource.state === "available"
        );
      }, { timeoutMs: 20_000 });
      repaired = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: contract,
        seed: identity.seed,
      }).orThrow();
      assert(repaired.kv.extras);
      assertEquals(await repaired.kv.extras.get("retained").orThrow(), {
        value: "original",
      });
      repaired.jobs.work.handle(({ job }) =>
        Promise.resolve(Result.ok(job.payload))
      );
      const repairedExit = repaired.wait().catch((error: unknown) => error);
      try {
        const job = await repaired.jobs.work.create({
          value: "broker recovered",
        }).orThrow();
        assertEquals((await job.wait().orThrow()).state, "completed");
        await runtime.restartControlPlane();
        assertEquals(await repaired.kv.extras.get("retained").orThrow(), {
          value: "original",
        });
      } finally {
        await repaired.stop();
        await repairedExit;
      }
    } finally {
      await repaired?.stop();
      await service.stop();
      await exit;
      if (replacement) replacement.kill("SIGTERM");
      await replacementOutput;
    }
  });
});
