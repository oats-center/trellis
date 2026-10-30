import assert from "node:assert/strict";
import { test } from "node:test";
import { PermissionViolationError } from "@nats-io/nats-core";
import { Result } from "@oatscenter/trellis";
import { connectNativeSession } from "@oatscenter/trellis/native";
import { TrellisService } from "@oatscenter/trellis/service";
import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

test("a real Jobs worker appears publicly while another principal cannot publish its heartbeat", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const contract =
        participants.TransportGrowthOperationProvider.participant;
      const identity = await runtime.registerService({
        name: "heartbeat-owner",
        contract,
      });
      const service = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: contract,
        seed: identity.seed,
      }).orThrow();
      service.jobs.retained.handle(
        ({ job }) => Promise.resolve(Result.ok(job.payload)),
        { concurrency: 1 },
      );
      const exited = service.wait().catch((error: unknown) => error);
      try {
        const job = await service.jobs.retained
          .create({ value: "heartbeat-work" })
          .orThrow();
        assert.deepEqual((await job.wait().orThrow()).result, {
          value: "heartbeat-work",
        });
        const inspector = await runtime.connectClient({
          name: "heartbeat-inspector",
          contract: apps.JobsAdmin.request,
        });
        const worker = await runtime.waitFor(async () => {
          const page = await inspector.listServices({}).orThrow();

          return page.items
            .flatMap((entry) => entry.workers)
            .find((entry) => entry.jobType === "retained");
        });
        const subject = runtime
          .nativeTransportGate()
          .connections()
          .flatMap((connection) => connection.outboundContexts)
          .find(
            (out) =>
              out.subject.startsWith("trellis.jobs.workers.") &&
              out.subject.endsWith(`.${worker.instanceId}.heartbeat`),
          )?.subject;
        assert(subject);
        const login = await runtime.registerClient({
          name: "foreign-heartbeat",
          contract: apps.TransportGrowthCaller.request,
        });
        const unrelated = await connectNativeSession(login).orThrow();
        const socket = unrelated.manager.currentNats();
        const statuses = socket.status();
        try {
          let denied = false;
          const monitoring = (async () => {
            for await (const status of statuses) {
              if (
                status.type === "error" &&
                status.error instanceof PermissionViolationError
              ) {
                assert.equal(status.error.operation, "publish");
                assert.equal(status.error.subject, subject);
                denied = true;
                return;
              }
            }
          })();
          socket.publish(
            subject,
            new TextEncoder().encode(
              JSON.stringify({
                service: worker.service,
                jobType: worker.jobType,
                instanceId: worker.instanceId,
                timestamp: new Date().toISOString(),
              }),
            ),
          );
          await runtime.waitFor(() => denied);
          await monitoring;
          const remaining = await inspector.listServices({}).orThrow();
          assert(
            remaining.items
              .flatMap((entry) => entry.workers)
              .some((entry) => entry.instanceId === worker.instanceId),
          );
        } finally {
          await unrelated.manager.close();
        }
      } finally {
        await service.stop();
        await exited;
      }
    },
    { interruptibleNativeProxy: true },
  );
});
