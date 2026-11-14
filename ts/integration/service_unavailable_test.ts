import { Result } from "@oatscenter/result";
import { TransportError } from "@oatscenter/trellis/errors";
import { TrellisService } from "@oatscenter/trellis/service";
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

test("unavailable provider preserves typed observation errors and completed operations", async () => {
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
      contract: apps.Caller.request,
    });
    try {
      const operation = await client
        .work({ value: "durable-result" })
        .start()
        .orThrow();
      const terminal = await operation.wait().orThrow();
      assert.deepEqual(terminal.state, "completed");
      await service.stop();
      await service.wait();

      // Each call that finds no provider first waits up to five seconds for
      // one, so these wait together.
      const unavailable = await Promise.all([
        operation.live(),
        operation.wait(),
        client.runtimeWatch({}),
        client.echo({ value: "unavailable" }),
      ]);
      for (const outcome of unavailable) {
        assert(outcome.isErr());
        assert(outcome.error instanceof TransportError);
        assert.deepEqual(outcome.error.code, "trellis.request.unavailable");
        assert.deepEqual(
          outcome.error.toSerializable().context?.noResponders,
          true,
        );
      }

      // A call made while the provider is away waits for it to come back, as
      // when a provider reconnects: the provider stays away for a moment after
      // the call is sent, then connects and handles its routes.
      const waited = client.echo({ value: "waited" });
      const observing = operation.live();
      await new Promise((resolve) => setTimeout(resolve, 300));
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
      assert.deepEqual(await waited.orThrow(), { value: "waited" });
      const observation = await observing.orThrow();
      try {
        const event = await observation[Symbol.asyncIterator]().next();
        assert(!event.done);
        assert.deepEqual(event.value.type, "completed");
        assert.deepEqual(event.value.snapshot, terminal);
      } finally {
        await observation[Symbol.asyncDispose]();
      }
      assert.deepEqual(await operation.wait().orThrow(), terminal);
      assert.deepEqual(await client.echo({ value: "available" }).orThrow(), {
        value: "available",
      });
      assert.deepEqual(businessExecutions, 1);
    } finally {
      await client.connection.close();
      await service.stop();
      await service.wait();
    }
  });
});
