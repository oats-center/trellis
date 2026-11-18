/** A TypeScript provider accepts request proofs within the shared 30 s clock window. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

test(
  "a request verified 12 s after signing is served and one verified after 33 s is refused",
  { timeout: 120_000 },
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const providerIdentity = await runtime.registerService({
        name: "proof-age-provider",
        deployment: "proof-age-provider",
        contract: participants.FiniteProvider.participant,
      });
      const provider = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: participants.FiniteProvider.participant,
        seed: providerIdentity.seed,
      }).orThrow();
      const callerIdentity = await runtime.registerService({
        name: "proof-age-caller",
        deployment: "proof-age-caller",
        contract: participants.FiniteCaller.participant,
      });
      const caller = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: participants.FiniteCaller.participant,
        seed: callerIdentity.seed,
      }).orThrow();
      const executed: string[] = [];
      const entered = new Map<string, PromiseWithResolvers<void>>();
      const released = new Map<string, PromiseWithResolvers<void>>();
      for (const value of ["first", "beyond"]) {
        entered.set(value, Promise.withResolvers<void>());
        released.set(value, Promise.withResolvers<void>());
      }
      const handler: Parameters<typeof provider.handleHoldSuccess>[0] = async ({
        input,
      }) => {
        executed.push(input.value);
        entered.get(input.value)?.resolve();
        await released.get(input.value)?.promise;
        return Result.ok(input);
      };
      // A provider runs one subject's requests in order and verifies each when
      // its turn comes, so a request queued behind a slow one is checked
      // against a proof signed when it was sent.
      await provider.handleHoldSuccess(handler);
      await provider.handleHoldDeclared(handler);
      try {
        const first = caller.holdSuccess(
          { value: "first" },
          { timeout: 60_000 },
        );
        const beyond = caller.holdDeclared(
          { value: "beyond" },
          { timeout: 60_000 },
        );
        await entered.get("first")?.promise;
        await entered.get("beyond")?.promise;
        const queuedAt = Date.now();
        const queued = caller.holdSuccess(
          { value: "queued" },
          { timeout: 60_000 },
        );
        const expired = caller.holdDeclared(
          { value: "expired" },
          { timeout: 40_000 },
        );
        // The wait is the age under test: Rust accepts up to 30 s.
        await delay(queuedAt + 12_000 - Date.now());
        released.get("first")?.resolve();
        assert.deepEqual(await first.orThrow(), { value: "first" });
        assert.deepEqual(await queued.orThrow(), { value: "queued" });
        await delay(queuedAt + 33_000 - Date.now());
        released.get("beyond")?.resolve();
        assert.deepEqual(await beyond.orThrow(), { value: "beyond" });
        assert((await expired).isErr());
        assert.deepEqual(executed.sort(), ["beyond", "first", "queued"]);
      } finally {
        for (const release of released.values()) release.resolve();
        await caller.stop();
        await provider.stop();
      }
    });
  },
);
