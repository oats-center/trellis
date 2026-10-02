/**
 * Real-boundary held-carrier retirement acceptance.
 *
 * A connected service holds real accepted work on two superseded physical
 * generations: `G0` is the baseline, held by an accepted `Park` RPC, and `G1`
 * is the intermediate growth, also held. `G2` adopts a second optional
 * capability. Reducing the binding back to the `G1` policy revokes `G2`'s
 * context while the narrower `G0`/`G1` stay covered, so `G2` closes and `G1`
 * serves again.
 *
 * Releasing `G0`'s accepted work must let its exact physical socket reap
 * through the ordinary lease-zero graceful-retirement path, materially before
 * its hard authority expiry and without waiting for another authority change,
 * while `G1` stays attached and its held work still completes.
 *
 * This is the three-generation reduction path (the two-generation `F3` case
 * makes the held narrower generation the resumed current instead). It does not
 * by itself isolate the abort-and-restart of a reap that was canceled by an
 * *unavailable own authority* window: in the ordinary reduction here the
 * baseline reap stays in flight, so this case passes on a manager that never
 * restarts a canceled reap. It is kept as real coverage of the held-carrier
 * reduction path, not as a discriminating regression for that restart.
 */

import { encodePermissionTargetWasm } from "@oatscenter/trellis/auth";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import { assert, assertEquals } from "@std/assert";
import { fromFileUrl } from "@std/path";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import type { NativeTransportGate } from "../../ts/packages/trellis-testkit/src/native_gate.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

type Runtime = Parameters<Parameters<typeof withTrellisRuntime>[0]>[0];

/** Elapsed-millisecond milestone logging; the run is otherwise silent. */
const startedAt = performance.now();
function milestone(step: string): void {
  console.error(
    `[recovery +${Math.round(performance.now() - startedAt)}ms] ${step}`,
  );
}

/** Qualified API identity carrying the optional growth capabilities. */
const GROWTH_API = "runtime-trellis.transport_growth@v1";
const CAPABILITY_EXTEND = `${GROWTH_API}::extend`;
const CAPABILITY_EXTEND2 = `${GROWTH_API}::extend2`;
const PROVIDER_DEPLOYMENT = "held-retiree-provider";

/**
 * Long context lifetime so the bounded retirement wait is driven by the real
 * reduction rather than by a scheduled renewal, and so proving the exact `G0`
 * socket reaps well inside the window is meaningful against hard expiry.
 */
const CONTEXT_LIFETIME_SECONDS = 300;

/** Built server binary supplied by the live test harness. */
function serverBinary(): string {
  return Deno.env.get("TRELLIS_TEST_SERVER_BIN") ??
    fromFileUrl(new URL("../../target/debug/trellis-server", import.meta.url));
}

const runtimeOptions = {
  authorization: {
    contextLifetimeSeconds: CONTEXT_LIFETIME_SECONDS,
    refreshLeadSeconds: 20,
    refreshJitterSeconds: 0,
    minimumContextLifetimeSeconds: 240,
  },
  trellis: {
    command: {
      cmd: serverBinary(),
      args: ["--config", "{config}", "all"],
    },
  },
  interruptibleNativeProxy: true,
};

/** One admitted attachment as reported by the production admin surface. */
type Attachment = {
  participantId: string;
  runtimeConnectionId: string;
  connectionId: string;
  contextDigest: string;
};

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

/** Reject with `message` if `promise` has not settled within `ms`. */
function withTimeout<T>(
  promise: Promise<T>,
  ms: number,
  message: string,
): Promise<T> {
  return Promise.race([
    promise,
    new Promise<never>((_, reject) =>
      setTimeout(() => reject(new Error(message)), ms)
    ),
  ]);
}

function atomKey(atom: { action: string; target: Uint8Array }): string {
  return `${atom.action}:${btoa(String.fromCharCode(...atom.target))}`;
}

async function rpcAtom(name: string) {
  return {
    action: "call",
    target: await encodePermissionTargetWasm({
      kind: "apiSurface",
      api: GROWTH_API,
      surface: "rpc",
      name,
    }),
  };
}

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

async function attachmentsFor(
  runtime: Runtime,
  participantId: string,
): Promise<Attachment[]> {
  const page = await runtime.callAdminRpc("authConnectionsList", {}) as {
    items: Attachment[];
  };
  return page.items.filter((item) => item.participantId === participantId);
}

async function setGrants(
  runtime: Runtime,
  binding: GrantBinding,
  permissions: GrantBinding["grants"]["permissions"],
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
  "a held superseded carrier reaps after a reduction while the survivor resumes",
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const providerContract =
        participants.TransportGrowthOperationProvider.participant;
      const callerContract = participants.TransportGrowthCaller.participant;
      const providerId = providerContract.identity;

      await runtime.contracts.install({ contract: providerContract });
      const requested = await runtime.contracts.requestApply({
        deployment: PROVIDER_DEPLOYMENT,
        contract: providerContract,
      });
      assert(
        requested.status === "approval_required",
        "the provider deployment must require an approval",
      );
      if (requested.status !== "approval_required") return;
      await runtime.contracts.approveApply(requested.pendingId, {
        excludeCapabilities: [CAPABILITY_EXTEND, CAPABILITY_EXTEND2],
      });
      const providerInstance = await runtime.services.createInstance({
        deployment: PROVIDER_DEPLOYMENT,
        name: "held-retiree-provider",
        contract: providerContract,
      });

      const service = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: providerContract,
        name: "held-retiree-provider",
        seed: providerInstance.seed,
      }).orThrow();
      const serviceExit = service.wait().catch((error: unknown) => error);

      // `Park` is served on every generation, so a real accepted call keeps the
      // exact generation that accepted it alive independently of any pin. Each
      // held call is tracked so the test releases exactly the one it needs.
      let parkStarts = 0;
      let holdParks = false;
      const heldParks: { release: () => void }[] = [];
      /** Entries the test intentionally keeps held across a transition. */
      const keptParks = new Set<{ release: () => void }>();
      await service.handlePark(() => {
        parkStarts += 1;
        const value = `parked-${parkStarts}`;
        if (!holdParks) return Result.ok({ value });
        return new Promise((resolve) => {
          heldParks.push({ release: () => resolve(Result.ok({ value })) });
        });
      });
      await service.handleExtend(() => Result.ok({}));
      await service.handleExtend2(() => Result.ok({}));

      const caller = await runtime.connectClient({
        name: "held-retiree-caller",
        contract: callerContract,
        timeout: 600_000,
      });

      const gate: NativeTransportGate = runtime.nativeTransportGate();

      /** `Park` deliveries observed per proxied connection, for polling. */
      function parkDeliveries(): Map<number, number> {
        return new Map(
          gate.connections().map((connection) => [
            connection.id,
            connection.deliveries.filter((delivery) =>
              delivery.subject.endsWith(".Park")
            ).length,
          ]),
        );
      }

      /** Poll until a proxied connection reports a new `Park` delivery. */
      async function waitForParkDelivery(
        before: Map<number, number>,
        ms: number,
      ): Promise<number | undefined> {
        const deadline = Date.now() + ms;
        while (Date.now() < deadline) {
          for (const connection of gate.connections()) {
            const seen = connection.deliveries.filter((delivery) =>
              delivery.subject.endsWith(".Park")
            ).length;
            if (seen > (before.get(connection.id) ?? 0)) {
              return connection.id;
            }
          }
          await new Promise((resolve) => setTimeout(resolve, 25));
        }
        return undefined;
      }

      /** Poll until the `Park` handler has been entered after `before`. */
      async function waitForParkEntry(
        before: number,
        ms: number,
      ): Promise<boolean> {
        const deadline = Date.now() + ms;
        while (Date.now() < deadline) {
          if (parkStarts > before) return true;
          await new Promise((resolve) => setTimeout(resolve, 25));
        }
        return parkStarts > before;
      }

      /**
       * Hold one real `Park` RPC on the first generation whose connection id
       * satisfies `match`. A non-matching generation, or a delivery rejected
       * before its handler runs (for example while the service's own authority
       * is briefly unavailable), completes the probe and retries, so the held
       * call is provably pinned to the matched generation rather than to a
       * leftover queue-group member.
       */
      async function holdPark(
        match: (connectionId: number) => boolean,
      ): Promise<{
        connectionId: number;
        entries: { release: () => void }[];
        response: Promise<{ value: string } | undefined>;
      }> {
        for (let probe = 0; probe < 128; probe += 1) {
          const before = parkStarts;
          const entriesBefore = heldParks.length;
          const beforeDeliveries = parkDeliveries();
          // Abandon a probe that is not held: a rejected probe's retryable auth
          // error would otherwise keep retrying inside the caller.
          const abort = new AbortController();
          const response = caller.park({}, { signal: abort.signal })
            .orThrow() as Promise<{ value: string } | undefined>;
          response.catch(() => undefined);
          // A retried delivery can invoke the handler more than once for one
          // probe; every entry it created is returned or released together so
          // no untracked held call outlives the probe.
          const entriesOf = (): { release: () => void }[] =>
            heldParks.slice(entriesBefore);
          const releaseAll = (): void => {
            for (const held of entriesOf()) held.release();
          };
          const delivered = await waitForParkDelivery(beforeDeliveries, 20_000);
          if (delivered === undefined) {
            abort.abort();
            releaseAll();
            throw new Error("no Park delivery was observed");
          }
          if (!await waitForParkEntry(before, 5_000)) {
            abort.abort();
            releaseAll();
            continue;
          }
          if (match(delivered)) {
            const entries = entriesOf();
            return {
              connectionId: delivered,
              entries,
              response,
            };
          }
          releaseAll();
          abort.abort();
        }
        throw new Error("no generation matched the probe Park RPC");
      }

      /** Prove a generation accepts real work, then release that call. */
      async function proveServing(
        match: (connectionId: number) => boolean,
      ): Promise<number> {
        const probe = await holdPark(match);
        for (const entry of probe.entries) entry.release();
        return probe.connectionId;
      }

      try {
        holdParks = true;

        // Baseline `G0`, held by a real accepted RPC on its own generation.
        const g0Held = await holdPark(() => true);
        for (const entry of g0Held.entries) keptParks.add(entry);
        const g0ConnectedAtMs = Date.now();
        const g0HardDeadlineMs = g0ConnectedAtMs +
          CONTEXT_LIFETIME_SECONDS * 1_000;
        const g0GateId = g0Held.connectionId;
        milestone(`G0 held on gate connection ${g0GateId}`);
        const [g0Attachment] = await runtime.waitFor(async () => {
          const items = await attachmentsFor(runtime, providerId);
          return items.length === 1 ? items : false;
        }, { timeoutMs: 30_000, intervalMs: 250 });
        const g0Digest = g0Attachment.contextDigest;
        const g0ConnectionId = g0Attachment.connectionId;
        milestone(
          `G0 identity ${g0ConnectionId} digest ${g0Digest.slice(0, 8)}`,
        );

        const baseline = await grantBinding(runtime, providerId);
        const p1 = baseline.grants.permissions;
        const extendAtom = await rpcAtom("Extend");
        const extend2Atom = await rpcAtom("Extend2");
        assert(
          !p1.map(atomKey).includes(atomKey(extendAtom)),
          "the first optional capability must start ungranted",
        );

        // Growth `G1` (`extend`): the manager opens and adopts a wider
        // generation on the same logical connection. Hold its own accepted
        // work so the narrower generation survives the second growth.
        await setGrants(runtime, baseline, [...p1, extendAtom]);
        const g1Policy = (await grantBinding(runtime, providerId)).grants
          .permissions;
        const g1GateId = await proveServing((id) => id !== g0GateId);
        const g1Held = await holdPark((id) => id === g1GateId);
        for (const entry of g1Held.entries) keptParks.add(entry);
        milestone(`G1 held on gate connection ${g1GateId}`);
        const g1Attachment = await runtime.waitFor(async () => {
          const items = await attachmentsFor(runtime, providerId);
          const found = items.find((item) => item.contextDigest !== g0Digest);
          return items.length === 2 && found ? found : false;
        }, { timeoutMs: 30_000, intervalMs: 250 });
        const g1Digest = g1Attachment.contextDigest;
        const g1ConnectionId = g1Attachment.connectionId;
        milestone(`G1 admitted digest ${g1Digest.slice(0, 8)}`);

        // Growth `G2` (`extend2`): a third wider generation is adopted, which
        // drains `G1`'s generic intake while its held accepted work survives.
        const grown1 = await grantBinding(runtime, providerId);
        await setGrants(runtime, grown1, [...g1Policy, extend2Atom]);
        const g2GateId = await proveServing((id) =>
          id !== g0GateId && id !== g1GateId
        );
        milestone(`G2 serving on gate connection ${g2GateId}`);
        const g2Attachment = await runtime.waitFor(async () => {
          const items = await attachmentsFor(runtime, providerId);
          const found = items.find((item) =>
            item.contextDigest !== g0Digest &&
            item.contextDigest !== g1Digest
          );
          return items.length === 3 && found ? found : false;
        }, { timeoutMs: 30_000, intervalMs: 250 });
        const g2Digest = g2Attachment.contextDigest;
        milestone(`G2 admitted digest ${g2Digest.slice(0, 8)}`);

        // Reduce back to the `G1` policy. `G2`'s context is revoked (its
        // authority is no longer covered) while the narrower `G0`/`G1` stay
        // covered, so `G2` closes and `G1` becomes the default again.
        const grown2 = await grantBinding(runtime, providerId);
        await setGrants(runtime, grown2, g1Policy);
        await runtime.waitFor(async () => {
          const items = await attachmentsFor(runtime, providerId);
          if (items.length !== 2) return false;
          const digests = new Set(items.map((item) => item.contextDigest));
          return digests.has(g0Digest) && digests.has(g1Digest) &&
            !digests.has(g2Digest);
        }, { timeoutMs: 60_000, intervalMs: 250 });
        // `G1` resumed as the default: it accepts fresh real work again.
        await proveServing((id) => id === g1GateId);
        milestone("G1 resumed serving after reduction");

        // Settle every probe-overlap delivery, then release `G0`'s accepted
        // work. Its graceful retirement must reach lease zero and reap the
        // exact physical socket. No stray held call from an in-flight probe
        // retry may keep the superseded generation pinned. `G0`'s exact
        // attachment is still admitted while its accepted work is held — a
        // pinned superseded carrier survives until it is released.
        const beforeRelease = await attachmentsFor(runtime, providerId);
        assertEquals(
          new Set(beforeRelease.map((item) => item.connectionId)),
          new Set([g0ConnectionId, g1ConnectionId]),
          "the held G0 attachment must survive until its work is released",
        );
        holdParks = false;
        for (let sweep = 0; sweep < 80; sweep += 1) {
          for (const entry of heldParks) {
            if (!keptParks.has(entry)) entry.release();
          }
          if (heldParks.every((entry) => keptParks.has(entry))) break;
          await new Promise((resolve) => setTimeout(resolve, 25));
        }
        for (const entry of g0Held.entries) entry.release();
        const absenceDeadlineMs = g0HardDeadlineMs - 30_000;
        milestone(
          "released G0 held work; waiting for exact G0 identity to leave",
        );
        // The exact captured `G0` physical socket must close well inside the
        // bound: its retirement reaps at lease zero instead of waiting for hard
        // expiry and another authority change. The test transport proxy
        // observes the real client close on that exact connection.
        await withTimeout(
          gate.waitForClose(g0GateId),
          Math.max(5_000, absenceDeadlineMs - Date.now()),
          "the superseded G0 physical socket did not close before its hard expiry",
        );
        assert(
          gate.connection(g0GateId)?.closed === true,
          "the exact superseded G0 physical socket must be closed",
        );
        assert(
          Date.now() < absenceDeadlineMs,
          "G0 must retire materially before its hard authority expiry",
        );
        milestone(`G0 identity ${g0ConnectionId} reaped`);

        // `G1` remains serving with its held accepted work intact, and that
        // work still completes on the resumed generation. The admitted set
        // independently confirms the resumed generation is still attached.
        assert(
          gate.connection(g1GateId)?.closed === false,
          "the resumed G1 physical socket must remain open",
        );
        assert(
          (await attachmentsFor(runtime, providerId)).some((item) =>
            item.connectionId === g1ConnectionId
          ),
          "the resumed G1 generation must remain attached",
        );
        for (const entry of g1Held.entries) entry.release();
        assertEquals(
          (await withTimeout(
            g1Held.response,
            60_000,
            "G1's held accepted work did not complete",
          ))?.value?.startsWith("parked-"),
          true,
          "G1's held accepted work must complete after it resumes",
        );
        milestone("G1 held work completed");
      } finally {
        for (const entry of heldParks) entry.release();
        await caller.connection.close().catch(() => undefined);
        await service.connection.close().catch(() => undefined);
        await serviceExit;
      }
    }, runtimeOptions);
  },
);
