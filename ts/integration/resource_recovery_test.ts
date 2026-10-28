import assert from "node:assert/strict";
import { codecs } from "@oatscenter/trellis/generated";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { jetstreamManager } from "@nats-io/jetstream";
import { credsAuthenticator } from "@nats-io/nats-core";
import { connect } from "@nats-io/transport-node";
import { join } from "node:path";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import { ulid } from "ulid";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

test("deployment disable and enable preserve KV data and queued work", async () => {
  await withTrellisRuntime(async (runtime) => {
    const contract = participants.Provider.participant;
    const identity = await runtime.registerService({
      name: "paused",
      contract,
    });
    let service = await TrellisService.connect({
      trellisUrl: runtime.oauthIssuer,
      participant: contract,
      seed: identity.seed,
    }).orThrow();
    let exit = service.wait().catch((error: unknown) => error);
    try {
      await service.kv.records
        .put("retained", { value: "before disable" })
        .orThrow();
      const queued = await service.jobs.work
        .create({ value: "queued before disable" })
        .orThrow();
      const current = await runtime.callAdminRpc("authDeploymentsGet", {
        deploymentId: codecs.ulid.decode(identity.deploymentId),
      });
      const disabled = await runtime.callAdminRpc("authDeploymentsDisable", {
        deploymentId: codecs.ulid.decode(identity.deploymentId),
        expectedRevision: current.deployment.revision,
        idempotencyKey: ulid(),
      });
      await service.stop();
      await exit;
      const denied = await TrellisService.connect({
        trellisUrl: runtime.oauthIssuer,
        participant: contract,
        seed: identity.seed,
      });
      assert(denied.isErr());
      await runtime.callAdminRpc("authDeploymentsEnable", {
        deploymentId: codecs.ulid.decode(identity.deploymentId),
        expectedRevision: disabled.deployment.revision,
        idempotencyKey: ulid(),
      });
      service = await TrellisService.connect({
        trellisUrl: runtime.oauthIssuer,
        participant: contract,
        seed: identity.seed,
      }).orThrow();
      let completed: string | undefined;
      service.jobs.work.handle(({ job }) => {
        if (job.ref.id === queued.id) completed = job.payload.value;
        return Promise.resolve(Result.ok(job.payload));
      });
      exit = service.wait().catch((error: unknown) => error);
      assert.deepEqual(await service.kv.records.get("retained").orThrow(), {
        value: "before disable",
      });
      assert.equal(
        await runtime.waitFor(() => completed, { timeoutMs: 20_000 }),
        "queued before disable",
      );
    } finally {
      await service.stop();
      await exit;
    }
  });
});

for (const recovery of ["apply", "restart"] as const) {
  test(`${recovery} preserves data and fails closed when an owned job consumer is missing`, async () => {
    await withTrellisRuntime(async (runtime) => {
      const contract = participants.Provider.participant;
      const identity = await runtime.registerService({
        name: "repair",
        contract,
      });
      let service = await TrellisService.connect({
        trellisUrl: runtime.oauthIssuer,
        participant: contract,
        seed: identity.seed,
      }).orThrow();
      let exit = service.wait().catch((error: unknown) => error);
      const nc = await connect({
        servers: runtime.natsUrl,
        authenticator: credsAuthenticator(
          await readFile(
            join(
              runtime.workdir,
              "config/trellis/nats/creds/trellis-auth.creds",
            ),
          ),
        ),
      });
      try {
        await service.kv.records
          .put("retained", { value: "before repair" })
          .orThrow();
        await service.jobs.work.create({ value: "must-not-run" }).orThrow();
        await service.stop();
        await exit;
        const resources = await runtime.callAdminRpc("authResourcesList", {
          ownerKind: "deployment",
          ownerId: codecs.ulid.decode(identity.deploymentId),
        });
        const work = resources.items.find(
          (item) => item.localName === "work" && item.kind === "job",
        );
        assert(work);
        const manager = await jetstreamManager(nc);
        const consumers = await manager.consumers.list("JOBS_WORK").next();
        const consumer = consumers.find((item) =>
          item.config.description?.startsWith(
            `trellis.resource.v1:${work.resourceId}:`,
          ),
        );
        assert(consumer);
        assert(await manager.consumers.delete("JOBS_WORK", consumer.name));
        if (recovery === "restart") await runtime.restart();
        else await runtime.contracts.apply({ contract });
        service = await TrellisService.connect({
          trellisUrl: runtime.oauthIssuer,
          participant: contract,
          seed: identity.seed,
        }).orThrow();
        assert.deepEqual(await service.kv.records.get("retained").orThrow(), {
          value: "before repair",
        });
        let executions = 0;
        service.jobs.work.handle(({ job }) => {
          executions++;
          return Promise.resolve(Result.ok(job.payload));
        });
        exit = service.wait().catch((error: unknown) => error);
        let failure: unknown = await runtime.waitFor(() => exit, {
          timeoutMs: 20_000,
        });
        while (
          failure instanceof Error &&
          failure.name !== "JobsConsumerMissingError" &&
          failure.cause !== undefined
        ) {
          failure = failure.cause;
        }
        assert(failure instanceof Error);
        assert.equal(failure.name, "JobsConsumerMissingError");
        assert.equal(executions, 0);
        await assert.rejects(
          manager.consumers.info("JOBS_WORK", consumer.name),
          "startup must not recreate lost delivery state",
        );
      } finally {
        await service.stop();
        try {
          await exit;
        } finally {
          await nc.close();
        }
      }
    });
  });
}
