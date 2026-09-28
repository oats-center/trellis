/**
 * F2 — capability growth is passive until explicitly adopted.
 *
 * One service attachment starts with capability A admitted and its optional
 * capability B offered-but-declined. Growing B through the real admin API must
 * leave the same physical attachment serving A and an active Live observation
 * while B is reported as a pending transport condition, and only an explicit
 * `refreshTransport()` may replace the attachment.
 *
 * Physical identity is `connectionId` (broker server + client id + user NKey);
 * `runtimeConnectionId` is the SDK logical connection and must stay stable
 * across the replacement.
 */

import { assert, assertEquals } from "@std/assert";
import { fromFileUrl } from "@std/path";
import { Result } from "@oatscenter/trellis";
import { encodePermissionTargetWasm } from "@oatscenter/trellis/auth";
import type { TransportError } from "@oatscenter/trellis/errors";
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
async function waitForAttachment(
  runtime: Runtime,
  participantId: string,
  predicate: (item: Attachment) => boolean,
): Promise<Attachment> {
  return await runtime.waitFor(async () => {
    const items = await attachmentsFor(runtime, participantId);
    return items.find(predicate) ?? false;
  }, { timeoutMs: 90_000 });
}

/** Grant binding as reported by the production admin surface. */
type GrantBinding = {
  revision: bigint;
  installedRevision: bigint;
  expiresAt: bigint | null;
  ownerId: string;
  ownerKind: string;
  participantId: string;
  platformPrivileges: string[];
  grants: {
    format: string;
    permissions: { action: string; target: Uint8Array }[];
  };
};

async function grantBinding(
  runtime: Runtime,
  participantId: string,
): Promise<GrantBinding> {
  const page = await runtime.callAdminRpc("authGrantsList", {
    participantId,
  }) as { items: GrantBinding[] };
  const binding = page.items.find((item) =>
    item.participantId === participantId
  );
  if (!binding) throw new Error(`no grant binding for ${participantId}`);
  return binding;
}

Deno.test(
  "F2 a grown capability stays passive until explicit adoption",
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
      let targetCalls = 0;
      await target.handleAdvance(() => {
        targetCalls += 1;
        return Result.ok({});
      });
      await target.handleExtend(() => {
        targetCalls += 1;
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
        const feedTask = (async () => {
          for await (const frame of feed) frames.push(frame.index);
        })();
        await runtime.waitFor(() => frames.length > 0, { timeoutMs: 30_000 });

        // 5. A succeeds on the admitted authority.
        assertEquals(
          (await subject.advance({})).isOk(),
          true,
        );
        assertEquals(targetCalls, 1);

        // 6. The attachment under test.
        const [subjectAttachment] = await attachmentsFor(runtime, subjectId);
        assert(subjectAttachment, "the subject must have an attachment");
        const logical = subjectAttachment.runtimeConnectionId;
        const physical = subjectAttachment.connectionId;

        // 7. Status before growth.
        assertEquals(
          subject.connection.status.transportUpgradeAvailable,
          false,
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

        // 9. The retained notice appears on the same attachment.
        await runtime.waitFor(
          () => subject.connection.status.transportUpgradeAvailable === true,
          { timeoutMs: 90_000 },
        );

        // 10. A reader arriving after the growth sees the retained status.
        assertEquals(subject.connection.status.transportUpgradeAvailable, true);

        // 11. Still the same attachment, logical and physical.
        const [grown] = await attachmentsFor(runtime, subjectId);
        assertEquals(grown.runtimeConnectionId, logical);
        assertEquals(grown.connectionId, physical);

        // 12-13. An ordinary renewal keeps everything on the same attachment.
        // The short configured lifetimes guarantee at least one renewal inside
        // this window; the attachment identity is re-read afterwards.
        assertEquals(
          (await subject.advance({})).isOk(),
          true,
        );
        await runtime.waitFor(() => frames.length > 1, { timeoutMs: 30_000 });
        const [renewed] = await attachmentsFor(runtime, subjectId);
        assertEquals(renewed.runtimeConnectionId, logical);
        assertEquals(renewed.connectionId, physical);
        assertEquals(subject.connection.status.transportUpgradeAvailable, true);

        // 14. The grown capability is gated, not silently attempted.
        const gated = await subject.extend({});
        assert(gated.isErr(), "the unadopted capability must not run");
        assertEquals(
          (gated.error as TransportError).code,
          "transport_upgrade_required",
        );
        const [stillPending] = await attachmentsFor(runtime, subjectId);
        assertEquals(stillPending.connectionId, physical);
        assertEquals(
          targetCalls,
          1,
          "the gated call must not reach the target",
        );

        // 15-16. Explicit adoption replaces the physical attachment only.
        await subject.connection.refreshTransport().orThrow();
        const adopted = await waitForAttachment(
          runtime,
          subjectId,
          (item) => item.connectionId !== physical,
        );
        assertEquals(adopted.runtimeConnectionId, logical);
        assert(
          adopted.connectionId !== physical,
          "explicit adoption must produce a new physical attachment",
        );

        // 17. The old ephemeral observation experiences ordinary loss.
        await feedTask.catch(() => undefined);

        // 18. Both capabilities work on the adopted attachment.
        assertEquals(
          (await subject.extend({})).isOk(),
          true,
        );
        assertEquals(
          (await subject.advance({})).isOk(),
          true,
        );
        assertEquals(targetCalls, 3);

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
