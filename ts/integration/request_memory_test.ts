import { assert, assertEquals } from "@std/assert";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

Deno.test("completed requests release large payloads while the connection stays open", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "request-memory-provider",
      contract: participants.Provider.participant,
    });
    const provider = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Provider.participant,
      seed: identity.seed,
    }).orThrow();
    const client = await runtime.connectClient({
      name: "request-memory-caller",
      contract: participants.Caller.participant,
    });
    try {
      await provider.handleEcho(({ input }) => Result.ok(input));
      await runtime.waitFor(async () =>
        (await client.echo({ value: "ready" })).isOk()
      );
      const baseline = Deno.memoryUsage().heapUsed;
      // More than 512 MiB crosses the real request path, with just 16 calls
      // outstanding. Completed exchanges must not accumulate that entire
      // history in the client. The generous heap allowance includes both SDKs,
      // decoded replies, allocations awaiting GC, and the testkit itself.
      for (let batch = 0; batch < 128; batch++) {
        await Promise.all(Array.from({ length: 16 }, async (_, offset) => {
          const value = `${batch}-${offset}:${"x".repeat(256 * 1024)}`;
          const response = await client.echo({ value }).orThrow();
          assertEquals(response.value, value);
        }));
      }
      const retainedGrowth = Deno.memoryUsage().heapUsed - baseline;
      console.info(
        `Completed RPC heap growth: ${
          Math.round(retainedGrowth / 1024 / 1024)
        } MiB`,
      );
      assert(
        retainedGrowth < 256 * 1024 * 1024,
        `Completed RPCs retained ${
          Math.round(retainedGrowth / 1024 / 1024)
        } MiB of additional heap with the connection still open`,
      );
      assertEquals(
        (await client.echo({ value: "still-open" }).orThrow()).value,
        "still-open",
      );
    } finally {
      await client.connection.close();
      await provider.stop();
    }
  });
});
