import assert from "node:assert/strict";
import { appendFileSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";
import { ulid } from "ulid";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import {
  apis,
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { spawnTracked } from "./_support/process.ts";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";

for (const grow of [false, true]) {
  test(
    `mixed Rust/Node RPC mutations execute once through removal${grow ? " and authority handover" : ""}`,
    { timeout: 180_000 },
    async () => {
      await withTrellisRuntime(
        async (runtime) => {
          await using resources = new AsyncDisposableStack();
          const contract =
            participants.TransportGrowthOperationProvider.participant;
          const nodeIdentity = await runtime.registerService({
            name: "distribution-node",
            contract,
          });
          const capabilityId = "runtime-trellis.transport_growth@v1::extend";
          const grants = await runtime.callAdminRpc(
            "authCapabilityGrantsList",
            { capabilityId },
          );
          assert.equal(grants.items.length, 1);
          const [grant] = grants.items;
          if (grow)
            await runtime.callAdminRpc("authCapabilityGrantsRevoke", {
              grantId: grant.grantId,
              expectedRevision: grant.revision,
              idempotencyKey: ulid(),
            });
          const nativeIdentity = await runtime.registerService({
            name: "distribution-rust",
            contract,
          });
          const nodeEffects = join(runtime.workdir, "node-rpc-effects");
          const rustEffects = join(runtime.workdir, "rust-rpc-effects");
          writeFileSync(nodeEffects, "");
          writeFileSync(rustEffects, "");
          const service = await TrellisService.connect({
            trellisUrl: runtime.oauthIssuer,
            participant: contract,
            seed: nodeIdentity.seed,
          }).orThrow();
          resources.defer(() => service.stop());
          const caller = await runtime.connectClient({
            name: "distribution-caller",
            contract: apps.RpcDistributionCaller.request,
            timeout: 120_000,
          });
          resources.defer(() => caller.connection.close());
          const release = Promise.withResolvers<void>();
          resources.defer(() => release.resolve());
          let nodeCalls = 0;
          await service.handlePark(async () => {
            appendFileSync(nodeEffects, "node\n");
            if (++nodeCalls === 1) await release.promise;
            return Result.ok({ value: "node" });
          });
          await service.handleExtend(() => Result.ok({}));
          await service.handleProgress(async ({ emit }) => {
            await emit({ value: "node" });
          });
          const gate = runtime.nativeTransportGate();
          const subject = apis.transport_growth.API.actions["rpc:Park"].subject;
          const intakes = () =>
            gate
              .connections()
              .filter(
                (connection) =>
                  !connection.closed &&
                  connection.subs.some((sub) => sub.subject === subject),
              );
          await runtime.waitFor(() => intakes().length === 1);
          const nodeIntakeId = intakes()[0].id;
          const held = caller.park({}).orThrow();
          void held.catch(() => undefined);
          await runtime.waitFor(() => nodeCalls === 1);
          if (!grow) {
            release.resolve();
          }
          const native = spawnTracked(
            "setsid",
            rustFixtureArgv("rpc_distribution"),
            {
              env: {
                ...process.env,
                TRELLIS_URL: runtime.oauthIssuer,
                TRELLIS_IDENTITY_SEED: nativeIdentity.seed,
                RPC_EFFECTS_FILE: rustEffects,
              },
            },
          );
          native.stderr.pipe(process.stderr);
          native.stdout.resume();
          try {
            const before = await runtime.waitFor(() =>
              intakes().length === 2 ? intakes() : undefined,
            );
            if (grow) {
              await runtime.callAdminRpc("authCapabilityGrantsGrant", {
                principalId: grant.principalId,
                capabilityId,
                expectedRevision: 0n,
                idempotencyKey: ulid(),
              });
              await runtime.waitFor(
                () =>
                  intakes().some(
                    (connection) =>
                      !before.some((old) => old.id === connection.id),
                  ),
                { timeoutMs: 90_000 },
              );
            }
            // A handler is serial within one intake. Complete its accepted
            // request after replacement intake appears, then observe retirement
            // before sending fresh work through the surviving generations.
            release.resolve();
            assert.deepEqual(await held, { value: "node" });
            if (grow)
              await runtime.waitFor(
                () =>
                  gate
                    .connections()
                    .find((connection) => connection.id === nodeIntakeId)
                    ?.closed,
              );
            const observation = await caller.progress({}).orThrow();
            const values: string[] = [];
            try {
              for await (const frame of observation) values.push(frame.value);
            } finally {
              await observation.close().orThrow();
            }
            assert.equal(values.length, 1);
            assert(["rust", "node"].includes(values[0]));
            assert(
              ["rust", "node"].includes(
                (await caller.park({}).orThrow()).value,
              ),
            );
            native.stdin.end();
            assert.equal((await native.exited).code, 0);
            await runtime.waitFor(() =>
              intakes().every(
                (connection) =>
                  before[0].id === connection.id ||
                  !before.some((old) => old.id === connection.id),
              ),
            );
            assert.deepEqual(await caller.park({}).orThrow(), {
              value: "node",
            });
            await service.stop();
            const effects = (
              readFileSync(nodeEffects, "utf8") +
              readFileSync(rustEffects, "utf8")
            )
              .trim()
              .split("\n");
            assert.equal(
              effects.length,
              3,
              "three ordinary sends must have exactly three combined provider effects",
            );
            const opening =
              apis.transport_growth.API.actions["live:Progress"].subject;
            assert.equal(
              gate
                .connections()
                .flatMap((connection) => connection.deliveries)
                .filter((delivery) => delivery.subject === opening).length,
              1,
              "the broker must deliver a Live opening to only one mixed-language intake",
            );
          } finally {
            release.resolve();
            await held.catch(() => undefined);
            native.stdin.end();
            await native.exited;
            await caller.connection.close();
            await service.stop();
          }
        },
        {
          interruptibleNativeProxy: true,
          authorization: {
            contextLifetimeSeconds: 125,
            refreshLeadSeconds: 15,
            refreshJitterSeconds: 0,
            minimumContextLifetimeSeconds: 95,
          },
        },
      );
    },
  );
}
