import assert from "node:assert/strict";
import { test } from "node:test";
import { TrellisService } from "@oatscenter/trellis/service";
import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

test(
  "an idle Live route opens after its original signed authority expires",
  { timeout: 120_000 },
  async () => {
    await withTrellisRuntime(
      async (runtime) => {
        const identity = await runtime.registerService({
          name: "live-idle-renewal",
          contract: participants.LiveV1Provider.participant,
        });
        const provider = await TrellisService.connect({
          trellisUrl: runtime.oauthIssuer,
          participant: participants.LiveV1Provider.participant,
          seed: identity.seed,
        }).orThrow();
        try {
          await provider.handleWatch(async ({ input, emit }) => {
            await emit({ value: input.value }).orThrow();
          });
          const sessions = await runtime.callAdminRpc(
            "authAuthorizationSessionsList",
            { page: { limit: 100 } },
          );
          const initial = sessions.items.find(
            (entry) =>
              entry.binding.kind === "service" &&
              entry.binding.participantId ===
                participants.LiveV1Provider.participant.id,
          );
          assert(initial?.maximumContextExpiresAt);
          const expiresAt = Date.parse(String(initial.maximumContextExpiresAt));
          // No Live opening keeps the route guard reconciled while its owner
          // renews. Cross the original expiry, including the 5 s proof skew.
          await runtime.waitFor(() => Date.now() > expiresAt + 5_000, {
            timeoutMs: 90_000,
          });
          const caller = await runtime.connectClient({
            name: "live-idle-caller",
            contract: apps.LiveV1Caller.request,
          });
          const values = [];
          for await (const value of await caller
            .watch({ value: "after-idle-renewal" })
            .orThrow()) {
            values.push(value);
          }
          assert.deepEqual(values, [{ value: "after-idle-renewal" }]);
        } finally {
          await provider.stop();
        }
      },
      {
        authorization: {
          contextLifetimeSeconds: 80,
          refreshLeadSeconds: 15,
          refreshJitterSeconds: 0,
          minimumContextLifetimeSeconds: 46,
        },
      },
    );
  },
);
