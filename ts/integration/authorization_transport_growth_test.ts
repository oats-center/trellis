/**
 * F2 — capability growth is adopted automatically on a service.
 *
 * One service attachment starts with capability A admitted and its optional
 * capability B offered-but-declined. Growing B through the real admin API must
 * automatically open a wider physical generation that serves B while A and an
 * active Live observation accepted on the original generation keep working.
 * No application refresh call is made, and growth is not an outage.
 *
 * Physical identity is `connectionId`; `runtimeConnectionId` is the SDK logical
 * connection and must stay stable across the generation change.
 */

import { assert, assertEquals } from "@std/assert";
import { fromFileUrl } from "@std/path";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

type Runtime = Parameters<Parameters<typeof withTrellisRuntime>[0]>[0];

/** Optional capability id declined at consent and grown later. */
const CAPABILITY_B = "runtime-trellis.transport_growth@v1::extend";

/**
 * One deployment carries one participant assignment, so the subject and its
 * target use separate deployments and nothing relies on the runtime default.
 */
const SUBJECT_DEPLOYMENT = "f2-growth-subject";
const TARGET_DEPLOYMENT = "f2-growth-target";

/** Built server binary supplied by the live test harness. */
function serverBinary(): string {
  return Deno.env.get("TRELLIS_TEST_SERVER_BIN") ??
    fromFileUrl(
      new URL("../../target/debug/trellis-server", import.meta.url),
    );
}

/**
 * Short lifetimes so the case crosses a real ordinary context renewal without
 * waiting for the production cadence.
 */
const runtimeOptions = {
  authorization: {
    contextLifetimeSeconds: 76,
    refreshLeadSeconds: 15,
    refreshJitterSeconds: 0,
    minimumContextLifetimeSeconds: 46,
  },
  trellis: {
    command: {
      cmd: serverBinary(),
      args: ["--config", "{config}", "all"],
    },
  },
};

/** One admitted attachment as reported by the production admin surface. */
type Attachment = {
  participantId: string;
  runtimeConnectionId: string;
  connectionId: string;
  connectedAt: bigint;
  loginSessionId?: string | null;
};

/** Reads the admitted attachments for one participant, newest first. */
async function attachmentsFor(
  runtime: Runtime,
  participantId: string,
): Promise<Attachment[]> {
  const page = await runtime.callAdminRpc("authConnectionsList", {}) as {
    items: Attachment[];
  };
  return page.items
    .filter((item) => item.participantId === participantId)
    .sort((a, b) => Number(b.connectedAt - a.connectedAt));
}

/** Waits until the attachment predicate holds for one participant. */

Deno.test(
  "F2 a grown service capability adopts automatically without disturbing Live",
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const subjectContract = participants.TransportGrowthSubject.participant;
      const targetContract = participants.TransportGrowthTarget.participant;

      // 1. The target serves A and B.
      await runtime.contracts.apply({
        deployment: TARGET_DEPLOYMENT,
        contract: targetContract,
      });
      const targetInstance = await runtime.services.createInstance({
        deployment: TARGET_DEPLOYMENT,
        name: "growth-target",
        contract: targetContract,
      });
      const target = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: targetContract,
        name: "growth-target",
        seed: targetInstance.seed,
      }).orThrow();
      const targetExit = target.wait().catch((error: unknown) => error);
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

      // 2. The subject is approved with B declined.
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
        excludeCapabilities: [CAPABILITY_B],
      });
      const subjectInstance = await runtime.services.createInstance({
        deployment: SUBJECT_DEPLOYMENT,
        name: "growth-subject",
        contract: subjectContract,
      });
      const subject = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: subjectContract,
        name: "growth-subject",
        seed: subjectInstance.seed,
      }).orThrow();
      const subjectExit = subject.wait().catch((error: unknown) => error);

      // 3. The subject serves the Live watch the caller observes.
      const emitters = new Set<() => void>();
      await subject.handleWatch(async ({ emit, signal }) => {
        let index = 1n;
        while (!signal.aborted) {
          emit({
            runId: "growth-run",
            streamId: "growth",
            sourceGeneration: 1n,
            index,
            payload: new Uint8Array(),
            padding: "",
          });
          index += 1n;
          await new Promise((resolve) => {
            const timer = setTimeout(resolve, 1_000);
            emitters.add(() => {
              clearTimeout(timer);
              resolve(undefined);
            });
          });
        }
      });

      const caller = await runtime.connectClient({
        name: "growth-caller",
        contract: participants.LiveProbeCaller.participant,
      });
      const subjectId = subjectContract.identity;
      let callerExit = Promise.resolve();

      try {
        // 4. An active observation on the subject's attachment.
        const feed = await caller.watch({
          runId: crypto.randomUUID(),
          streamId: "growth",
        }).orThrow();
        const frames: bigint[] = [];
        // The held observation must continue across planned generation growth.
        // Consume termination here; continued delivery is asserted below.
        const feedTask = (async () => {
          for await (const frame of feed) frames.push(frame.index);
        })().catch(() => undefined);
        await runtime.waitFor(() => frames.length > 0, { timeoutMs: 30_000 });

        // 5. A succeeds on the initial authority.
        assertEquals(
          (await subject.advance({})).isOk(),
          true,
        );
        assertEquals(advanceCalls, 1);

        // 6. The attachment under test.
        const [subjectAttachment] = await attachmentsFor(runtime, subjectId);
        assert(subjectAttachment, "the subject must have an attachment");
        const logical = subjectAttachment.runtimeConnectionId;
        const physical = subjectAttachment.connectionId;

        // 7. The logical connection is healthy before growth.
        assertEquals(
          subject.connection.status.phase,
          "connected",
        );

        // 8. Grow B through the real deployment consent path, keeping A.
        //
        // `Auth.Grants.Set` can only write atoms that present authority resolves,
        // and `resolve_authority` keeps capability atoms only when the capability
        // is approved. A declined capability is therefore approved through the
        // same server-computed consent the deployment already uses, which is the
        // real administrative path for authority growth.
        await runtime.contracts.apply({
          deployment: SUBJECT_DEPLOYMENT,
          contract: subjectContract,
        });

        // 9. Growth is adopted automatically: a second physical attachment
        //    appears under the same logical connection without any application
        //    refresh call.
        const adopted = await runtime.waitFor(async () => {
          const items = (await attachmentsFor(runtime, subjectId)).filter(
            (item) => item.runtimeConnectionId === logical,
          );
          return items.length >= 2 ? items : false;
        }, { timeoutMs: 90_000 });
        assert(
          adopted.some((item) => item.connectionId !== physical),
          "authority growth must adopt a new physical generation",
        );

        // 10. Planned growth is not a logical connection outage.
        assertEquals(
          subject.connection.status.phase,
          "connected",
        );

        // 11. The observation accepted on the original generation keeps
        //     delivering: make-before-break, not a replacement.
        const framesAfterAdoption = frames.length;
        await runtime.waitFor(() => frames.length > framesAfterAdoption, {
          timeoutMs: 30_000,
        });

        // 12-13. The grown capability now runs automatically on the new
        //     generation, and A keeps working.
        assertEquals(
          subject.connection.availability().capabilities[CAPABILITY_B],
          true,
          "the approved capability must be available at the application layer",
        );
        assertEquals(
          (await subject.extend({})).isOk(),
          true,
          "the grown capability must run without an explicit refresh",
        );
        assertEquals(
          (await subject.advance({})).isOk(),
          true,
        );
        assertEquals(extendCalls, 1);
        assertEquals(advanceCalls, 2);

        // 14. The logical identity stayed stable across the rollover.
        assert(
          (await attachmentsFor(runtime, subjectId)).every(
            (item) => item.runtimeConnectionId === logical,
          ),
          "the logical connection identity must stay stable across growth",
        );
        void feed.close();
        await feedTask;

        // 19. A newly opened observation works.
        const reopened = await caller.watch({
          runId: crypto.randomUUID(),
          streamId: "growth",
        }).orThrow();
        const reopenedFrames: bigint[] = [];
        callerExit = (async () => {
          for await (const frame of reopened) reopenedFrames.push(frame.index);
        })();
        await runtime.waitFor(() => reopenedFrames.length > 0, {
          timeoutMs: 30_000,
        });
        await reopened.close();
      } finally {
        for (const wake of emitters) wake();
        await callerExit.catch(() => undefined);
        await caller.connection.close().catch(() => undefined);
        await subject.connection.close().catch(() => undefined);
        await target.connection.close().catch(() => undefined);
        await Promise.all([subjectExit, targetExit]);
      }
    }, runtimeOptions);
  },
);
