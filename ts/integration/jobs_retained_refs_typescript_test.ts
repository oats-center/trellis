/** Retained JobRefs resolve against the current carrier after their original socket is reaped. */
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
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

test("retained Jobs refs get, wait and cancel after broker-confirmed original socket reaping", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const contract =
        participants.TransportGrowthOperationProvider.participant;
      const instance = await runtime.registerService({
        name: "retained-jobs",
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
      const release = Promise.withResolvers<void>();
      let started = false;
      let cancellationObserved = false;
      let executions = 0;
      const jobs = service.jobs.retained;
      await service.handleExtend(() => Result.ok({}));
      jobs.handle(
        async ({ job }) => {
          executions++;
          if (job.payload.value === "held") {
            started = true;
            await release.promise;
          } else if (job.payload.value === "cancel") {
            await new Promise<void>((resolve) => {
              const abort = () => {
                cancellationObserved = true;
                resolve();
              };
              job.signal.addEventListener("abort", abort, { once: true });
              if (job.signal.aborted) abort();
            });
          }
          return Result.ok({ value: `result:${job.payload.value}` });
        },
        { concurrency: 2 },
      );
      const serviceExit = service.wait().catch((error: unknown) => error);
      const attachments = () =>
        runtime.callAdminRpc("authConnectionsList", {
          principalId: grant.principalId,
          state: "admitted",
        });
      let waiting: Promise<unknown> | undefined;
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
        const held = await jobs.create({ value: "held" }).orThrow();
        await runtime.waitFor(() => started);
        const heldWait = held.wait().orThrow();
        waiting = heldWait;
        void waiting.catch(() => undefined);
        await runtime.callAdminRpc("authCapabilityGrantsGrant", {
          principalId: grant.principalId,
          capabilityId,
          expectedRevision: 0n,
          idempotencyKey: ulid(),
        });
        await runtime.waitFor(async () => (await service.extend({})).isOk(), {
          timeoutMs: 90_000,
        });
        release.resolve();
        const terminal = await heldWait;
        assert.equal(terminal.state, "completed");
        assert.deepEqual(terminal.result, { value: "result:held" });
        waiting = undefined;
        await runtime.waitFor(
          async () =>
            !(await inventory()).some(
              (item) => brokerConnectionKey(item) === originalKey,
            ),
          { timeoutMs: 30_000 },
        );
        const current = (await attachments()).items;
        assert.equal(current.length, 1);
        assert.notEqual(current[0].attachmentId, initial[0].attachmentId);
        assert.deepEqual((await held.get().orThrow()).result, {
          value: "result:held",
        });
        assert.deepEqual((await held.wait().orThrow()).result, {
          value: "result:held",
        });
        const cancelled = await jobs.create({ value: "cancel" }).orThrow();
        await runtime.waitFor(
          async () => (await cancelled.get().orThrow()).state === "active",
        );
        assert.equal((await cancelled.cancel().orThrow()).state, "cancelled");
        await runtime.waitFor(() => cancellationObserved);
        assert.equal((await cancelled.get().orThrow()).state, "cancelled");
        assert.equal((await cancelled.wait().orThrow()).state, "cancelled");
        const fresh = await jobs.create({ value: "fresh" }).orThrow();
        assert.deepEqual((await fresh.wait().orThrow()).result, {
          value: "result:fresh",
        });
        assert.equal(executions, 3);
      } finally {
        release.resolve();
        await waiting?.catch(() => undefined);
        await service.stop();
        await serviceExit;
        await system.close();
      }
    },
    {
      authorization: {
        contextLifetimeSeconds: 55,
      },
    },
  );
});
