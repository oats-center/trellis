import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import assert from "node:assert/strict";
import { test } from "node:test";
import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

test("a provisioned Node service recovers after losing every native attachment", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const identity = await runtime.registerService({
        name: "reconnecting-provider",
        contract: participants.Provider.participant,
      });
      const service = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: participants.Provider.participant,
        seed: identity.seed,
      }).orThrow();
      const client = await runtime.connectClient({
        name: "reconnecting-caller",
        contract: apps.Caller.request,
      });
      let calls = 0;
      await service.handleEcho(({ input }) => {
        calls++;
        return Result.ok(input);
      });
      service.jobs.work.handle(({ job }) =>
        Promise.resolve(Result.ok(job.payload)),
      );
      const received: string[] = [];
      await service
        .onChanged(({ event }) => {
          received.push(event.value);
          return Promise.resolve();
        })
        .orThrow();
      let businessExecutions = 0;
      let recoveredCancellation = false;
      let executionInterrupted = false;
      await service.handleWork(async ({ op, signal, resuming }) => {
        if (signal.aborted) {
          recoveredCancellation = resuming;
          return;
        }
        businessExecutions++;
        await op.started().orThrow();
        if (!signal.aborted) {
          await new Promise<void>((resolve) => {
            signal.addEventListener("abort", () => resolve(), { once: true });
          });
        }
        executionInterrupted = true;
      });
      await service.handleWatch(async ({ emit }) => {
        await emit({ value: "recovered" }).orThrow();
      });
      const exited = service.wait().catch((error: unknown) => error);
      try {
        const before = await service.jobs.work
          .create({ value: "before" })
          .orThrow();
        assert.equal((await before.wait().orThrow()).state, "completed");
        await service.publishChanged({ value: "before" }).orThrow();
        await runtime.waitFor(() => received.includes("before"));
        const operation = await client
          .work({ value: "retained" })
          .start()
          .orThrow();
        await runtime.waitFor(
          async () => (await operation.get().orThrow()).state === "running",
        );
        assert.deepEqual(await client.echo({ value: "before" }).orThrow(), {
          value: "before",
        });
        runtime.interruptNativeTransport();
        try {
          await runtime.waitFor(() =>
            ["disconnected", "reconnecting"].includes(
              service.connection.status.phase,
            ),
          );
          await runtime.waitFor(() => executionInterrupted);
        } finally {
          runtime.restoreNativeTransport();
        }
        await runtime.waitFor(
          () =>
            service.connection.status.phase === "connected" &&
            client.connection.status.phase === "connected",
        );
        assert.deepEqual(await client.echo({ value: "after" }).orThrow(), {
          value: "after",
        });
        assert.equal(calls, 2);
        const live = await client.runtimeWatch({}).orThrow();
        try {
          const frame = await live.next();
          assert.equal(frame.done, false);
          assert.deepEqual(frame.value, { value: "recovered" });
        } finally {
          await live.return();
        }
        const after = await service.jobs.work
          .create({ value: "after" })
          .orThrow();
        assert.equal((await after.wait().orThrow()).state, "completed");
        await service.publishChanged({ value: "after" }).orThrow();
        await runtime.waitFor(() => received.includes("after"));
        assert.equal((await operation.get().orThrow()).state, "running");
        assert.equal((await operation.cancel().orThrow()).state, "cancelled");
        assert(recoveredCancellation);
        assert.equal(businessExecutions, 1);
      } finally {
        await client.connection.close();
        await service.stop();
        assert.equal(await exited, undefined);
      }
    },
    { interruptibleNativeProxy: true },
  );
});
