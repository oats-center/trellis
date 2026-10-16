/**
 * Real-boundary Rust transport-generation acceptance.
 *
 * A generated Rust `TransportGrowthSubject` service connects with capability A
 * admitted and optional capability B declined. On its initial generation (G1) it
 * holds one accepted `Advance` RPC in flight and keeps two Live sessions open —
 * one it observes (`Progress`) and one it serves (`liveprobe Watch`). A real
 * deployment consent then grows B, which publishes a wider generation (G2) while
 * the held G1 work is still outstanding. The grown call completes on G2 without
 * disturbing the held G1 call or either G1 Live session. Releasing the held call
 * and closing its Live sessions lets the exact G1 physical socket reap from the
 * broker. A subsequent ordinary renewal is proven *installed*, not merely
 * issued, by the caller digest a real RPC carries; fresh RPC and Live traffic
 * then succeed with the baseline socket still gone and no extra renewal socket.
 *
 * Physical identity is `connectionId`; `runtimeConnectionId` is the SDK logical
 * connection and must stay stable across growth. Physical socket presence and
 * reclamation are attributed from the broker's own authenticated `CONNZ`
 * inventory, never from the auth-side connection presence.
 */

import assert from "node:assert/strict";
import { TextDecoderStream } from "node:stream/web";
import { test } from "node:test";
import { ulid } from "ulid";
import { Readable, Writable } from "node:stream";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import type { TrellisTestRuntime } from "@oatscenter/trellis-testkit";

import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import {
  admittedConnections,
  brokerConnectionKey,
  readRuntimeBrokerInventory,
  readRuntimeServiceAttachments,
} from "./_support/broker_inventory.ts";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";
import { spawnTracked } from "./_support/process.ts";

/** Optional capability id declined at consent and grown later. */
const CAPABILITY_EXTEND = "runtime-trellis.transport_growth@v1::extend";

/**
 * One deployment carries one participant assignment, so the subject and its
 * target use separate deployments and nothing relies on the runtime default.
 */
const SUBJECT_DEPLOYMENT = "tg-subject";
const TARGET_DEPLOYMENT = "tg-target";

/** Short lifetimes so the case crosses real ordinary renewals quickly. */
const runtimeOptions = {
  authorization: {
    contextLifetimeSeconds: 80,
    refreshLeadSeconds: 15,
    refreshJitterSeconds: 0,
    minimumContextLifetimeSeconds: 46,
  },
};

/** A bounded command channel to the Rust growth leg. */
type GrowthLeg = {
  waitFor(marker: string, count?: number): Promise<void>;
  send(command: string): Promise<void>;
  latestObservedIndex(): number | undefined;
  /** Number of protocol lines emitted so far for one marker prefix. */
  count(marker: string): number;
};

/**
 * Spawns the Rust growth leg, runs `body`, and always reaps the process on a
 * bounded exit.
 */
async function withTransportGrowthLeg(
  runtime: TrellisTestRuntime,
  seed: string,
  deadlineMs: number,
  body: (leg: GrowthLeg) => Promise<void>,
): Promise<void> {
  const child = spawnTracked(
    "setsid",
    rustFixtureArgv("transport_growth_subject"),
    {
      env: {
        ...process.env,
        TRELLIS_URL: runtime.oauthIssuer,
        TRELLIS_IDENTITY_SEED: seed,
      },
    },
  );
  child.stderr.pipe(process.stderr);
  const lines: string[] = [];
  let output = "";
  let exited = false;
  const status = child.exited.then((result) => {
    exited = true;
    return result;
  });
  const stdin = Writable.toWeb(child.stdin).getWriter();
  const drain = (async () => {
    const reader = Readable.toWeb(child.stdout)
      .pipeThrough(new TextDecoderStream())
      .getReader();
    let buffer = "";
    while (true) {
      const chunk = await reader.read();
      if (chunk.done) break;
      output += chunk.value;
      buffer += chunk.value;
      const parts = buffer.split("\n");
      buffer = parts.pop() ?? "";
      for (const part of parts) {
        const line = part.trim();
        if (line.length > 0) lines.push(line);
      }
    }
    const tail = buffer.trim();
    if (tail.length > 0) lines.push(tail);
  })();
  const drainSettled = drain.catch(() => {});
  const deadline = Date.now() + deadlineMs;
  const remainingMs = (): number => Math.max(0, deadline - Date.now());

  const leg: GrowthLeg = {
    async waitFor(marker: string, count = 1): Promise<void> {
      await runtime.waitFor(
        () => {
          const seen = lines.filter((line) => line.startsWith(marker)).length;
          if (seen >= count) return true;
          const error = lines.find(
            (line) =>
              line.startsWith("TRANSPORT_GROWTH_ERROR ") ||
              line.startsWith("TRANSPORT_GROWTH_ADVANCE_ERROR "),
          );
          if (error !== undefined) {
            throw new Error(`Rust transport growth leg failed: ${error}`);
          }
          return undefined;
        },
        { timeoutMs: remainingMs(), intervalMs: 50 },
      );
    },
    async send(command: string): Promise<void> {
      await stdin.write(new TextEncoder().encode(`${command}\n`));
    },
    latestObservedIndex(): number | undefined {
      let latest: number | undefined;
      for (const line of lines) {
        if (line.startsWith("TRANSPORT_GROWTH_OBSERVED ")) {
          const value = Number(line.slice("TRANSPORT_GROWTH_OBSERVED ".length));
          if (Number.isFinite(value)) latest = value;
        }
      }
      return latest;
    },
    count(marker: string): number {
      return lines.filter((line) => line.startsWith(marker)).length;
    },
  };

  try {
    try {
      await body(leg);
    } catch (cause) {
      const markers = lines.filter(
        (line) => !line.startsWith("TRANSPORT_GROWTH_OBSERVED "),
      );
      throw new Error(
        `${
          cause instanceof Error ? cause.message : String(cause)
        }\n--- growth leg markers ---\n${markers.join("\n")}`,
        { cause },
      );
    }
    const finished =
      lines.includes("TRANSPORT_GROWTH_DONE") ||
      lines.includes("TRANSPORT_GROWTH_CLOSED");
    if (!finished && !exited) {
      await leg.send("EXIT");
      await leg.waitFor("TRANSPORT_GROWTH_DONE");
    }
    await runtime.waitFor(() => (exited ? true : undefined), {
      timeoutMs: remainingMs(),
      intervalMs: 50,
    });
    const finalStatus = await status;
    assert.equal(
      finalStatus.code,
      0,
      `Rust transport growth leg failed: ${output}`,
    );
  } finally {
    try {
      await stdin.close();
    } catch {
      // stdin may already be closed once the process exited.
    }
    if (!exited && child.pid !== undefined) {
      try {
        process.kill(-child.pid, "SIGKILL");
      } catch {
        // the process may have exited between the check and the signal.
      }
    }
    await Promise.allSettled([status, drainSettled]);
  }
}

test("Rust transport generations adopt grown authority automatically without an extra renewal socket", async () => {
  await withTrellisRuntime(async (runtime) => {
    const subjectContract = participants.TransportGrowthSubject.participant;
    const targetContract = participants.TransportGrowthTarget.participant;

    // The target serves both capabilities through its generated handlers.
    await runtime.contracts.apply({
      deployment: TARGET_DEPLOYMENT,
      contract: targetContract,
    });
    const targetInstance = await runtime.services.createInstance({
      deployment: TARGET_DEPLOYMENT,
      name: "tg-target",
      contract: targetContract,
    });
    const target = await TrellisService.connect({
      trellisUrl: runtime.oauthIssuer,
      participant: targetContract,
      name: "tg-target",
      seed: targetInstance.seed,
    }).orThrow();
    let advanceCalls = 0;
    let extendCalls = 0;
    // The verified caller of every accepted `Advance`, in handler-entry order, so
    // the exact authorization context a real RPC carried is observable.
    const advanceCallers: { type: string; contextDigest: string | null }[] = [];
    // Exactly one `Advance` — the one the subject holds on G1 — parks here until
    // the test releases it. Later calls complete normally.
    let holdAdvance = false;
    const advanceReleased = Promise.withResolvers<void>();
    await target.handleAdvance(async ({ context }) => {
      advanceCalls += 1;
      const caller = context.caller;
      advanceCallers.push({
        type: caller.type,
        contextDigest:
          caller.type === "verified" ? caller.authorityDigest : null,
      });
      if (holdAdvance) {
        holdAdvance = false;
        await advanceReleased.promise;
      }
      return Result.ok({});
    });
    await target.handleExtend(() => {
      extendCalls += 1;
      return Result.ok({});
    });
    // A plain periodic Live source the subject observes across growth. The test
    // can stop it to drive a natural, provider-initiated terminal, then resume it
    // so a later observation is not trivially observing an already-ended source.
    let progressRunning = true;
    let progressIndex = 0;
    await target.handleProgress(async ({ emit, signal }) => {
      while (!signal.aborted && progressRunning) {
        progressIndex += 1;
        await emit({ value: `${progressIndex}` });
        await new Promise((resolve) => setTimeout(resolve, 150));
      }
    });
    const targetExit = target.wait().catch((error: unknown) => error);

    // The subject starts approved with the optional capability declined.
    await runtime.contracts.install({ contract: subjectContract });
    await runtime.contracts.apply({
      deployment: SUBJECT_DEPLOYMENT,
      contract: subjectContract,
    });
    const subjectInstance = await runtime.services.createInstance({
      deployment: SUBJECT_DEPLOYMENT,
      name: "tg-subject",
      contract: subjectContract,
    });
    const subjectId = subjectContract.identity;
    const grants = await runtime.callAdminRpc("authCapabilityGrantsList", {
      capabilityId: CAPABILITY_EXTEND,
    });
    const grant = grants.items[0];
    assert(grant);
    await runtime.callAdminRpc("authCapabilityGrantsRevoke", {
      grantId: grant.grantId,
      expectedRevision: grant.revision,
      idempotencyKey: ulid(),
    });

    // The subject's inbound Live consumer, kept for the whole case so its
    // receiving-generation behaviour is exercised across growth and renewal.
    const observer = await runtime.connectClient({
      name: "tg-subject-live",
      contract: apps.LiveProbeCaller.request,
    });

    /** The subject's admitted physical attachments. */
    const attachments = () => readRuntimeServiceAttachments(runtime, subjectId);

    try {
      await withTransportGrowthLeg(
        runtime,
        subjectInstance.seed,
        120_000,
        async (leg) => {
          // The always-granted capability works on the initial attachment.
          await leg.waitFor("TRANSPORT_GROWTH_ADVANCE_OK");
          const before = await attachments();
          assert.deepEqual(
            before.length,
            1,
            "a fresh subject has exactly one admitted attachment",
          );
          const logical = before[0].runtimeId;
          const baselineDigest = before[0].attachmentId;

          // The exact initial physical generation (G1) from the broker's own
          // inventory: its callout-owned socket is the single one admitted under
          // the baseline context. The read pins the system connection's own
          // advertised server, and that exact server is retained for every later
          // absence assertion so a dropped discovery reply cannot read as
          // absence.
          const baselineSockets = admittedConnections(
            await readRuntimeBrokerInventory(runtime),
            new Set([baselineDigest]),
          );
          assert.deepEqual(
            baselineSockets.length,
            1,
            "the baseline generation must own exactly one physical socket",
          );
          const g1Key = brokerConnectionKey(baselineSockets[0]);
          const brokerServerId = baselineSockets[0].server;

          // Open a Live observation on the initial generation. It must keep
          // receiving frames across the automatic growth and pin its generation.
          await leg.send("OBSERVE");
          await leg.waitFor("TRANSPORT_GROWTH_OBSERVING");
          await runtime.waitFor(
            () => ((leg.latestObservedIndex() ?? -1) >= 0 ? true : undefined),
            { timeoutMs: 30_000, intervalMs: 50 },
          );
          const observedBefore = leg.latestObservedIndex();
          assert(
            observedBefore !== undefined,
            "the Live observation must start delivering frames",
          );

          // The Rust subject also serves the generated `liveprobe Watch` source.
          // Observe it from a real LiveProbeCaller so the Rust provider's
          // receiving-generation owner controls are exercised across the growth.
          const feed = await observer
            .watch({
              runId: ulid(),
              streamId: "growth",
            })
            .orThrow();
          const liveFrames: bigint[] = [];
          const liveTask = (async () => {
            for await (const frame of feed) liveFrames.push(frame.index);
          })().catch(() => undefined);
          await runtime.waitFor(
            () => (liveFrames.length > 0 ? true : undefined),
            { timeoutMs: 30_000, intervalMs: 50 },
          );
          const liveBefore = liveFrames.length;

          // Start the held RPC near ordinary renewal, so its existing deadline
          // spans maintenance rather than most of an issued authority lifetime.
          const initialSessions = await runtime.callAdminRpc(
            "authAuthorizationSessionsList",
            {},
          );
          const initialSession = initialSessions.items.find(
            (session) =>
              session.authorizationSessionId ===
              before[0].authorizationSessionId,
          );
          assert(initialSession?.maximumContextExpiresAt);
          const initialExpiry = Date.parse(
            initialSession.maximumContextExpiresAt,
          );
          await runtime.waitFor(() => Date.now() >= initialExpiry - 40_000, {
            timeoutMs: 45_000,
          });

          // Hold one real, accepted `Advance` RPC on G1. The target handler parks
          // until the test releases it, so stdin stays free for the rest of the
          // protocol. The handler entry is the observable, not the command echo.
          holdAdvance = true;
          await leg.send("START_ADVANCE");
          await runtime.waitFor(() => (advanceCalls >= 2 ? true : undefined), {
            timeoutMs: 30_000,
            intervalMs: 50,
          });
          assert.deepEqual(
            leg.count("TRANSPORT_GROWTH_ADVANCE_OK"),
            1,
            "the held Advance must not complete before it is released",
          );

          // Grow the optional capability through the real deployment consent. It
          // publishes a wider authority and opens G2.
          await runtime.callAdminRpc("authCapabilityGrantsGrant", {
            principalId: grant.principalId,
            capabilityId: CAPABILITY_EXTEND,
            expectedRevision: 0n,
            idempotencyKey: ulid(),
          });
          await runtime.waitFor(
            async () => ((await attachments()).length >= 2 ? true : undefined),
            { timeoutMs: 45_000, intervalMs: 100 },
          );

          // Fresh grown work completes on G2 while the G1 `Advance` is still held.
          await leg.send("AVAILABLE");
          await leg.waitFor("TRANSPORT_GROWTH_EXTEND_AVAILABLE");
          await leg.send("EXTEND");
          await leg.waitFor("TRANSPORT_GROWTH_EXTEND_OK");
          assert.deepEqual(
            extendCalls,
            1,
            "the grown capability ran exactly once",
          );

          const after = await attachments();
          const grown = after.find(
            (item) => item.attachmentId !== baselineDigest,
          );
          assert(
            grown !== undefined,
            "growth must admit a generation with a newer context",
          );
          const grownDigest = grown.attachmentId;
          // The wider generation (G2) is a single exact physical socket.
          const grownSockets = await runtime.waitFor(
            async () => {
              const inventory = await readRuntimeBrokerInventory(runtime, {
                requiredServerIds: [brokerServerId],
              });
              const sockets = admittedConnections(
                inventory,
                new Set([grownDigest]),
              );
              return sockets.length === 1 ? sockets : undefined;
            },
            { timeoutMs: 45_000, intervalMs: 200 },
          );
          const g2Key = brokerConnectionKey(grownSockets[0]);
          // The wider generation is admitted with a new exact context and the
          // original attachment survives: make-before-break, not a replacement.
          assert(
            after.some((item) => item.attachmentId === before[0].attachmentId),
            "the original attachment must keep serving covered work",
          );
          assert.deepEqual(
            after[0].runtimeId,
            logical,
            "the logical connection identity must be stable across growth",
          );

          // The held G1 call is still outstanding and both G1 Live sessions keep
          // delivering: growth did not move or interrupt accepted baseline work.
          assert.deepEqual(
            advanceCalls,
            2,
            "the held Advance is still in flight",
          );
          assert.deepEqual(
            leg.count("TRANSPORT_GROWTH_ADVANCE_OK"),
            1,
            "the held Advance still has not completed",
          );
          await runtime.waitFor(
            () =>
              (leg.latestObservedIndex() ?? -1) > (observedBefore ?? -1)
                ? true
                : undefined,
            { timeoutMs: 30_000, intervalMs: 50 },
          );
          await runtime.waitFor(
            () => (liveFrames.length > liveBefore ? true : undefined),
            { timeoutMs: 30_000, intervalMs: 50 },
          );

          // The exact G1 socket stays physically admitted while its accepted work
          // is outstanding: a generation is not closed under held work.
          const whileHeld = await readRuntimeBrokerInventory(runtime, {
            requiredServerIds: [brokerServerId],
          });
          assert(
            whileHeld.some((item) => brokerConnectionKey(item) === g1Key),
            "the baseline generation must stay admitted while its RPC is held",
          );

          // Release the held RPC exactly once: it completes on G1 and is not
          // retried or re-executed.
          advanceReleased.resolve();
          await leg.waitFor("TRANSPORT_GROWTH_ADVANCE_OK", 2);
          assert.deepEqual(
            advanceCalls,
            2,
            "the held Advance must complete exactly once",
          );

          // Close the G1 Live sessions, asserting the *confirmed* remote
          // disposition: the close receipt reports whether the receiving
          // generation settled it, not merely that the local promise resolved.
          const receipt = await feed.close().orThrow();
          assert.deepEqual(
            receipt.remote,
            "confirmed",
            "the Live close must be remotely confirmed on the receiving generation",
          );
          assert.deepEqual(
            receipt.cleanup,
            "complete",
            "the closed observation must report a complete remote cleanup",
          );
          await liveTask;
          await leg.send("OBSERVE_CLOSE");
          await leg.waitFor("TRANSPORT_GROWTH_OBSERVED_CLOSED");

          // With every G1 session closed, the exact baseline socket reaps from the
          // broker's complete inventory. Absence is only ever read from a
          // validated complete reply, never inferred from an error.
          try {
            await runtime.waitFor(
              async () => {
                const inventory = await readRuntimeBrokerInventory(runtime, {
                  requiredServerIds: [brokerServerId],
                });
                return inventory.some(
                  (item) => brokerConnectionKey(item) === g1Key,
                )
                  ? undefined
                  : true;
              },
              { timeoutMs: 45_000, intervalMs: 400 },
            );
          } catch (cause) {
            const inventory = await readRuntimeBrokerInventory(runtime, {
              requiredServerIds: [brokerServerId],
            });
            throw new Error(
              `the exact baseline socket ${g1Key} was not reclaimed; callout sockets: ${JSON.stringify(
                inventory
                  .filter((item) =>
                    item.authorizedUser.startsWith("trellis.auth.v1:"),
                  )
                  .map(brokerConnectionKey)
                  .sort(),
              )}`,
              { cause },
            );
          }
          const reaped = await readRuntimeBrokerInventory(runtime, {
            requiredServerIds: [brokerServerId],
          });
          assert(
            !reaped.some((item) => brokerConnectionKey(item) === g1Key),
            "the exact baseline socket must be gone from the broker",
          );
          assert(
            admittedConnections(reaped, new Set([baselineDigest])).length === 0,
            "no socket may remain admitted under the baseline context",
          );

          // A retained handle whose session ends on its own must also release its
          // generation: open a fresh observation, then let the provider end its
          // source so the observation reaches a natural terminal.
          const beforeEnd = leg.latestObservedIndex() ?? -1;
          await leg.send("OBSERVE");
          await runtime.waitFor(
            () =>
              (leg.latestObservedIndex() ?? -1) > beforeEnd ? true : undefined,
            { timeoutMs: 30_000, intervalMs: 50 },
          );
          progressRunning = false;
          await leg.waitFor("TRANSPORT_GROWTH_OBSERVED_ENDED");

          // Observe renewal through authenticated RPCs, not private Auth storage.
          await leg.send("ADVANCE");
          await leg.waitFor("TRANSPORT_GROWTH_ADVANCE_OK", 3);
          const beforeRenewal = advanceCallers.at(-1)?.contextDigest;
          assert(beforeRenewal);
          const sessions = await runtime.callAdminRpc(
            "authAuthorizationSessionsList",
            {},
          );
          const current = sessions.items.find(
            (session) =>
              session.runtimeId === logical && session.state === "active",
          );
          assert(current?.maximumContextExpiresAt);
          let installedRenewedDigest: string | undefined;
          const installDeadline = Date.parse(current.maximumContextExpiresAt);
          while (Date.now() < installDeadline) {
            const expected = advanceCalls + 1;
            await leg.send("ADVANCE");
            await runtime.waitFor(
              () => (advanceCalls >= expected ? true : undefined),
              { timeoutMs: 15_000, intervalMs: 50 },
            );
            const latest = advanceCallers[advanceCalls - 1];
            if (
              latest?.type === "verified" &&
              latest.contextDigest !== null &&
              latest.contextDigest !== beforeRenewal
            ) {
              installedRenewedDigest = latest.contextDigest;
              break;
            }
            await new Promise((resolve) => setTimeout(resolve, 500));
          }
          assert(
            installedRenewedDigest !== undefined,
            "a real Advance must carry the newly installed renewed context",
          );

          // The ordinary renewal must not have opened another physical socket:
          // the surviving grown generation is still the only one, from the
          // broker's own inventory.
          const afterRenewalInventory = await readRuntimeBrokerInventory(
            runtime,
            { requiredServerIds: [brokerServerId] },
          );
          assert(
            admittedConnections(
              afterRenewalInventory,
              new Set([baselineDigest]),
            ).length === 0,
            "the baseline context must stay unadmitted after renewal",
          );
          assert.deepEqual(
            admittedConnections(
              afterRenewalInventory,
              new Set([grownDigest]),
            ).map(brokerConnectionKey),
            [g2Key],
            "the ordinary renewal must not open another socket",
          );

          // Fresh Live traffic succeeds on the surviving generation. The observed
          // source is resumed first so the fresh outbound observation is not
          // trivially observing an already-ended source.
          progressRunning = true;
          const freshObservedBefore = leg.latestObservedIndex() ?? -1;
          await leg.send("OBSERVE");
          await runtime.waitFor(
            () =>
              (leg.latestObservedIndex() ?? -1) > freshObservedBefore
                ? true
                : undefined,
            { timeoutMs: 30_000, intervalMs: 50 },
          );

          // The incoming Feed served by the Rust subject also opens on the
          // surviving generation and closes with a confirmed remote cleanup.
          const freshFeed = await observer
            .watch({
              runId: ulid(),
              streamId: "growth",
            })
            .orThrow();
          const freshFrames: bigint[] = [];
          const freshTask = (async () => {
            for await (const frame of freshFeed) freshFrames.push(frame.index);
          })().catch(() => undefined);
          await runtime.waitFor(
            () => (freshFrames.length > 0 ? true : undefined),
            { timeoutMs: 30_000, intervalMs: 50 },
          );
          const freshReceipt = await freshFeed.close().orThrow();
          assert.deepEqual(
            freshReceipt.remote,
            "confirmed",
            "the fresh Live close must be remotely confirmed",
          );
          assert.deepEqual(
            freshReceipt.cleanup,
            "complete",
            "the fresh Live close must report a complete remote cleanup",
          );
          await freshTask;

          // A fresh RPC after the renewal still succeeds.
          const freshCalls = advanceCalls + 1;
          await leg.send("ADVANCE");
          await runtime.waitFor(
            () => (advanceCalls >= freshCalls ? true : undefined),
            { timeoutMs: 30_000, intervalMs: 50 },
          );

          // The baseline stays gone and the ordinary renewal opened no extra
          // socket: the surviving admitted attachment owns exactly the one
          // physical socket G2 opened, proven from the broker's full inventory.
          const finalInventory = await readRuntimeBrokerInventory(runtime, {
            requiredServerIds: [brokerServerId],
          });
          assert(
            !finalInventory.some((item) => brokerConnectionKey(item) === g1Key),
            "the exact baseline socket must stay reaped",
          );
          assert(
            admittedConnections(finalInventory, new Set([baselineDigest]))
              .length === 0,
            "the baseline context must stay unadmitted",
          );
          const finalAttachments = await attachments();
          assert.deepEqual(
            finalAttachments.length,
            1,
            "the grown attachment must be the only one left",
          );
          assert.deepEqual(
            finalAttachments[0].runtimeId,
            logical,
            "the logical connection identity must stay stable",
          );
          assert.deepEqual(
            finalAttachments[0].attachmentId,
            grownDigest,
            "the surviving attachment must keep its admitted grown context",
          );
          const survivingSockets = admittedConnections(
            finalInventory,
            new Set([grownDigest]),
          );
          assert.deepEqual(
            survivingSockets.map(brokerConnectionKey),
            [g2Key],
            "no ordinary renewal may open an extra socket",
          );

          // A logical close releases the surviving generation and does not
          // resurrect the reaped baseline.
          await leg.send("CLOSE");
          await leg.waitFor("TRANSPORT_GROWTH_CLOSED");
          await runtime.waitFor(
            async () => {
              return (await attachments()).length === 0 ? true : undefined;
            },
            { timeoutMs: 30_000 },
          );
        },
      );
    } finally {
      await observer.connection.close().catch(() => undefined);
      await target.connection.close().catch(() => undefined);
      await targetExit;
    }
  }, runtimeOptions);
});
