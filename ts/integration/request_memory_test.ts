import assert from "node:assert/strict";
import { test } from "node:test";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";

import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

test("completed requests release large payloads while their connection stays open", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "request-memory-provider",
      contract: participants.Provider.participant,
    });
    const provider = (
      await TrellisService.connect({
        trellisUrl: runtime.oauthIssuer,
        participant: participants.Provider.participant,
        seed: identity.seed,
      })
    ).orThrow();
    try {
      await provider.handleEcho(({ input }) => Result.ok(input));
      const client = await runtime.connectClient({
        name: "request-memory-caller",
        contract: apps.Caller.request,
      });
      try {
        assert.equal(
          (await client.echo({ value: "ready" }).orThrow()).value,
          "ready",
        );
        const baseline = process.memoryUsage().heapUsed;
        // Exchange over 512 MiB with only sixteen outstanding calls. The
        // allowance covers transient allocations, not the completed history.
        for (let batch = 0; batch < 128; batch++) {
          await Promise.all(
            Array.from({ length: 16 }, async (_, offset) => {
              const value = `${batch}-${offset}:${"x".repeat(256 * 1024)}`;
              assert.equal(
                (await client.echo({ value }).orThrow()).value,
                value,
              );
            }),
          );
        }
        const growth = process.memoryUsage().heapUsed - baseline;
        console.info(
          `Completed RPC heap growth: ${Math.round(growth / 1024 / 1024)} MiB`,
        );
        assert(
          growth < 256 * 1024 * 1024,
          `Completed requests retained ${Math.round(growth / 1024 / 1024)} MiB`,
        );
        assert.equal(
          (await client.echo({ value: "still-open" }).orThrow()).value,
          "still-open",
        );
      } finally {
        await client.connection.close();
      }
    } finally {
      await provider.stop();
    }
  });
});
