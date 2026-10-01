/**
 * Real-broker retained-receipt takeover after the owning generation is disposed.
 *
 * One service (`TransportGrowthSubject`) serves two live routes. Its optional
 * `extend` capability is declined at consent and approved later, so authority
 * growth opens a wider physical generation (G2) under the same logical
 * connection. The caller's Live observation is then opened ON G2 (the old
 * generic intake is retired, so G2 owns the session). Narrowing the grant back
 * to the original atom set closes G2, which disposes G2's per-generation live
 * provider through `onRetire`: the accepted session becomes a retained receipt.
 *
 * The same-route provider installed on the surviving original generation (G1)
 * has never served a session yet must answer the caller's bounded signed CLOSE
 * from its receipt; an unrelated-route provider registered first must not be
 * the responder. This is the real production manager/provider path over a real
 * broker, no captured NATS.
 */

import { assert, assertEquals } from "@std/assert";
import { fromFileUrl } from "@std/path";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import {
  admittedConnections,
  brokerConnectionKey,
  readRuntimeBrokerInventory,
} from "./_support/broker_inventory.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

type Runtime = Parameters<Parameters<typeof withTrellisRuntime>[0]>[0];

/** Optional capability id declined at consent and grown later. */
const CAPABILITY_B = "runtime-trellis.transport_growth@v1::extend";
const SUBJECT_DEPLOYMENT = "receipt-takeover-subject";
const TARGET_DEPLOYMENT = "receipt-takeover-target";

function serverBinary(): string {
  return Deno.env.get("TRELLIS_TEST_SERVER_BIN") ??
    fromFileUrl(
      new URL("../../target/debug/trellis-server", import.meta.url),
    );
}

/** Canonical short lifetimes; no timing widening for the receipt window. */
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

type Attachment = {
  participantId: string;
  runtimeConnectionId: string;
  connectionId: string;
  contextDigest: string;
  connectedAt: bigint;
};

type TrellisGrantBinding = {
  approval: unknown;
  approvalMode: string;
  createdAt: bigint;
  expiresAt: bigint | null;
  grants: {
    format: string;
    permissions: { action: string; target: Uint8Array }[];
  };
  installedRevision: bigint;
  ownerId: string;
  ownerKind: string;
  participantId: string;
  platformPrivileges: string[];
  provenance: unknown;
  revision: bigint;
  state: string;
  updatedAt: bigint;
};

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

async function grantBinding(
  runtime: Runtime,
  participantId: string,
): Promise<TrellisGrantBinding> {
  const page = await runtime.callAdminRpc("authGrantsList", {
    participantId,
  }) as { items: TrellisGrantBinding[] };
  const binding = page.items.find((item) =>
    item.participantId === participantId
  );
  if (!binding) throw new Error(`no grant binding for ${participantId}`);
  return binding;
}

async function setPermissions(
  runtime: Runtime,
  binding: TrellisGrantBinding,
  permissions: TrellisGrantBinding["grants"]["permissions"],
): Promise<void> {
  await runtime.callAdminRpc("authGrantsSet", {
    expectedRevision: binding.revision,
    expiresAt: binding.expiresAt,
    grants: { format: binding.grants.format, permissions },
    idempotencyKey: crypto.randomUUID(),
    installedRevision: binding.installedRevision,
    ownerId: binding.ownerId,
    ownerKind: binding.ownerKind,
    participantId: binding.participantId,
    platformPrivileges: binding.platformPrivileges,
  });
}

Deno.test(
  "a retained receipt is answered by the surviving same-route generation",
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const subjectContract = participants.TransportGrowthSubject.participant;
      const targetContract = participants.TransportGrowthTarget.participant;

      // The subject's `transport_growth` dependency deployment must resolve
      // before the subject may bootstrap its own authority.
      await runtime.contracts.apply({
        deployment: TARGET_DEPLOYMENT,
        contract: targetContract,
      });
      const targetInstance = await runtime.services.createInstance({
        deployment: TARGET_DEPLOYMENT,
        name: "receipt-takeover-target",
        contract: targetContract,
      });
      const target = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: targetContract,
        name: "receipt-takeover-target",
        seed: targetInstance.seed,
      }).orThrow();
      const targetExit = target.wait().catch((error: unknown) => error);
      await target.handleAdvance(() => Result.ok({}));
      await target.handleExtend(() => Result.ok({}));

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
      const instance = await runtime.services.createInstance({
        deployment: SUBJECT_DEPLOYMENT,
        name: "receipt-takeover-subject",
        contract: subjectContract,
      });
      const subject = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: subjectContract,
        name: "receipt-takeover-subject",
        seed: instance.seed,
      }).orThrow();
      const subjectExit = subject.wait().catch((error: unknown) => error);
      const subjectId = subjectContract.identity;

      // An accepted RPC keeps G1 alive across growth without offering Watch on
      // that generation. An idle superseded generation must otherwise reap.
      let inspectionAccepted = false;
      const inspectionRelease = Promise.withResolvers<void>();
      await subject.handleInspect(async () => {
        inspectionAccepted = true;
        await inspectionRelease.promise;
        return Result.ok({ starts: 0n, active: 0n, cleanups: 0n, emitted: 0n });
      });

      const emitters = new Set<() => void>();
      const emitLoop = async (
        emit: (frame: {
          runId: string;
          streamId: string;
          sourceGeneration: bigint;
          index: bigint;
          payload: Uint8Array;
          padding: string;
        }) => void,
        streamId: string,
        signal: AbortSignal,
      ) => {
        let index = 1n;
        while (!signal.aborted) {
          emit({
            runId: "receipt-run",
            streamId,
            sourceGeneration: 1n,
            index,
            payload: new Uint8Array(),
            padding: "",
          });
          index += 1n;
          await new Promise<void>((resolve) => {
            const timer = setTimeout(resolve, 250);
            emitters.add(() => {
              clearTimeout(timer);
              resolve();
            });
          });
        }
      };

      // The unrelated route (`probe_aux.Pulse`) is installed FIRST, so it
      // registers as an unrelated-route live provider before the Watch
      // route's provider on the same connection.
      await subject.handlePulse(async ({ emit, signal }) => {
        let index = 1n;
        while (!signal.aborted) {
          emit({ value: String(index) });
          index += 1n;
          await new Promise<void>((resolve) => {
            const timer = setTimeout(resolve, 250);
            emitters.add(() => {
              clearTimeout(timer);
              resolve();
            });
          });
        }
      });
      // The Watch route under test: installed at readiness, never offered a
      // session on the original generation.
      await subject.handleWatch(async ({ emit, signal }) => {
        await emitLoop(emit as never, "watch", signal);
      });

      const caller = await runtime.connectClient({
        name: "receipt-takeover-caller",
        contract: participants.LiveProbeCaller.participant,
      });
      const callerExit = Promise.resolve();
      let heldInspection: Promise<unknown> | undefined;

      try {
        // Baseline: the original attachment and its narrow atom set.
        const [original] = await attachmentsFor(runtime, subjectId);
        assert(original, "the subject must have an attachment");
        const logical = original.runtimeConnectionId;
        const baselineBinding = await grantBinding(runtime, subjectId);
        const baselineAtoms = baselineBinding.grants.permissions.map(
          (atom) => ({ action: atom.action, target: atom.target }),
        );
        assert(
          baselineAtoms.length > 0,
          "the baseline authority must carry atoms",
        );

        heldInspection = caller.inspect({
          runId: crypto.randomUUID(),
          streamId: "hold-baseline",
        }, { timeout: 30_000 }).orThrow();
        void heldInspection.catch(() => undefined);
        await runtime.waitFor(() => inspectionAccepted);

        // Grow the declined optional capability: a wider generation is adopted
        // under the same logical connection, automatically.
        await runtime.contracts.apply({
          deployment: SUBJECT_DEPLOYMENT,
          contract: subjectContract,
        });
        const overlap = await runtime.waitFor(async () => {
          const items = (await attachmentsFor(runtime, subjectId)).filter(
            (item) => item.runtimeConnectionId === logical,
          );
          return items.some((item) =>
              item.connectionId === original.connectionId
            ) &&
              items.some((item) => item.connectionId !== original.connectionId)
            ? items
            : false;
        }, { timeoutMs: 90_000 });
        const wider = overlap.find((item) =>
          item.connectionId !== original.connectionId
        );
        assert(wider, "growth must create a distinct physical generation");
        const overlapInventory = await readRuntimeBrokerInventory(runtime);
        const [g1Socket] = admittedConnections(
          overlapInventory,
          new Set([original.contextDigest]),
        );
        const [g2Socket] = admittedConnections(
          overlapInventory,
          new Set([wider.contextDigest]),
        );
        assert(g1Socket, "the held baseline RPC must keep G1 broker-admitted");
        assert(g2Socket, "the wider generation must be broker-admitted");
        assert(brokerConnectionKey(g1Socket) !== brokerConnectionKey(g2Socket));
        const grown = await grantBinding(runtime, subjectId);
        assert(
          grown.grants.permissions.length > baselineAtoms.length,
          "growth must widen the grant atom set",
        );

        // Only now open the observation, so the wider generation owns it; the
        // original generation's generic intake has already been retired.
        const feed = await caller.watch({
          runId: crypto.randomUUID(),
          streamId: "watch",
        }).orThrow();
        const frames: bigint[] = [];
        const feedTask = (async () => {
          for await (const frame of feed) frames.push(frame.index);
        })().catch(() => undefined);
        await runtime.waitFor(() => frames.length > 0, { timeoutMs: 30_000 });

        // Narrow back to the baseline atom set: the wider generation must close
        // (its provider is disposed) while the original attachment survives.
        await setPermissions(runtime, grown, baselineAtoms);
        await runtime.waitFor(async () => {
          const items = await attachmentsFor(runtime, subjectId);
          const inventory = await readRuntimeBrokerInventory(runtime, {
            requiredServerIds: [g1Socket.server, g2Socket.server],
          });
          const physicalKeys = new Set(inventory.map(brokerConnectionKey));
          return items.length === 1 &&
              items[0].runtimeConnectionId === logical &&
              items[0].connectionId === original.connectionId &&
              physicalKeys.has(brokerConnectionKey(g1Socket)) &&
              !physicalKeys.has(brokerConnectionKey(g2Socket))
            ? items
            : false;
        }, { timeoutMs: 60_000 });

        // The accepted session is now a retained receipt on the shared manager;
        // the surviving same-route provider has never offered a session. A
        // bounded signed CLOSE from the original caller must be confirmed by a
        // real exchange, not a no-op.
        const receipt = await feed.close().orThrow();
        assertEquals(
          receipt.remote,
          "confirmed",
          "the surviving same-route generation must confirm the close receipt",
        );
        await feedTask;
        inspectionRelease.resolve();
        await heldInspection;
      } finally {
        inspectionRelease.resolve();
        await heldInspection?.catch(() => undefined);
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
