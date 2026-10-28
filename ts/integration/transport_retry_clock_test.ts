import assert from "node:assert/strict";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

test(
  "transport recovery retries when its timer expires before wall-clock time",
  { timeout: 60_000 },
  async (context) => {
    await withTrellisRuntime(
      async (runtime) => {
        const identity = await runtime.registerService({
          name: "retry-clock-provider",
          contract: participants.Provider.participant,
        });
        const service = await TrellisService.connect({
          trellisUrl: runtime.oauthIssuer,
          participant: participants.Provider.participant,
          seed: identity.seed,
        }).orThrow();
        try {
          const caller = await runtime.connectClient({
            name: "retry-clock-caller",
            contract: apps.Caller.request,
          });
          let executed = 0;
          try {
            await service.handleEcho(({ input }) => {
              executed++;
              return Result.ok(input);
            });
            assert.equal(
              (await caller.echo({ value: "before" }).orThrow()).value,
              "before",
            );
            runtime.interruptNativeTransport();
            const unavailable = await caller.echo(
              { value: "during outage" },
              { timeout: 5_000 },
            );
            assert(unavailable.isErr());
            assert.equal(executed, 1);
            context.mock.timers.enable({ apis: ["Date"], now: Date.now() });
            try {
              // Hold the clock behind a complete capped recovery-backoff window.
              // Timer expiry must permit another attempt without Date.now advancing.
              await delay(8_000);
              assert.notEqual(caller.connection.status.phase, "connected");
            } finally {
              context.mock.timers.reset();
              runtime.restoreNativeTransport();
            }
            const deadline = AbortSignal.timeout(10_000);
            // Restoring the shared proxy recovers caller and provider
            // independently. Caller admission alone cannot serve Echo.
            while (
              (caller.connection.status.phase !== "connected" ||
                service.connection.status.phase !== "connected") &&
              !deadline.aborted
            ) {
              await delay(25);
            }
            assert.equal(caller.connection.status.phase, "connected");
            assert.equal(service.connection.status.phase, "connected");
            assert.equal(
              (await caller.echo({ value: "after" }).orThrow()).value,
              "after",
            );
            assert.equal(executed, 2);
          } finally {
            runtime.restoreNativeTransport();
            await caller.connection.close();
          }
        } finally {
          await service.stop();
        }
      },
      { interruptibleNativeProxy: true },
    );
  },
);
