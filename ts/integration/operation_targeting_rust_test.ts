import assert from "node:assert/strict";
import { test } from "node:test";
import { createInterface } from "node:readline";
import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import { ulid } from "ulid";
import { TrellisService } from "@oatscenter/trellis/service";
import type { OperationRefData, OperationSnapshot } from "@oatscenter/trellis";
import {
  apis,
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { spawnTracked } from "./_support/process.ts";
import {
  authorizeRustNativeLogin,
  rustFixtureArgv,
  withTrellisRuntime,
} from "./_support/runtime.ts";

for (const sharedDeployment of [false, true]) {
  test(
    sharedDeployment
      ? "Rust and Node replicas share one deployment's Operation records and routes"
      : "Rust and Node target each other's deployments and share one canonical delivery group",
    { timeout: 180_000 },
    async () => {
      await withTrellisRuntime(
        async (runtime) => {
          const nativeIdentity = await runtime.registerService({
            name: "mixed-native",
            deployment: sharedDeployment
              ? "mixed-deployment"
              : "native-destination",
            contract: participants.OperationProvider.participant,
          });
          const nodeIdentity = await runtime.registerService({
            name: "mixed-node",
            deployment: sharedDeployment
              ? "mixed-deployment"
              : "node-destination",
            contract: participants.OperationProvider.participant,
          });
          if (sharedDeployment)
            assert.equal(
              nodeIdentity.deploymentId,
              nativeIdentity.deploymentId,
            );
          else
            assert.notEqual(
              nodeIdentity.deploymentId,
              nativeIdentity.deploymentId,
            );
          const native = spawnTracked(
            "setsid",
            rustFixtureArgv("operation_target"),
            {
              env: {
                ...process.env,
                TRELLIS_URL: runtime.oauthIssuer,
                TRELLIS_IDENTITY_SEED: nativeIdentity.seed,
              },
            },
          );
          native.stderr.pipe(process.stderr);
          const nativeLines: string[] = [];
          const nativeOutput = (async () => {
            for await (const line of createInterface({ input: native.stdout }))
              nativeLines.push(line);
          })();
          const service = await TrellisService.connect({
            trellisUrl: runtime.oauthIssuer,
            participant: participants.OperationProvider.participant,
            seed: nodeIdentity.seed,
            name: "mixed-node",
          }).orThrow();
          const client = await runtime.connectClient({
            name: "mixed-caller",
            contract: apps.Caller.request,
          });
          let caller: typeof native | undefined;
          let callerOutput: Promise<void> | undefined;
          try {
            await runtime.waitFor(() =>
              nativeLines.includes('{"ready":"provider"}'),
            );
            const canonical =
              apis.runtime.API.actions["operation:Work"].subject;
            const gate = runtime.nativeTransportGate();
            await runtime.waitFor(() =>
              gate
                .connections()
                .some((connection) =>
                  connection.subs.some((sub) => sub.subject === canonical),
                ),
            );
            const nativeResult = await client
              .work({ value: "node-to-rust" })
              .start(undefined, {
                invocationId: ulid(),
                targetDeploymentId: nativeIdentity.deploymentId,
              })
              .orThrow();
            assert.equal(
              (await nativeResult.wait().orThrow()).output?.value,
              "rust:node-to-rust",
            );
            await service.handleWork(async ({ input, signal, op }) => {
              if (signal.aborted) return;
              await op
                .complete({ value: `typescript:${input.value}` })
                .orThrow();
            });
            const providers = await runtime.waitFor(() => {
              const connections = gate
                .connections()
                .filter(
                  (connection) =>
                    !connection.closed &&
                    connection.subs.some((sub) => sub.subject === canonical),
                );
              return connections.length === 2 ? connections : false;
            });
            {
              const subject: string = canonical;
              const deliveries = (id: number): number =>
                gate
                  .connection(id)
                  ?.deliveries.filter(
                    (delivery) => delivery.subject === subject,
                  ).length ?? 0;
              const before = providers.map(({ id }) => deliveries(id));
              let starts = 0;
              const invoke = async () => {
                starts++;
                const invocationId = ulid();
                const operation = await client
                  .work({ value: `group-${starts}` })
                  .start(undefined, { invocationId })
                  .orThrow();
                assert(
                  [
                    nativeIdentity.deploymentId,
                    nodeIdentity.deploymentId,
                  ].includes(operation.deploymentId),
                );
                assert.match(
                  (await operation.wait().orThrow()).output?.value ?? "",
                  /^(rust|typescript):group-/u,
                );
              };
              for (let index = 0; index < 6; index++) await invoke();
              const beforeMarkers = providers.map(({ id }) => deliveries(id));
              // Each provider must observe a later start on this same subject. NATS
              // subject ordering then proves its earlier deliveries have been observed;
              // no cross-subscription flush assumption or sleeps are involved.
              const markerDeadline = Date.now() + 30_000;
              while (
                !providers.every(
                  ({ id }, index) => deliveries(id) > beforeMarkers[index],
                )
              ) {
                assert(
                  Date.now() < markerDeadline,
                  "both providers must receive a later same-subject marker",
                );
                await invoke();
              }
              const received = providers.reduce(
                (sum, { id }, index) => sum + deliveries(id) - before[index],
                0,
              );
              assert.equal(
                received,
                starts,
                "one start must produce one broker delivery, including across language implementations",
              );
            }
            native.stdin.end("stop\n");
            assert.equal((await native.exited).code, 0);
            await nativeOutput;

            const custody = join(runtime.workdir, "mixed-native-caller");
            await mkdir(custody, { mode: 0o700 });
            caller = spawnTracked(
              "setsid",
              rustFixtureArgv("operation_target", ["--caller"]),
              {
                env: {
                  ...process.env,
                  TRELLIS_URL: runtime.oauthIssuer,
                  TRELLIS_NATIVE_CLIENT_ID: apps.Caller.request.id,
                  XDG_CONFIG_HOME: custody,
                },
              },
            );
            caller.stderr.pipe(process.stderr);
            const callerLines: string[] = [];
            const nativeCaller = caller;
            callerOutput = (async () => {
              for await (const line of createInterface({
                input: nativeCaller.stdout,
              }))
                callerLines.push(line);
            })();
            await runtime.waitFor(() =>
              callerLines.some((line) => line.startsWith("NATIVE_LOGIN ")),
            );
            await authorizeRustNativeLogin(
              runtime,
              callerLines.join("\n"),
              apps.Caller.request,
            );
            await runtime.waitFor(() =>
              callerLines.includes('{"ready":"caller"}'),
            );
            const results: {
              ref: OperationRefData;
              snapshot: OperationSnapshot<unknown, { value: string }>;
            }[] = [];
            const invokeNative = async (
              invocationId: string,
              value: string,
              cancelled = false,
            ) => {
              const before = results.length;
              nativeCaller.stdin.write(
                `${JSON.stringify({ deploymentId: nodeIdentity.deploymentId, invocationId, value, cancelled })}\n`,
              );
              await runtime.waitFor(() => {
                const lines = callerLines.filter((line) =>
                  line.startsWith('{"ref":'),
                );
                if (lines.length <= before) return false;
                results.push(JSON.parse(lines[before]));
                return true;
              });
              const result = results[before];
              assert.equal(result.ref.deploymentId, nodeIdentity.deploymentId);
              return result;
            };
            assert.equal(
              (await invokeNative(ulid(), "rust-to-node")).snapshot.output
                ?.value,
              "typescript:rust-to-node",
            );
            const cancelledId = ulid();
            assert.equal(
              (await invokeNative(cancelledId, "cancelled", true)).snapshot
                .state,
              "cancelled",
            );
            assert.equal(
              (await invokeNative(cancelledId, "cancelled")).snapshot.state,
              "cancelled",
            );
            caller.stdin.end();
            assert.equal((await caller.exited).code, 0);
            await callerOutput;
          } finally {
            if (native.exitCode === null) native.kill("SIGTERM");
            if (caller && caller.exitCode === null) caller.kill("SIGTERM");
            await Promise.all([
              native.exited,
              caller?.exited,
              nativeOutput,
              callerOutput,
            ]);
            await client.connection.close();
            await service.stop();
            await service.wait();
          }
        },
        { interruptibleNativeProxy: true },
      );
    },
  );
}
