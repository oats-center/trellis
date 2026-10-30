import assert from "node:assert/strict";
import { test } from "node:test";
import { TrellisService } from "@oatscenter/trellis/service";
import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import {
  apps as v2Apps,
  participants as v2Participants,
} from "../../integration/fixtures/runtime/v2/packages/runtime-trellis/index.js";
import { spawnTracked } from "./_support/process.ts";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";

test(
  "idle TypeScript and Rust Live routes open after their original signed authority expires",
  { timeout: 120_000 },
  async () => {
    await withTrellisRuntime(
      async (runtime) => {
        const identity = await runtime.registerService({
          deployment: "live-idle-v1",
          name: "live-idle-renewal",
          contract: participants.LiveV1Provider.participant,
        });
        const rustIdentity = await runtime.registerService({
          deployment: "live-idle-v2",
          name: "live-idle-renewal-rust",
          contract: v2Participants.LiveV2Provider.participant,
        });
        // Register both before connecting either, so no failure can strand a
        // connected provider outside the cleanup below.
        const provider = await TrellisService.connect({
          trellisUrl: runtime.trellisUrl,
          participant: participants.LiveV1Provider.participant,
          seed: identity.seed,
        }).orThrow();
        const argv = rustFixtureArgv("live_application_versions");
        const rust = spawnTracked(argv[0], argv.slice(1), {
          env: {
            ...process.env,
            TRELLIS_URL: runtime.trellisUrl,
            TRELLIS_CA_CERTIFICATE: runtime.caCertificate,
            TRELLIS_IDENTITY_SEED: rustIdentity.seed,
          },
        });
        let rustOutput = "";
        rust.stdout.on("data", (chunk) => {
          rustOutput += String(chunk);
        });
        rust.stderr.on("data", (chunk) => {
          rustOutput += String(chunk);
        });
        try {
          await provider.handleWatch(async ({ input, emit }) => {
            await emit({ value: input.value }).orThrow();
          });
          const rustCaller = await runtime.connectClient({
            name: "live-idle-rust-caller",
            contract: v2Apps.LiveV2Caller.request,
          });
          // A verified reply proves the Rust provider's intake is ready.
          await runtime.waitFor(async () =>
            (
              await rustCaller.echo({ value: "ready" }, { timeout: 1_000 })
            ).isOk(),
          );
          const sessions = await runtime.callAdminRpc(
            "authAuthorizationSessionsList",
            { page: { limit: 100 } },
          );
          const expiresAt = Math.max(
            ...[
              participants.LiveV1Provider.participant.id,
              v2Participants.LiveV2Provider.participant.id,
            ].map((participantId) => {
              const session = sessions.items.find(
                (entry) =>
                  entry.binding.kind === "service" &&
                  entry.binding.participantId === participantId,
              );
              assert(session?.maximumContextExpiresAt, participantId);
              return Date.parse(String(session.maximumContextExpiresAt));
            }),
          );
          // No Live opening keeps either route guard reconciled while its owner
          // renews. Cross both original expiries, including the 5 s proof skew.
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
          const rustValues = [];
          for await (const value of await rustCaller
            .watch({ value: "after-idle-renewal" })
            .orThrow()) {
            rustValues.push(value);
          }
          assert.deepEqual(rustValues, [{ value: "v2:after-idle-renewal" }]);
        } finally {
          await provider.stop();
          rust.stdin.end();
          const status = await rust.exited;
          assert.equal(status.code, 0, rustOutput);
        }
      },
      {
        // The shortest policy the runtime accepts with its 30 s clock skew.
        authorization: {
          contextLifetimeSeconds: 30,
        },
      },
    );
  },
);
