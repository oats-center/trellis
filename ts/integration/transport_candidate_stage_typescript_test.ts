/** Real TS no-carrier recovery and cancellation while registry warming is held. */
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { join } from "node:path";
import { connect, credsAuthenticator } from "@nats-io/transport-node";
import { jetstream } from "@nats-io/jetstream";
import { Result, type SessionCaller } from "@oatscenter/trellis";
import { ulid } from "ulid";
import { TrellisService } from "@oatscenter/trellis/service";
import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import {
  admittedConnections,
  brokerConnectionKey,
  completeBrokerInventory,
  readRuntimeServiceAttachments as attachmentsFor,
} from "./_support/broker_inventory.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

type WatchInfo = {
  created?: string;
  config?: { filter_subject?: string; deliver_subject?: string };
  push_bound?: boolean;
};

for (const closeWhileHeld of [false, true]) {
  test(
    closeWhileHeld
      ? "TS logical close releases a withheld private stage before late INFO without resurrection"
      : "TS no-survivor reduction warms one private socket then adopts that same socket",
    async () => {
      await withTrellisRuntime(
        async (runtime) => {
          const targetContract = participants.TransportGrowthTarget.participant;
          const subjectContract =
            participants.TransportGrowthSubject.participant;
          await runtime.contracts.apply({
            deployment: "cs-target",
            contract: targetContract,
          });
          const targetInstance = await runtime.services.createInstance({
            deployment: "cs-target",
            name: "cs-target",
            contract: targetContract,
          });
          const target = await TrellisService.connect({
            trellisUrl: runtime.oauthIssuer,
            participant: targetContract,
            name: "cs-target",
            seed: targetInstance.seed,
          }).orThrow();
          const targetExit = target.wait().catch((error: unknown) => error);
          const system = await connect({
            servers: runtime.natsUrl,
            authenticator: credsAuthenticator(
              await readFile(
                join(runtime.workdir, "config/trellis/nats/creds/system.creds"),
              ),
            ),
          });
          const gate = runtime.nativeTransportGate();
          let hold: ReturnType<typeof gate.armResponseHold> | undefined;
          let subject: Awaited<ReturnType<typeof connectSubject>> | undefined;
          let subjectExit: Promise<unknown> | undefined;
          let observer: Awaited<ReturnType<typeof connectObserver>> | undefined;
          const advanceCallers: SessionCaller[] = [];
          let extendsEntered = 0;
          let inspectEntered = 0;
          let watchEntered = 0;
          let watchCleanups = 0;
          const key = { runId: ulid(), streamId: "stage" };

          async function connectSubject(seed: string) {
            return await TrellisService.connect({
              trellisUrl: runtime.oauthIssuer,
              participant: subjectContract,
              name: "cs-subject",
              seed,
            }).orThrow();
          }
          async function connectObserver() {
            return await runtime.connectClient({
              name: "cs-observer",
              contract: apps.LiveProbeCaller.request,
            });
          }

          try {
            await target.handleAdvance(({ context }) => {
              advanceCallers.push(context.caller);
              return Result.ok({});
            });
            await target.handleExtend(() => {
              extendsEntered += 1;
              return Result.ok({});
            });
            await target.handleProgress(async ({ emit, signal }) => {
              await emit({ value: "progress" }).orThrow();
              if (!signal.aborted) {
                await new Promise<void>((resolve) =>
                  signal.addEventListener("abort", () => resolve(), {
                    once: true,
                  }),
                );
              }
            });
            await runtime.contracts.install({ contract: subjectContract });
            await runtime.contracts.apply({
              deployment: "cs-subject",
              contract: subjectContract,
            });
            const instance = await runtime.services.createInstance({
              deployment: "cs-subject",
              name: "cs-subject",
              contract: subjectContract,
            });
            subject = await connectSubject(instance.seed);
            subjectExit = subject.wait().catch((error: unknown) => error);
            await subject.handleInspect(() => {
              inspectEntered += 1;
              return Result.ok({
                active: BigInt(watchEntered - watchCleanups),
                cleanups: BigInt(watchCleanups),
                emitted: BigInt(watchEntered),
                starts: BigInt(watchEntered),
              });
            });
            await subject.handleWatch(async ({ input, emit, signal }) => {
              watchEntered += 1;
              try {
                await emit({
                  ...input,
                  index: 1n,
                  padding: "stage",
                  payload: new Uint8Array([42]),
                  sourceGeneration: 1n,
                }).orThrow();
                if (!signal.aborted) {
                  await new Promise<void>((resolve) =>
                    signal.addEventListener("abort", () => resolve(), {
                      once: true,
                    }),
                  );
                }
              } finally {
                watchCleanups += 1;
              }
            });
            observer = await connectObserver();
            await subject.advance({}).orThrow();
            await subject.extend({}).orThrow();
            assert.deepEqual(
              extendsEntered,
              1,
              "Extend really reaches its handler",
            );
            await observer.inspect(key).orThrow();
            assert.deepEqual(inspectEntered, 1);
            const before = await attachmentsFor(
              runtime,
              subjectContract.identity,
            );
            assert.deepEqual(
              before.length,
              1,
              "no narrower baseline generation exists",
            );
            const wide = before[0];
            const wideSockets = admittedConnections(
              await completeBrokerInventory(system),
              new Set([wide.attachmentId]),
            );
            assert.deepEqual(wideSockets.length, 1);
            const wideKey = brokerConnectionKey(wideSockets[0]);
            const serverId = wideSockets[0].server;
            const inventory = () =>
              completeBrokerInventory(system, {
                requiredServerIds: [serverId],
              });
            // Application proofs can renew on the same admitted socket. Identify
            // its physical proxy by the real Inspect request it just served, not
            // by equating a request proof with the historical CONNECT digest.
            const inspectRequests = gate
              .connections()
              .flatMap((connection) =>
                connection.outboundContexts.filter((out) =>
                  out.subject.endsWith(".Inspect"),
                ),
              );
            assert.deepEqual(inspectRequests.length, 1);
            const wideProxies = gate
              .connections()
              .filter(
                (connection) =>
                  !connection.closed &&
                  connection.deliveries.some(
                    (delivery) =>
                      delivery.subject === inspectRequests[0].subject,
                  ),
              );
            assert.deepEqual(
              wideProxies.length,
              1,
              "the real served Inspect identifies exactly one wide socket proxy",
            );
            const wideProxy = wideProxies[0];
            const baselineCaller = advanceCallers.at(-1);
            assert(baselineCaller?.type === "verified");
            console.log(
              JSON.stringify({
                case: closeWhileHeld ? "B" : "A",
                wideKey,
                admittedDigest: wide.attachmentId,
                advanceDigest: baselineCaller.authorityDigest,
                wideProxyId: wideProxy.id,
                outbound: wideProxy.outboundContexts.filter(
                  (out) =>
                    out.subject.endsWith(".Advance") ||
                    out.subject.endsWith(".Extend"),
                ),
                inspectDeliveries: wideProxy.deliveries.filter(
                  (delivery) => delivery.subject === inspectRequests[0].subject,
                ),
              }),
            );
            const appSubs = new Set(
              wideProxy.subs
                .filter(
                  (sub) =>
                    !sub.subject.startsWith("_INBOX.") &&
                    !sub.subject.startsWith("$"),
                )
                .map((sub) => sub.subject),
            );
            assert([...appSubs].some((route) => route.endsWith(".Inspect")));
            assert([...appSubs].some((route) => route.endsWith(".Watch")));
            const priorProxyIds = new Set(
              gate.connections().map((conn) => conn.id),
            );
            let heldInfo: WatchInfo | undefined;
            // getPushConsumer performs an earlier INFO before subscribing. Hold
            // only the real post-subscription INFO on a newly opened connection.
            hold = gate.armResponseHold(
              "$JS.API.CONSUMER.INFO.",
              undefined,
              (body) => {
                const info = JSON.parse(
                  new TextDecoder().decode(body),
                ) as WatchInfo;
                if (
                  info.push_bound !== true ||
                  info.config?.filter_subject !==
                    "$KV.TRELLIS_AUTH_REVOCATIONS.>"
                )
                  return false;
                const owner = gate
                  .connections()
                  .find(
                    (conn) =>
                      !conn.closed &&
                      !priorProxyIds.has(conn.id) &&
                      conn.subs.some(
                        (sub) => sub.subject === info.config?.deliver_subject,
                      ),
                  );
                if (!owner) return false;
                heldInfo = info;
                return true;
              },
            );
            let held: Awaited<typeof hold.held> | undefined;
            let holdFailure: unknown;
            void hold.held.then(
              (value) => (held = value),
              (error) => (holdFailure = error),
            );
            const page = await runtime.callAdminRpc(
              "authCapabilityGrantsList",
              {
                capabilityId: "runtime-trellis.transport_growth@v1::extend",
              },
            );
            const grant = page.items[0];
            assert(grant);
            await runtime.callAdminRpc("authCapabilityGrantsRevoke", {
              grantId: grant.grantId,
              expectedRevision: grant.revision,
              idempotencyKey: ulid(),
            });
            const withheld = await runtime.waitFor(
              () => {
                if (holdFailure) throw holdFailure;
                return held;
              },
              { timeoutMs: 90_000, intervalMs: 25 },
            );
            const stagedAttachments = await attachmentsFor(
              runtime,
              subjectContract.identity,
            );
            assert.deepEqual(stagedAttachments.length, 1);
            const staged = stagedAttachments[0];
            assert(staged.attachmentId !== wide.attachmentId);
            assert.deepEqual(staged.runtimeId, wide.runtimeId);
            const stageInventory = await inventory();
            assert(
              !stageInventory.some(
                (conn) => brokerConnectionKey(conn) === wideKey,
              ),
              "the only wide generation is physically gone before promotion",
            );
            const stageSockets = admittedConnections(
              stageInventory,
              new Set([staged.attachmentId]),
            );
            assert.deepEqual(stageSockets.length, 1);
            const stageKey = brokerConnectionKey(stageSockets[0]);
            console.log(
              JSON.stringify({
                case: closeWhileHeld ? "B" : "A",
                wideKey,
                stageKey,
                stageProxyId: withheld.connectionId,
                logical: wide.runtimeId,
                stageDigest: staged.attachmentId,
                heldSubject: withheld.requestSubject,
              }),
            );
            const stageProxy = gate.connection(withheld.connectionId);
            assert(stageProxy && !stageProxy.closed);
            assert.deepEqual(
              stageProxy.subs.filter((sub) => appSubs.has(sub.subject)),
              [],
              "a private stage has no application RPC/Live intake while warm INFO is withheld",
            );
            assert.deepEqual(
              gate
                .connections()
                .filter(
                  (conn) =>
                    !conn.closed &&
                    conn.subs.some((sub) => appSubs.has(sub.subject)),
                )
                .map((conn) => conn.id),
              [],
              "no new current application socket exists while the stage is warming",
            );
            assert.deepEqual(
              // A revoked socket can briefly attempt a TCP reconnect then close
              // without any observable NATS work. Count admitted preparations by
              // their real admission probe, not every accepted TCP connection.
              gate
                .connections()
                .filter(
                  (conn) =>
                    !priorProxyIds.has(conn.id) &&
                    conn.outboundContexts.some(
                      (out) => out.subject === "$SYS.REQ.USER.INFO",
                    ),
                )
                .map((conn) => conn.id),
              [withheld.connectionId],
              "exactly one new native connection reaches the real admission probe",
            );

            if (closeWhileHeld) {
              // Start the five-second bound at explicit logical close, not after
              // awaiting cleanup. A warm request timeout must not satisfy this.
              const closeStarted = Date.now();
              const infoTimeout = jetstream(system).getOptions().timeout;
              assert(infoTimeout !== undefined && heldInfo?.created);
              // Creation precedes getPushConsumer, subscription and the held INFO
              // request. This conservative deadline proves close wins before that
              // real request could time out; the five-second close bound remains.
              const warmDeadline =
                Date.parse(heldInfo.created) + infoTimeout - 250;
              const closing = subject.connection.close();
              void closing.catch(() => undefined);
              await runtime.waitFor(
                async () => {
                  const present = await inventory();
                  return !present.some(
                    (conn) => brokerConnectionKey(conn) === stageKey,
                  );
                },
                {
                  timeoutMs: Math.max(1, closeStarted + 5_000 - Date.now()),
                  intervalMs: 25,
                },
              );
              assert(
                Date.now() - closeStarted < 5_000,
                "the staged broker socket must disappear within five seconds of logical close",
              );
              assert(
                Date.now() < warmDeadline,
                "broker disappearance must precede the withheld INFO timeout",
              );
              console.log(
                JSON.stringify({
                  case: "B",
                  stageKey,
                  closeToBrokerAbsenceMs: Date.now() - closeStarted,
                }),
              );
              await closing;
              await subject.stop();
              await subjectExit;
              assert.deepEqual(
                gate
                  .connections()
                  .filter(
                    (conn) =>
                      !conn.closed &&
                      conn.subs.some((sub) => appSubs.has(sub.subject)),
                  )
                  .map((conn) => conn.id),
                [],
              );
              await hold.release();
              // Repeated real inventory and allowed admin activity bound the
              // late-response observation while this same TS process remains alive.
              const observeUntil = Date.now() + 10_000;
              while (Date.now() < observeUntil) {
                await runtime.callAdminRpc("authCapabilitiesList", {});
                const current = await inventory();
                const subjectDigests = new Set([
                  wide.attachmentId,
                  staged.attachmentId,
                  ...(
                    await attachmentsFor(runtime, subjectContract.identity)
                  ).map((item) => item.attachmentId),
                ]);
                assert.deepEqual(
                  admittedConnections(current, subjectDigests),
                  [],
                  "late INFO must not resurrect a subject attachment",
                );
                assert.deepEqual(
                  gate
                    .connections()
                    .filter(
                      (conn) => !priorProxyIds.has(conn.id) && !conn.closed,
                    )
                    .map((conn) => conn.id),
                  [],
                  "late INFO must not open another native candidate",
                );
                assert.deepEqual(inspectEntered, 1);
                assert.deepEqual(watchEntered, 0);
              }
            } else {
              await hold.release();
              await runtime.waitFor(
                () => {
                  const conn = gate.connection(withheld.connectionId);
                  return (
                    conn &&
                    !conn.closed &&
                    [...appSubs].every((route) =>
                      conn.subs.some((sub) => sub.subject === route),
                    )
                  );
                },
                { timeoutMs: 30_000, intervalMs: 25 },
              );
              await subject.advance({}).orThrow();
              const installed = advanceCallers.at(-1);
              assert(installed?.type === "verified");
              assert.deepEqual(
                gate
                  .connection(withheld.connectionId)
                  ?.outboundContexts.findLast((message) =>
                    message.subject.endsWith(".Advance"),
                  )?.context,
                installed.authorityDigest,
                "the staged socket publishes the authority verified by the real handler",
              );
              console.log(
                JSON.stringify({
                  case: "A",
                  stageKey,
                  installedHandlerDigest: installed.authorityDigest,
                }),
              );
              await observer.inspect(key).orThrow();
              assert.deepEqual(inspectEntered, 2);
              const feed = await observer.watch(key).orThrow();
              try {
                const frame = await feed[Symbol.asyncIterator]().next();
                assert.deepEqual(frame.value?.padding, "stage");
                assert.deepEqual(frame.value?.payload, new Uint8Array([42]));
                const receipt = await feed.close().orThrow();
                assert.deepEqual(receipt.remote, "confirmed");
                assert.deepEqual(receipt.cleanup, "complete");
                await feed.closed;
                assert.deepEqual(watchCleanups, 1);
              } finally {
                await feed
                  .close()
                  .orThrow()
                  .catch(() => undefined);
              }
              const progress = await subject.progress({}).orThrow();
              try {
                assert.deepEqual(
                  (await progress[Symbol.asyncIterator]().next()).value?.value,
                  "progress",
                );
                const receipt = await progress.close().orThrow();
                assert.deepEqual(receipt.remote, "confirmed");
                assert.deepEqual(receipt.cleanup, "complete");
                await progress.closed;
              } finally {
                await progress
                  .close()
                  .orThrow()
                  .catch(() => undefined);
              }
              const refused = await subject.extend({});
              assert(refused.isErr(), "removed Extend must be refused");
              assert.deepEqual(
                extendsEntered,
                1,
                "removed Extend cannot reach its handler",
              );
              const finalInventory = await inventory();
              assert(
                !finalInventory.some(
                  (conn) => brokerConnectionKey(conn) === wideKey,
                ),
              );
              assert.deepEqual(
                admittedConnections(
                  finalInventory,
                  new Set([staged.attachmentId]),
                ).map(brokerConnectionKey),
                [stageKey],
                "the same staged physical socket is the sole narrowed attachment",
              );
              const finalAttachments = await attachmentsFor(
                runtime,
                subjectContract.identity,
              );
              assert.deepEqual(
                finalAttachments.map((item) => [
                  item.runtimeId,
                  item.attachmentId,
                  item.attachmentId,
                ]),
                [[wide.runtimeId, staged.attachmentId, staged.attachmentId]],
              );
              assert.deepEqual(
                gate
                  .connections()
                  .filter(
                    (conn) =>
                      !priorProxyIds.has(conn.id) &&
                      conn.outboundContexts.some(
                        (out) => out.subject === "$SYS.REQ.USER.INFO",
                      ),
                  )
                  .map((conn) => conn.id),
                [withheld.connectionId],
                "adoption must not create a second socket",
              );
            }
          } finally {
            // A failed assertion must never leave the real server reply retained.
            await hold?.release().catch(() => undefined);
            await observer?.connection.close().catch(() => undefined);
            await subject?.connection.close().catch(() => undefined);
            await subject?.stop();
            await subjectExit;
            await target.connection.close().catch(() => undefined);
            await target.stop();
            await targetExit;
            await system.close();
          }
        },
        {
          authorization: {
            contextLifetimeSeconds: 55,
            refreshLeadSeconds: 12,
            refreshJitterSeconds: 0,
            minimumContextLifetimeSeconds: 20,
            allowedClockSkewSeconds: 2,
          },
          interruptibleNativeProxy: true,
        },
      );
    },
  );
}
