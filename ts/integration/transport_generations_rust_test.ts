/**
 * Real-boundary Rust transport-generation acceptance.
 *
 * A generated Rust `TransportGrowthSubject` service connects with capability A
 * admitted and optional capability B declined. After a real deployment consent
 * approves B, a B call on the same logical connection must adopt a wider
 * physical transport generation automatically — no explicit refresh — while A
 * keeps working and an ordinary renewal opens no extra socket.
 *
 * Physical identity is `connectionId`; `runtimeConnectionId` is the SDK logical
 * connection and must stay stable across growth.
 */

import { createClient } from "@libsql/client";
import { assert, assertEquals } from "@std/assert";
import { join } from "@std/path";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import type { TrellisTestRuntime } from "@oatscenter/trellis-testkit";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";

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
    contextLifetimeSeconds: 62,
    refreshLeadSeconds: 25,
    refreshJitterSeconds: 0,
    minimumContextLifetimeSeconds: 32,
  },
};

/** One admitted attachment as reported by the production admin surface. */
type Attachment = {
  participantId: string;
  runtimeConnectionId: string;
  connectionId: string;
  contextDigest: string;
};

/** A bounded command channel to the Rust growth leg. */
type GrowthLeg = {
  waitFor(marker: string, count?: number): Promise<void>;
  send(command: string): Promise<void>;
  latestObservedIndex(): number | undefined;
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
  const child = new Deno.Command("setsid", {
    args: rustFixtureArgv("transport_growth_subject"),
    env: { TRELLIS_URL: runtime.trellisUrl, TRELLIS_IDENTITY_SEED: seed },
    stdin: "piped",
    stdout: "piped",
    stderr: "inherit",
  }).spawn();
  const lines: string[] = [];
  let output = "";
  let exited = false;
  const status = child.status.then((result) => {
    exited = true;
    return result;
  });
  const stdin = child.stdin.getWriter();
  const drain = (async () => {
    const reader = child.stdout.pipeThrough(new TextDecoderStream())
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
          const error = lines.find((line) =>
            line.startsWith("TRANSPORT_GROWTH_ERROR ")
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
  };

  try {
    await body(leg);
    const finished = lines.includes("TRANSPORT_GROWTH_DONE") ||
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
    assert(finalStatus.success, `Rust transport growth leg failed: ${output}`);
  } finally {
    try {
      await stdin.close();
    } catch {
      // stdin may already be closed once the process exited.
    }
    if (!exited) {
      try {
        Deno.kill(-child.pid, "SIGKILL");
      } catch {
        // the process may have exited between the check and the signal.
      }
    }
    await Promise.allSettled([status, drainSettled]);
  }
}

Deno.test("Rust transport generations adopt grown authority automatically without an extra renewal socket", async () => {
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
      trellisUrl: runtime.trellisUrl,
      participant: targetContract,
      name: "tg-target",
      seed: targetInstance.seed,
    }).orThrow();
    let advanceCalls = 0;
    let extendCalls = 0;
    await target.handleAdvance(() => {
      advanceCalls += 1;
      return Result.ok({});
    });
    await target.handleExtend(() => {
      extendCalls += 1;
      return Result.ok({});
    });
    // A plain periodic Live source the subject observes across growth. The test
    // can stop it to drive a natural, provider-initiated terminal.
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
    const requested = await runtime.contracts.requestApply({
      deployment: SUBJECT_DEPLOYMENT,
      contract: subjectContract,
    });
    assert(
      requested.status === "approval_required",
      "the subject deployment must require an approval",
    );
    if (requested.status !== "approval_required") return;
    await runtime.contracts.approveApply(requested.pendingId, {
      excludeCapabilities: [CAPABILITY_EXTEND],
    });
    const subjectInstance = await runtime.services.createInstance({
      deployment: SUBJECT_DEPLOYMENT,
      name: "tg-subject",
      contract: subjectContract,
    });
    const subjectId = subjectContract.identity;

    /** The subject's admitted physical attachments. */
    const attachments = async (): Promise<Attachment[]> => {
      const page = await runtime.callAdminRpc("authConnectionsList", {}) as {
        items: Attachment[];
      };
      return page.items.filter((item) => item.participantId === subjectId);
    };

    const database = createClient({
      url: `file:${
        join(runtime.workdir, "trellis", "trellis.sqlite.platform")
      }`,
    });
    /** Distinct persisted context digests for the subject. */
    const renewalCount = async (): Promise<number> => {
      const result = await database.execute({
        sql:
          "SELECT COUNT(DISTINCT context_digest) AS count FROM auth_authorization_contexts WHERE participant_id = ?",
        args: [subjectId],
      });
      return Number(result.rows[0].count);
    };
    /** Waits until persisted issuance shows at least `minimum` renewals. */
    const waitForRenewals = async (minimum: number): Promise<void> => {
      await runtime.waitFor(
        async () => (await renewalCount()) >= minimum ? true : undefined,
        {
          timeoutMs: 45_000,
          intervalMs: 250,
        },
      );
    };

    try {
      await withTransportGrowthLeg(
        runtime,
        subjectInstance.seed,
        120_000,
        async (leg) => {
          // The always-granted capability works on the initial attachment.
          await leg.waitFor("TRANSPORT_GROWTH_ADVANCE_OK");
          const before = await attachments();
          assertEquals(
            before.length,
            1,
            "a fresh subject has exactly one admitted attachment",
          );
          const logical = before[0].runtimeConnectionId;

          // Open a Live observation on the initial generation. It must keep
          // receiving frames across the automatic growth and pin its generation.
          await leg.send("OBSERVE");
          await leg.waitFor("TRANSPORT_GROWTH_OBSERVING");
          await runtime.waitFor(
            () => (leg.latestObservedIndex() ?? -1) >= 0 ? true : undefined,
            { timeoutMs: 30_000, intervalMs: 50 },
          );
          const observedBefore = leg.latestObservedIndex();
          assert(
            observedBefore !== undefined,
            "the Live observation must start delivering frames",
          );

          // The Rust subject now serves the generated `liveprobe Watch` source.
          // Observe it from a real LiveProbeCaller so the Rust provider's
          // receiving-generation owner controls are exercised across the growth.
          const observer = await runtime.connectClient({
            name: "tg-subject-live",
            contract: participants.LiveProbeCaller.participant,
          });
          const feed = await observer.watch({
            runId: crypto.randomUUID(),
            streamId: "growth",
          }).orThrow();
          const liveFrames: bigint[] = [];
          const liveTask = (async () => {
            for await (const frame of feed) liveFrames.push(frame.index);
          })().catch(() => undefined);
          await runtime.waitFor(
            () => liveFrames.length > 0 ? true : undefined,
            { timeoutMs: 30_000, intervalMs: 50 },
          );
          const liveBefore = liveFrames.length;

          // An ordinary renewal must not replace or add a physical attachment.
          await waitForRenewals(2);
          assertEquals(
            (await attachments()).length,
            1,
            "ordinary renewal must not open another socket",
          );

          // Grow the optional capability through the real deployment consent.
          await runtime.contracts.apply({
            deployment: SUBJECT_DEPLOYMENT,
            contract: subjectContract,
          });

          // The application layer becomes authorized by the renewed context;
          // the transport then automatically adopts a wider generation for the
          // grown call, with no explicit refresh.
          await leg.send("AVAILABLE");
          await leg.waitFor("TRANSPORT_GROWTH_EXTEND_AVAILABLE");
          await leg.send("EXTEND");
          await leg.waitFor("TRANSPORT_GROWTH_EXTEND_OK");
          assertEquals(extendCalls, 1, "the grown capability ran exactly once");

          const after = await attachments();
          const added = after.filter((item) =>
            !before.some((previous) =>
              previous.connectionId === item.connectionId
            )
          );
          assert(
            added.length >= 1,
            "growth must admit a new physical attachment",
          );
          // The wider generation is admitted with a new exact context, and the
          // original attachment survives: make-before-break, not a replacement.
          assert(
            added.every((item) =>
              item.contextDigest !== before[0].contextDigest
            ),
            "the grown generation must be admitted with a newer authorization context",
          );
          assert(
            after.some((item) => item.connectionId === before[0].connectionId),
            "the original attachment must keep serving covered work",
          );
          assertEquals(
            after[0].runtimeConnectionId,
            logical,
            "the logical connection identity must be stable across growth",
          );

          // The observation opened before growth keeps receiving frames: it is
          // pinned to its generation and never moved mid-observation.
          await runtime.waitFor(
            () =>
              (leg.latestObservedIndex() ?? -1) > (observedBefore ?? -1)
                ? true
                : undefined,
            { timeoutMs: 30_000, intervalMs: 50 },
          );
          assert(
            (leg.latestObservedIndex() ?? -1) > (observedBefore ?? -1),
            "the Live observation must keep delivering after growth",
          );
          assert(
            (await attachments()).some((item) =>
              item.connectionId === before[0].connectionId
            ),
            "the observation's original attachment must remain while it is open",
          );

          // The caller's observation was accepted on the subject's initial
          // generation and keeps delivering across the growth: its control must
          // still reach the subject on the receiving generation.
          await runtime.waitFor(
            () => liveFrames.length > liveBefore ? true : undefined,
            { timeoutMs: 30_000, intervalMs: 50 },
          );
          // Explicit close, asserting the *confirmed* remote disposition: the
          // close receipt reports whether the receiving generation settled it,
          // not merely that the local request promise resolved.
          const receipt = await feed.close().orThrow();
          assertEquals(
            receipt.remote,
            "confirmed",
            "the Live close must be remotely confirmed on the receiving generation",
          );
          assertEquals(
            receipt.cleanup,
            "complete",
            "the closed observation must report a complete remote cleanup",
          );
          await liveTask;
          // A fresh observation opens on the current generation and delivers.
          const fresh = await observer.watch({
            runId: crypto.randomUUID(),
            streamId: "growth",
          }).orThrow();
          const freshFrames: bigint[] = [];
          const freshTask = (async () => {
            for await (const frame of fresh) freshFrames.push(frame.index);
          })().catch(() => undefined);
          await runtime.waitFor(
            () => freshFrames.length > 0 ? true : undefined,
            { timeoutMs: 30_000, intervalMs: 50 },
          );
          await fresh.close().orThrow();
          await freshTask;
          await observer.connection.close().catch(() => undefined);

          // A retained closed handle settles its signed close exchange and then
          // stops pinning its generation; the application keeps the handle.
          await leg.send("OBSERVE_CLOSE");
          await leg.waitFor("TRANSPORT_GROWTH_OBSERVED_CLOSED");

          // A retained handle whose session ends on its own must also release
          // its generation: open a fresh observation, then let the provider end
          // its source so the observation reaches a natural terminal.
          const beforeEnd = leg.latestObservedIndex() ?? -1;
          await leg.send("OBSERVE");
          await runtime.waitFor(
            () =>
              (leg.latestObservedIndex() ?? -1) > beforeEnd ? true : undefined,
            { timeoutMs: 30_000, intervalMs: 50 },
          );
          progressRunning = false;
          await leg.waitFor("TRANSPORT_GROWTH_OBSERVED_ENDED");

          // Existing covered work keeps working and a renewal still adds nothing.
          await leg.send("ADVANCE");
          await leg.waitFor("TRANSPORT_GROWTH_ADVANCE_OK", 2);
          assertEquals(advanceCalls, 2, "the admitted capability kept working");
          const issued = await renewalCount();
          await runtime.waitFor(
            async () => (await renewalCount()) > issued ? true : undefined,
            {
              timeoutMs: 45_000,
              intervalMs: 250,
            },
          );
          assertEquals(
            (await attachments()).length,
            after.length,
            "a further renewal must not open another socket",
          );

          // A logical close releases every physical generation, including the
          // retained baseline, and does not resurrect one afterwards.
          await leg.send("CLOSE");
          await leg.waitFor("TRANSPORT_GROWTH_CLOSED");
          await runtime.waitFor(async () => {
            return (await attachments()).length === 0 ? true : undefined;
          }, { timeoutMs: 30_000 });
        },
      );
    } finally {
      database.close();
      await target.connection.close().catch(() => undefined);
      await targetExit;
    }
  }, runtimeOptions);
});
