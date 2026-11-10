/** A running service's job workers ride out their broker restarting. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

test("a service keeps processing jobs, including one in flight, after its broker restarts", async () => {
  await withTrellisRuntime(async (runtime) => {
    const participant = participants.Provider.participant;
    const instance = await runtime.registerService({
      name: "restarted-broker",
      contract: participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant,
      seed: instance.seed,
    }).orThrow();
    const release = Promise.withResolvers<void>();
    const handled: string[] = [];
    service.jobs.work.handle(
      ({ job }) => {
        handled.push(job.payload.value);
        return Promise.resolve(Result.ok(job.payload));
      },
      { concurrency: 4 },
    );
    let holding = false;
    service.jobs.restartWork.handle(async ({ job }) => {
      holding = true;
      await release.promise;
      return Result.ok(job.payload);
    });
    let exited: unknown;
    const serviceExit = service.wait().then(
      () => (exited = "service stopped"),
      (error: unknown) => (exited = error ?? "service failed"),
    );
    try {
      await service.jobs.work.create({ value: "before" }).orThrow();
      await runtime.waitFor(() => handled.includes("before"));
      const inFlight = await service.jobs.restartWork
        .create({ value: "held" })
        .orThrow();
      await runtime.waitFor(() => holding);

      await runtime.restart();
      release.resolve();

      await runtime.waitFor(
        async () => {
          assert.equal(exited, undefined);
          return (await service.jobs.work.create({ value: "after" })).isOk();
        },
        { timeoutMs: 60_000 },
      );
      await runtime.waitFor(() => handled.includes("after"), {
        timeoutMs: 60_000,
      });
      assert.equal((await inFlight.wait().orThrow()).state, "completed");
      assert.equal(exited, undefined);
    } finally {
      release.resolve();
      await service.stop();
      await serviceExit;
    }
  });
});
