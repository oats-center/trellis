import { TrellisService } from "@oatscenter/trellis/service";
import { OperationNotFoundError } from "@oatscenter/trellis/errors";
import { assert, assertEquals, assertRejects } from "@std/assert";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

Deno.test("generated operation control requires the local owner fence", async () => {
  await withTrellisRuntime(async (runtime) => {
    const firstIdentity = await runtime.registerService({
      name: "operation-control-replicas",
      contract: participants.Provider.participant,
    });
    const secondIdentity = await runtime.services.createInstance({
      name: "operation-control-replica-b",
      contract: participants.Provider.participant,
    });
    const services = await Promise.all(
      [firstIdentity, secondIdentity].map((identity) =>
        TrellisService.connect({
          trellisUrl: runtime.trellisUrl,
          participant: participants.Provider.participant,
          name: "operation-control-replicas",
          seed: identity.seed,
        }).orThrow()
      ),
    );
    const firstStarted = Promise.withResolvers<number>();
    const recovered = Promise.withResolvers<void>();
    const releaseRecovered = Promise.withResolvers<void>();
    let executions = 0;
    for (const [index, service] of services.entries()) {
      await service.handleWork(async ({ input, op }) => {
        await op.started().orThrow();
        if (++executions === 1) {
          firstStarted.resolve(index);
          return await new Promise<never>(() => {});
        }
        recovered.resolve();
        await releaseRecovered.promise;
        return await op.complete({ value: input.value }).orThrow();
      });
    }
    const client = await runtime.connectClient({
      name: "operation-control-caller",
      contract: participants.Caller.participant,
    });
    const exits = services.map((service) => service.wait());
    try {
      const operation = await client.work({ value: "control" }).start()
        .orThrow();
      const owner = await firstStarted.promise;
      const other = 1 - owner;
      const active = await services[owner].handleWork.control(operation.id)
        .orThrow();
      assertEquals(active.id, operation.id);
      const absent = await services[owner].handleWork.control(
        "missing-operation",
      );
      assert(absent.isErr());
      assert(absent.error instanceof OperationNotFoundError);
      const nonlocal = await services[other].handleWork.control(operation.id);
      assert(nonlocal.isErr());
      assert(nonlocal.error instanceof OperationNotFoundError);

      await services[owner].stop();
      await exits[owner];
      await recovered.promise;
      await assertRejects(() =>
        services[owner].handleWork.control(operation.id).orThrow()
      );
      const successor = await services[other].handleWork.control(operation.id)
        .orThrow();
      assertEquals(successor.id, operation.id);
      releaseRecovered.resolve();
      assertEquals((await operation.wait().orThrow()).state, "completed");
      await runtime.waitFor(async () =>
        (await services[other].handleWork.control(operation.id)).isErr()
      );
      const stale = await services[other].handleWork.control(operation.id);
      assert(stale.isErr());
      assert(stale.error instanceof OperationNotFoundError);
    } finally {
      releaseRecovered.resolve();
      await client.connection.close();
      await Promise.all(services.map((service) => service.stop()));
      await Promise.all(exits);
    }
  });
});
