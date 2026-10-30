import assert from "node:assert/strict";
import { codecs } from "@oatscenter/trellis/generated";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import { jetstreamManager } from "@nats-io/jetstream";
import { ulid } from "ulid";
import { API as authApi } from "../packages/trellis-testkit/trellis/apis/auth/mod.js";
import { types } from "../packages/trellis-testkit/trellis/index.js";
import { test } from "node:test";
import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { participants as pinnedParticipants } from "../../integration/fixtures/runtime-pinned/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

test("identical deployment apply retains keyed Jobs and durable Events evidence", async () => {
  await withTrellisRuntime(async (runtime) => {
    const contract = participants.Provider.participant;
    const identity = await runtime.registerService({
      name: "unchanged-owner",
      contract,
    });
    let service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: contract,
      seed: identity.seed,
    }).orThrow();
    const jobs = await runtime.connectClient({
      name: "unchanged-jobs",
      contract: apps.JobEvidenceReader.request,
    });
    const events = await runtime.connectClient({
      name: "unchanged-events",
      contract: apps.EventEvidenceReader.request,
    });
    const admin = await runtime.connectClient({
      name: "unchanged-key-reader",
      contract: apps.JobsAdmin.request,
    });
    const broker = await runtime.connectNats();
    const manager = await jetstreamManager(broker);
    const release = Promise.withResolvers<void>();
    const started = Promise.withResolvers<void>();
    service.jobs.keyedWork.handle(async ({ job }) => {
      started.resolve();
      await release.promise;
      return Result.ok(job.payload);
    });
    const received: string[] = [];
    await service
      .onChanged(({ event }) => {
        received.push(event.value);
        return Promise.resolve();
      })
      .orThrow();
    let exit = service.wait().catch((error: unknown) => error);
    const ownerId = codecs.ulid.decode(identity.deploymentId);
    try {
      await service.kv.records
        .put("retained", { value: "unchanged data" })
        .orThrow();
      const active = await service.jobs.keyedWork
        .create({ key: "retained-key", value: "active" })
        .orThrow();
      await runtime.waitFor(() => started.promise.then(() => true));
      const queued = await service.jobs.keyedWork
        .create({ key: "retained-key", value: "queued" })
        .orThrow();
      await service.publishChanged({ value: "before apply" }).orThrow();
      await runtime.waitFor(() => received.includes("before apply"));
      const before = await events
        .resourceBindingsQuery({ kind: "eventConsumer", ownerId })
        .orThrow();
      const consumer = before.items[0].eventConsumer;
      assert(consumer);
      const progress = await runtime.waitFor(async () => {
        const info = await manager.consumers.info(
          consumer.stream,
          consumer.consumer,
        );
        return info.ack_floor.stream_seq > 0 ? info : undefined;
      });
      const current = await runtime.callAdminRpc("authDeploymentsGet", {
        deploymentId: ownerId,
      });
      const applied = await runtime.callAdminRpc("authDeploymentsApply", {
        deploymentId: ownerId,
        expectedRevision: current.deployment.revision,
        packageEvidence: types.AuthPackageEvidenceCodec.decode(
          contract.packageEvidence,
        ),
        idempotencyKey: ulid(),
      });
      assert(applied.deployment.revision > current.deployment.revision);
      const queues = await jobs
        .resourceBindingsQuery({ kind: "jobQueue", ownerId })
        .orThrow();
      assert(queues.items.some((row) => row.localName === "keyedWork"));
      assert(
        queues.items.every(
          (row) =>
            row.resourceBindingsDigest ===
            before.items[0].resourceBindingsDigest,
        ),
      );
      const after = await events
        .resourceBindingsQuery({ kind: "eventConsumer", ownerId })
        .orThrow();
      assert.deepEqual(after.items, before.items);
      const key = await runtime.waitFor(async () => {
        const snapshot = await admin
          .getKey({
            service: contract.identity,
            type: "keyedWork",
            key: "retained-key",
          })
          .orThrow();
        return snapshot.active.some((entry) => entry.jobId === active.id) &&
          snapshot.queued.some((entry) => entry.jobId === queued.id)
          ? snapshot
          : undefined;
      });
      assert.equal(key.active.length, 1);
      assert.equal((await active.get().orThrow()).payload.value, "active");
      assert.equal((await queued.get().orThrow()).payload.value, "queued");
      release.resolve();
      await active.wait().orThrow();
      await queued.wait().orThrow();
      await service.stop();
      await exit;
      service = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: contract,
        seed: identity.seed,
      }).orThrow();
      await service
        .onChanged(({ event }) => {
          received.push(event.value);
          return Promise.resolve();
        })
        .orThrow();
      exit = service.wait().catch((error: unknown) => error);
      assert.deepEqual(await service.kv.records.get("retained").orThrow(), {
        value: "unchanged data",
      });
      const retained = await manager.consumers.info(
        consumer.stream,
        consumer.consumer,
      );
      assert(retained.ack_floor.stream_seq >= progress.ack_floor.stream_seq);
      await service.publishChanged({ value: "after apply" }).orThrow();
      await runtime.waitFor(() => received.includes("after apply"));
      assert.deepEqual(received, ["before apply", "after apply"]);

      // A newly required resource has not been materialized. Even a subsequent
      // identical apply must not make the older ready allocations look current.
      await service.stop();
      await exit;
      const changed = await runtime.callAdminRpc("authDeploymentsApply", {
        deploymentId: ownerId,
        expectedRevision: applied.deployment.revision,
        packageEvidence: types.AuthPackageEvidenceCodec.decode(
          pinnedParticipants.Provider.participant.packageEvidence,
        ),
        idempotencyKey: ulid(),
      });
      await runtime.callAdminRpc("authDeploymentsApply", {
        deploymentId: ownerId,
        expectedRevision: changed.deployment.revision,
        packageEvidence: types.AuthPackageEvidenceCodec.decode(
          pinnedParticipants.Provider.participant.packageEvidence,
        ),
        idempotencyKey: ulid(),
      });
      const detachedJobs = await jobs
        .resourceBindingsQuery({ kind: "jobQueue", ownerId })
        .orThrow();
      const detachedEvents = await events
        .resourceBindingsQuery({ kind: "eventConsumer", ownerId })
        .orThrow();
      assert(detachedJobs.items.length > 0);
      assert(detachedEvents.items.length > 0);
      assert(
        [...detachedJobs.items, ...detachedEvents.items].every(
          (row) => row.resourceBindingsDigest === undefined,
        ),
      );
    } finally {
      release.resolve();
      await service.stop();
      await exit;
      await jobs.connection.close();
      await events.connection.close();
      await admin.connection.close();
      await broker.close();
    }
  });
});

test("resource evidence is paginated, current, and isolated by Jobs/Events capability", async () => {
  await withTrellisRuntime(async (runtime) => {
    const key = await runtime.registerService({
      name: "evidence-owner",
      contract: participants.Provider.participant,
    });
    const jobs = await runtime.connectClient({
      name: "job-evidence",
      contract: apps.JobEvidenceReader.request,
    });
    const events = await runtime.connectClient({
      name: "event-evidence",
      contract: apps.EventEvidenceReader.request,
    });
    try {
      const first = await jobs
        .resourceBindingsQuery({
          kind: "jobQueue",
          ownerId: codecs.ulid.decode(key.deploymentId),
          page: { limit: 1 },
        })
        .orThrow();
      assert.equal(first.items.length, 1);
      assert(first.page.nextCursor);
      assert(first.items[0].job?.publishPrefix);
      assert(first.items[0].resourceBindingsDigest);
      const second = await jobs
        .resourceBindingsQuery({
          kind: "jobQueue",
          ownerId: codecs.ulid.decode(key.deploymentId),
          page: { limit: 1, cursor: first.page.nextCursor },
        })
        .orThrow();
      assert.equal(second.items.length, 1);
      assert.notEqual(second.items[0].resourceId, first.items[0].resourceId);
      const consumer = await events
        .resourceBindingsQuery({
          kind: "eventConsumer",
          ownerId: codecs.ulid.decode(key.deploymentId),
        })
        .orThrow();
      assert.equal(consumer.items.length, 1);
      assert.equal(consumer.items[0].localName, "changes");
      assert(consumer.items[0].eventConsumer?.consumer);
      assert.equal(
        consumer.items[0].resourceBindingsDigest,
        first.items[0].resourceBindingsDigest,
      );
      assert.equal(
        (await jobs.resourceBindingsQuery({ kind: "eventConsumer" })).isErr(),
        true,
      );
      assert.equal(
        (await events.resourceBindingsQuery({ kind: "jobQueue" })).isErr(),
        true,
      );
      const { deployment } = await runtime.callAdminRpc("authDeploymentsGet", {
        deploymentId: codecs.ulid.decode(key.deploymentId),
      });
      await runtime.callAdminRpc(
        "authDeploymentsDisable",
        authApi.actions["rpc:Deployments.Disable"].input.decode({
          deploymentId: key.deploymentId,
          expectedRevision: deployment.revision.toString(),
          idempotencyKey: ulid(),
        }),
      );
      const disabled = await jobs
        .resourceBindingsQuery({
          kind: "jobQueue",
          ownerId: codecs.ulid.decode(key.deploymentId),
        })
        .orThrow();
      assert(
        disabled.items.length > 0,
        "retained evidence must remain discoverable",
      );
      assert(
        disabled.items.every((row) => row.resourceBindingsDigest === undefined),
        "disabled deployment evidence cannot authorize current resource use",
      );
    } finally {
      await jobs.connection.close();
      await events.connection.close();
    }
  });
});
