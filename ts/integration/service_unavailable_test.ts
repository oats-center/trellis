import { Result } from "@oatscenter/result";
import { TransportError } from "@oatscenter/trellis/errors";
import { TrellisService } from "@oatscenter/trellis/service";
import { assert, assertEquals } from "@std/assert";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

Deno.test("unavailable provider preserves typed observation errors and completed operations", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "observation-unavailable",
      contract: participants.Provider.participant,
    });
    let service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Provider.participant,
      name: "observation-unavailable",
      seed: identity.seed,
    }).orThrow();
    let businessExecutions = 0;
    await service.handleWork(({ input }) => {
      businessExecutions++;
      return input;
    });
    await service.handleEcho(({ input }) => Result.ok(input));
    const client = await runtime.connectClient({
      name: "observation-unavailable-caller",
      contract: participants.Caller.participant,
    });
    try {
      const operation = await client.work({ value: "durable-result" }).start()
        .orThrow();
      const terminal = await operation.wait().orThrow();
      assertEquals(terminal.state, "completed");
      await service.stop();
      await service.wait();

      const unavailable = [
        await operation.live(),
        await operation.wait(),
        await client.watch({}),
        await client.echo({ value: "unavailable" }),
      ];
      for (const outcome of unavailable) {
        assert(outcome.isErr());
        assert(outcome.error instanceof TransportError);
        assertEquals(outcome.error.code, "trellis.request.unavailable");
        assertEquals(
          outcome.error.toSerializable().context?.noResponders,
          true,
        );
      }

      service = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: participants.Provider.participant,
        name: "observation-unavailable",
        seed: identity.seed,
      }).orThrow();
      await service.handleWork(({ input }) => {
        businessExecutions++;
        return input;
      });
      await service.handleEcho(({ input }) => Result.ok(input));
      const observation = await operation.live().orThrow();
      try {
        const event = await observation[Symbol.asyncIterator]().next();
        assert(!event.done);
        assertEquals(event.value.type, "completed");
        assertEquals(event.value.snapshot, terminal);
      } finally {
        await observation[Symbol.asyncDispose]();
      }
      assertEquals(await operation.wait().orThrow(), terminal);
      assertEquals(
        await client.echo({ value: "available" }).orThrow(),
        { value: "available" },
      );
      assertEquals(businessExecutions, 1);
    } finally {
      await client.connection.close();
      await service.stop();
      await service.wait();
    }
  });
});
