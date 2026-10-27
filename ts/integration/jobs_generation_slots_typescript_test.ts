/** Logical Jobs capacity and receiving ownership across transport growth. */
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { jetstream, jetstreamManager } from "@nats-io/jetstream";
import { connect, credsAuthenticator } from "@nats-io/transport-node";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import { ulid } from "ulid";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import {
  admittedConnections,
  brokerConnectionKey,
  completeBrokerInventory,
} from "./_support/broker_inventory.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

test("managed Jobs keeps two execution slots and receives new work after generation reaping", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const contract =
        participants.TransportGrowthOperationProvider.participant;
      const instance = await runtime.registerService({
        name: "jobs-slots",
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
      const system = await connect({
        servers: runtime.natsUrl,
        authenticator: credsAuthenticator(
          await readFile(
            join(runtime.workdir, "config/trellis/nats/creds/system.creds"),
          ),
        ),
      });
      const releases = new Map<
        string,
        ReturnType<typeof Promise.withResolvers<void>>
      >();
      const started: string[] = [];
      let active = 0;
      let maximumActive = 0;
      let serviceExit: Promise<unknown> | undefined;
      const jobs = service.jobs.retained;
      await service.handleExtend(() => Result.ok({}));
      const submit = async (value: string) => {
        const release = Promise.withResolvers<void>();
        releases.set(value, release);
        return { job: await jobs.create({ value }).orThrow(), release };
      };
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
        const originalKey = brokerConnectionKey(original);
        const inventory = () =>
          completeBrokerInventory(system, {
            requiredServerIds: [original.server],
          });
        const a = await submit("A");
        const b = await submit("B");
        const c = await submit("C");
        const d = await submit("D");
        jobs.handle(
          async ({ job }) => {
            started.push(job.payload.value);
            active++;
            maximumActive = Math.max(maximumActive, active);
            try {
              const release = releases.get(job.payload.value);
              assert(release, "the broker delivered submitted work");
              await release.promise;
              return Result.ok(job.payload);
            } finally {
              active--;
            }
          },
          { concurrency: 2 },
        );
        serviceExit = service.wait().catch((error: unknown) => error);
        await runtime.waitFor(
          () => started.includes("A") && started.includes("B"),
        );
        await runtime.callAdminRpc("authCapabilityGrantsGrant", {
          principalId: grant.principalId,
          capabilityId,
          expectedRevision: 0n,
          idempotencyKey: ulid(),
        });
        await runtime.waitFor(async () => (await service.extend({})).isOk(), {
          timeoutMs: 90_000,
        });
        assert.equal((await c.job.get().orThrow()).state, "pending");
        assert.equal((await d.job.get().orThrow()).state, "pending");
        assert.equal(active, 2);
        a.release.resolve();
        assert.equal((await a.job.wait().orThrow()).state, "completed");
        await runtime.waitFor(() => started.includes("C"));
        b.release.resolve();
        assert.equal((await b.job.wait().orThrow()).state, "completed");
        await runtime.waitFor(() => started.includes("D"));
        assert.equal(active, 2);
        c.release.resolve();
        d.release.resolve();
        assert.deepEqual((await c.job.wait().orThrow()).result, { value: "C" });
        assert.deepEqual((await d.job.wait().orThrow()).result, { value: "D" });
        await runtime.waitFor(
          async () =>
            !(await inventory()).some(
              (item) => brokerConnectionKey(item) === originalKey,
            ),
          { timeoutMs: 30_000 },
        );
        const fresh = await submit("after-reap");
        await runtime.waitFor(() => started.includes("after-reap"));
        fresh.release.resolve();
        assert.deepEqual((await fresh.job.wait().orThrow()).result, {
          value: "after-reap",
        });
        assert.equal(maximumActive, 2);
        assert.deepEqual(
          [...started].sort(),
          ["A", "B", "C", "D", "after-reap"].sort(),
        );
      } finally {
        for (const release of releases.values()) release.resolve();
        await service.stop();
        await serviceExit;
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

test("managed Jobs stop interrupts withheld preparation without consuming queued work", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const contract = participants.Provider.participant;
      const instance = await runtime.registerService({
        name: "jobs-setup-stop",
        contract,
      });
      const service = await TrellisService.connect({
        trellisUrl: runtime.oauthIssuer,
        participant: contract,
        seed: instance.seed,
      }).orThrow();
      const observer = await runtime.connectNats();
      const manager = await jetstreamManager(observer);
      const gate = runtime.nativeTransportGate();
      let exited: Promise<unknown> | undefined;
      let hold: ReturnType<typeof gate.armResponseHold> | undefined;
      let handled = 0;
      try {
        const job = await service.jobs.keyedWork
          .create({ key: "queued", value: "queued" })
          .orThrow();
        service.jobs.keyedWork.handle(
          ({ job }) => {
            handled++;
            return Promise.resolve(Result.ok(job.payload));
          },
          { concurrency: 2 },
        );
        const infoPrefix = "$JS.API.CONSUMER.INFO.";
        hold = gate.armResponseHold(infoPrefix);
        exited = service.wait().catch((error: unknown) => error);
        const held = await hold.held;
        const [stream, consumerName] = held.requestSubject
          .slice(infoPrefix.length)
          .split(".");
        assert(stream && consumerName);
        let stopped = false;
        const stopping = service.stop().then(() => {
          stopped = true;
        });
        await runtime.waitFor(() => stopped, { timeoutMs: 10_000 });
        await stopping;
        await hold.release();
        hold = undefined;
        assert.equal(await exited, undefined);
        const info = await manager.consumers.info(stream, consumerName);
        assert.equal(info.num_ack_pending, 0);
        assert.equal(info.num_pending, 1);
        assert.equal(handled, 0);
        const stoppedConnection = gate.connection(held.connectionId);
        assert(stoppedConnection);
        assert.equal(
          stoppedConnection.outboundContexts.filter(
            (out) =>
              out.subject ===
              `$JS.API.CONSUMER.MSG.NEXT.${stream}.${consumerName}`,
          ).length,
          0,
        );
        const native = jetstream(observer).consumers.getConsumerFromInfo(info);
        const remaining = await native.next({ expires: 1_000 });
        assert(remaining);
        assert.equal(JSON.parse(remaining.string()).jobId, job.id);
        assert.equal(remaining.info.deliveryCount, 1);
        assert(await remaining.ackAck());
      } finally {
        await hold?.release();
        await service.stop();
        await exited;
        await observer.close();
      }
    },
    { interruptibleNativeProxy: true },
  );
});
