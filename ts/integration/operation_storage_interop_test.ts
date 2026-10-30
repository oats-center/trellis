import type { OperationRefData } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import assert from "node:assert/strict";
import { createInterface } from "node:readline";
import { test } from "node:test";
import { inspect } from "node:util";
import { ulid } from "ulid";

import {
  apis,
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { spawnTracked } from "./_support/process.ts";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";

for (const rustFirst of [true, false]) {
  for (const upload of [false, true]) {
    test(
      `Operation ${upload ? "committed uploads" : "checkpoints"} and acknowledged signals survive ${rustFirst ? "Rust-to-Node" : "Node-to-Rust"} takeover`,
      { timeout: 180_000 },
      async () => {
        await withTrellisRuntime(
          async (runtime) => {
            const identities: Awaited<
              ReturnType<typeof runtime.registerService>
            >[] = [];
            for (const name of ["rust", "node"])
              identities.push(
                await runtime.registerService({
                  name: `storage-${name}`,
                  deployment: "shared-operation-storage",
                  contract: participants.OperationProvider.participant,
                }),
              );
            assert.equal(
              identities[0].deploymentId,
              identities[1].deploymentId,
            );
            const client = await runtime.connectClient({
              name: "storage-caller",
              contract: apps.Caller.request,
            });
            const events: Record<string, unknown>[] = [];
            const bytes = Uint8Array.from(
              { length: 131_075 },
              (_, index) => index % 251,
            );
            const providers: { stop(): Promise<void> }[] = [];
            const startProvider = async (rust: boolean) => {
              const boundSubject = `operation.v1.${Buffer.from(apis.runtime.API.identity).toString("base64url")}.${Buffer.from(identities[0].deploymentId).toString("base64url")}.${upload ? "Upload" : "Work"}`;
              if (rust) {
                let brokerReady = false;
                // Observe the real post-subscription broker round trip, not
                // merely a SUB written by the provider's fresh connection.
                const readiness = runtime
                  .nativeTransportGate()
                  .armResponseHold(
                    "$SYS.REQ.USER.INFO",
                    undefined,
                    (_body, connection) =>
                      connection.subs.some(
                        ({ subject }) => subject === boundSubject,
                      ) &&
                      connection.subs.some(
                        ({ subject }) => subject === `${boundSubject}.control`,
                      ),
                  );
                void readiness.held.then(() => {
                  brokerReady = true;
                });
                const child = spawnTracked(
                  "setsid",
                  rustFixtureArgv(
                    upload ? "operation_storage_upload" : "operation_target",
                  ),
                  {
                    env: {
                      ...process.env,
                      TRELLIS_URL: runtime.trellisUrl,
                      TRELLIS_CA_CERTIFICATE: runtime.caCertificate,
                      TRELLIS_IDENTITY_SEED: identities[0].seed,
                    },
                  },
                );
                child.stderr.pipe(process.stderr);
                const output = (async () => {
                  for await (const line of createInterface({
                    input: child.stdout,
                  }))
                    events.push(JSON.parse(line));
                })();
                let stopped = false;
                const provider = {
                  async stop() {
                    if (stopped) return;
                    stopped = true;
                    if (child.exitCode === null) child.kill("SIGKILL");
                    await child.exited;
                    await output;
                  },
                };
                providers.push(provider);
                await runtime.waitFor(() =>
                  events.some((event) => event.ready === "provider"),
                );
                await runtime.waitFor(() => brokerReady);
                await readiness.release();
                return provider;
              }
              runtime.nativeTransportGate().arm(boundSubject);
              const service = await TrellisService.connect({
                trellisUrl: runtime.trellisUrl,
                participant: participants.OperationProvider.participant,
                seed: identities[1].seed,
              }).orThrow();
              let stopped = false;
              const provider = {
                async stop() {
                  if (stopped) return;
                  stopped = true;
                  await service.stop();
                  await service.wait();
                },
              };
              providers.push(provider);
              const registered = async (registration: Promise<void>) => {
                let acknowledged = false;
                let finished = false;
                void registration.then(() => {
                  finished = true;
                });
                void runtime
                  .nativeTransportGate()
                  .barrierHeld()
                  .then(() => {
                    acknowledged = true;
                  });
                try {
                  await runtime.waitFor(() => acknowledged || finished);
                  assert.equal(
                    finished,
                    false,
                    "registration must wait for the broker's subscription acknowledgement",
                  );
                } finally {
                  await runtime.nativeTransportGate().release();
                }
                await registration;
              };
              if (upload) {
                await registered(
                  service.handleUpload(async ({ transfer, op, signal }) => {
                    assert(transfer);
                    const received = await transfer.bytes().orThrow();
                    assert.deepEqual(received, bytes);
                    events.push({
                      uploaded: Array.from(received),
                      info: await transfer.completed().orThrow(),
                    });
                    const accepted = await op.nextSignal("Continue").orThrow();
                    await op.acknowledgeSignal(accepted.sequence).orThrow();
                    const value =
                      accepted.input &&
                      typeof accepted.input === "object" &&
                      "value" in accepted.input
                        ? accepted.input.value
                        : undefined;
                    if (value === "ack-before") {
                      events.push({ acknowledged: accepted.sequence });
                      if (!signal.aborted)
                        await new Promise<void>((resolve) =>
                          signal.addEventListener("abort", () => resolve(), {
                            once: true,
                          }),
                        );
                      return;
                    }
                    await op.complete({ value: `node:${value}` }).orThrow();
                  }),
                );
                return provider;
              }
              await registered(
                service.handleWork(async ({ input, op, signal }) => {
                  if (signal.aborted) return;
                  if (input.value === "handoff-node")
                    await op
                      .progress({
                        value: "retained-checkpoint",
                        nested: {
                          count: 9_223_372_036_854_775_807n,
                          payload: new Uint8Array([0, 127, 255]),
                        },
                      })
                      .orThrow();
                  events.push({ entered: input.value });
                  const accepted = await op.nextSignal("Continue").orThrow();
                  await op.acknowledgeSignal(accepted.sequence).orThrow();
                  const value =
                    accepted.input &&
                    typeof accepted.input === "object" &&
                    "value" in accepted.input
                      ? accepted.input.value
                      : undefined;
                  if (value === "ack-before") {
                    events.push({ acknowledged: accepted.sequence });
                    if (!signal.aborted)
                      await new Promise<void>((resolve) =>
                        signal.addEventListener("abort", () => resolve(), {
                          once: true,
                        }),
                      );
                    return;
                  }
                  await op.complete({ value: `node:${value}` }).orThrow();
                }),
              );
              return provider;
            };
            let stage = "initial provider/start";
            let finishUpload = () => {};
            try {
              const first = await startProvider(rustFirst);
              let second: Awaited<ReturnType<typeof startProvider>> | undefined;
              const releasedUpload = new Promise<void>((resolve) => {
                finishUpload = resolve;
              });
              let uploadPaused = false;
              const body =
                rustFirst && upload
                  ? (async function* () {
                      yield bytes.subarray(0, 4096);
                      uploadPaused = true;
                      await releasedUpload;
                      yield bytes.subarray(4096);
                    })()
                  : bytes;
              const input = {
                value: rustFirst ? "handoff-rust" : "handoff-node",
              };
              const options = {
                invocationId: ulid(),
                targetDeploymentId: identities[0].deploymentId,
              };
              let accepted: OperationRefData | undefined;
              const uploading = upload
                ? client
                    .upload(input)
                    .transfer(body)
                    .onAccepted(({ snapshot }) => {
                      accepted = {
                        ...snapshot,
                        deploymentId: identities[0].deploymentId,
                      };
                    })
                    .start(undefined, options)
                    .orThrow()
                : undefined;
              void uploading?.catch(() => undefined);
              if (upload) await runtime.waitFor(() => accepted !== undefined);
              const operation =
                (accepted ? client.upload.resume(accepted) : undefined) ??
                (await client.work(input).start(undefined, options).orThrow());
              let uploadSignals = 0;
              if (rustFirst && upload) {
                stage = "Node signal acceptance during Rust upload";
                await runtime.waitFor(() => uploadPaused);
                const before = new Set(
                  runtime
                    .nativeTransportGate()
                    .connections()
                    .map(({ id }) => id),
                );
                second = await startProvider(false);
                const controlSubject = `operation.v1.${Buffer.from(apis.runtime.API.identity).toString("base64url")}.${Buffer.from(identities[0].deploymentId).toString("base64url")}.Upload.control`;
                const node = runtime
                  .nativeTransportGate()
                  .connections()
                  .find(
                    (connection) =>
                      !before.has(connection.id) &&
                      connection.subs.some(
                        ({ subject }) => subject === controlSubject,
                      ),
                  );
                assert(node);
                let nodeReceived = false;
                void runtime
                  .nativeTransportGate()
                  .waitForDelivery(controlSubject, node.id)
                  .then(() => {
                    nodeReceived = true;
                  });
                // Each distinct signal is accepted through the public client. The
                // receiving-provider barrier, not a delay, proves a foreign write.
                while (!nodeReceived && uploadSignals < 32) {
                  await operation
                    .signal("Continue", { value: "during-upload" })
                    .orThrow();
                  uploadSignals++;
                }
                assert(
                  nodeReceived,
                  "Node must accept a signal while Rust still owns the unfinished upload",
                );
                finishUpload();
              }
              const startedUpload = await uploading;
              await runtime.waitFor(() =>
                events.some((event) =>
                  upload ? event.uploaded : event.entered,
                ),
              );
              if (uploadSignals)
                await runtime.waitFor(
                  () =>
                    events.filter((event) => event.uploadSignal).length ===
                    uploadSignals,
                );
              stage = "initial durable checkpoint read";
              if (!upload)
                assert.deepEqual((await operation.get().orThrow()).progress, {
                  value: "retained-checkpoint",
                  nested: {
                    count: 9_223_372_036_854_775_807n,
                    payload: new Uint8Array([0, 127, 255]),
                  },
                });
              stage = "durable signal acceptance";
              await operation
                .signal("Continue", { value: "ack-before" })
                .orThrow();
              await runtime.waitFor(() =>
                events.some(
                  (event) => event.acknowledged === uploadSignals + 1,
                ),
              );
              await operation.signal("Continue", { value: "finish" }).orThrow();
              await first.stop();
              stage = "replacement startup";
              second ??= await startProvider(!rustFirst);
              // A killed executor keeps its signed 30-second storage lease. Observe
              // actual takeover before opening a new Live observer; do not change
              // an RPC deadline or ask observation to retry an unavailable owner.
              await runtime.waitFor(
                () =>
                  events.filter((event) =>
                    upload ? event.uploaded : event.entered,
                  ).length === 2,
                { timeoutMs: 45_000 },
              );
              stage = "replacement terminal read";
              const terminal = await operation.wait().orThrow();
              assert.equal(terminal.state, "completed");
              assert.equal(
                terminal.output?.value,
                `${rustFirst ? "node" : "rust"}:finish`,
              );
              assert.equal(
                events.filter(
                  (event) => event.acknowledged === uploadSignals + 1,
                ).length,
                1,
              );
              if (!upload)
                assert.deepEqual(terminal.progress, {
                  value: "retained-checkpoint",
                  nested: {
                    count: 9_223_372_036_854_775_807n,
                    payload: new Uint8Array([0, 127, 255]),
                  },
                });
              if (upload) {
                assert(startedUpload);
                assert.deepEqual(
                  events
                    .filter((event) => event.uploaded)
                    .map((event) => event.uploaded),
                  [Array.from(bytes), Array.from(bytes)],
                );
                stage = "committed upload logical metadata";
                const committed = (await startedUpload.wait().orThrow())
                  .transferred;
                assert.equal(committed.size, bytes.byteLength);
                assert.deepEqual(
                  events
                    .filter((event) => event.uploaded)
                    .map((event) => event.info),
                  [committed, committed],
                );
                assert.equal(
                  terminal.transfer?.transferredBytes,
                  bytes.byteLength,
                );
                return;
              }
              const cancellationId = ulid();
              stage = "same-owner replay and immutable-input conflict";
              const replayed = await client
                .work(input)
                .start(undefined, options)
                .orThrow();
              assert.equal(replayed.id, operation.id);
              assert.equal(
                (await replayed.wait().orThrow()).output?.value,
                terminal.output?.value,
              );
              assert.equal(events.filter((event) => event.entered).length, 2);
              const conflict = await client
                .work({ value: "different-input" })
                .start(undefined, options);
              assert(conflict.isErr());
              stage = "cancellation-first start";
              const cancelled = await client
                .work({ value: "cancelled" })
                .start(undefined, {
                  invocationId: cancellationId,
                  targetDeploymentId: identities[0].deploymentId,
                  cancellationRequested: true,
                })
                .orThrow();
              assert.equal(
                (await cancelled.wait().orThrow()).state,
                "cancelled",
              );
              await second.stop();
              stage = "original SDK restart";
              await startProvider(rustFirst);
              stage = "cross-language cancelled replay";
              const replay = await client
                .work({ value: "cancelled" })
                .start(undefined, {
                  invocationId: cancellationId,
                  targetDeploymentId: identities[0].deploymentId,
                })
                .orThrow();
              stage = "cross-language cancelled terminal read";
              assert.equal((await replay.wait().orThrow()).state, "cancelled");
            } catch (cause) {
              throw new Error(`${stage}: ${inspect(cause, { depth: 3 })}`, {
                cause,
              });
            } finally {
              finishUpload();
              await Promise.all(providers.map((provider) => provider.stop()));
              await client.connection.close();
            }
          },
          { interruptibleNativeProxy: true },
        );
      },
    );
  }
}
