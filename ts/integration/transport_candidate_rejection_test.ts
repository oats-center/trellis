/**
 * Real-boundary unpublished-candidate rejection acceptance.
 *
 * A connected service grows from `P1` to `P2` (`extend`), which opens a wider
 * transport generation. Its post-subscription readiness flush is held by the
 * test transport proxy, so the candidate never publishes while its routes are
 * already broker-live. A real caller then drives real served `Park` RPCs and the
 * proxy attributes the accepted delivery to the candidate's physical
 * connection; that handler stays parked.
 *
 * A second real administrative grant revision then changes the desired policy:
 *
 * - Safe (`P2 + extend2`): the candidate's admitted policy is still covered, so
 *   it is drained and the already-accepted callback completes exactly once,
 *   while a successor generation becomes current.
 * - Unsafe (`P1`): the candidate's admitted policy is no longer covered, so it
 *   is physically closed regardless of the held callback.
 *
 * A real signed outbound service RPC on the still-covered baseline is read from
 * the wire `authorization-context` header, so the local context promotion is
 * proven by use, not by issuance alone.
 */

import { encodePermissionTargetWasm } from "@oatscenter/trellis/auth";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import { createClient } from "@libsql/client";
import { assert, assertEquals } from "@std/assert";
import { fromFileUrl, join } from "@std/path";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import type { NativeTransportGate } from "../../ts/packages/trellis-testkit/src/native_gate.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

type Runtime = Parameters<Parameters<typeof withTrellisRuntime>[0]>[0];

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

/** Qualified API identity carrying the optional growth capabilities. */
const GROWTH_API = "runtime-trellis.transport_growth@v1";
const CAPABILITY_EXTEND = `${GROWTH_API}::extend`;
const CAPABILITY_EXTEND2 = `${GROWTH_API}::extend2`;
const PROVIDER_DEPLOYMENT = "candidate-rejection-provider";

/** Built server binary supplied by the live test harness. */
function serverBinary(): string {
  return Deno.env.get("TRELLIS_TEST_SERVER_BIN") ??
    fromFileUrl(new URL("../../target/debug/trellis-server", import.meta.url));
}

/**
 * Short lifetimes so an administrative grant revision reaches the connected
 * service through its ordinary scheduled refresh without a manual call.
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

/**
 * Runs one rejection scenario. The candidate barrier is held across a real
 * second grant revision; the held work is a real served `Park` callback that the
 * proxy positively attributed to the candidate.
 */
async function runCandidateRejection(mode: "safe" | "unsafe"): Promise<void> {
  await withTrellisRuntime(async (runtime) => {
    const providerContract =
      participants.TransportGrowthOperationProvider.participant;
    const callerContract = participants.TransportGrowthCaller.participant;
    const providerId = providerContract.identity;

    // The provider starts with both optional capabilities withheld, so `P1` is
    // the public surface alone.
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
      name: "candidate-rejection-provider",
      contract: providerContract,
    });

    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: providerContract,
      name: "candidate-rejection-provider",
      seed: providerInstance.seed,
    }).orThrow();
    const serviceExit = service.wait().catch((error: unknown) => error);

    // `Park` is served on every generation, so the caller's real requests are
    // broker-balanced; the proxy attributes which physical generation accepts
    // one. `Advance` is the outbound promotion-proof route.
    let parkStarts = 0;
    let holdProbes = false;
    const parkReleases: (() => void)[] = [];
    await service.handlePark(() => {
      parkStarts += 1;
      const value = `parked-${parkStarts}`;
      if (!holdProbes) return Result.ok({ value });
      return new Promise((resolve) => {
        parkReleases.push(() => resolve(Result.ok({ value })));
      });
    });
    let advanceCalls = 0;
    void advanceCalls;
    await service.handleAdvance(() => Result.ok({ value: "advance" }));
    await service.handleExtend(() => Result.ok({}));
    await service.handleExtend2(() => Result.ok({}));

    const caller = await runtime.connectClient({
      name: `candidate-rejection-caller-${mode}`,
      contract: callerContract,
      // The candidate-attributed Park is deliberately held across the second
      // grant revision and the release, so the request budget must outlast the
      // whole rejected-candidate window.
      timeout: 240_000,
    });

    const gate: NativeTransportGate = runtime.nativeTransportGate();
    const database = createClient({
      url: `file:${
        join(runtime.workdir, "trellis", "trellis.sqlite.platform")
      }`,
    });
    const issuedContexts = async (): Promise<string[]> => {
      const result = await database.execute({
        sql:
          "SELECT DISTINCT context_digest AS digest FROM auth_authorization_contexts WHERE participant_id = ?",
        args: [providerId],
      });
      return result.rows.map((row) => String(row.digest));
    };

    let heldPark: Promise<unknown> | undefined;
    let heldRelease: (() => void) | undefined;
    try {
      // Baseline established under `P1`.
      assertEquals((await caller.park({}).orThrow()).value, "parked-1");
      await runtime.waitFor(() => parkStarts === 1, { timeoutMs: 30_000 });
      const baselineContexts = new Set(await issuedContexts());

      const extendAtom = await rpcAtom("Extend");
      const extend2Atom = await rpcAtom("Extend2");
      const baseline = await grantBinding(runtime, providerId);
      const p1 = baseline.grants.permissions;
      assert(
        !p1.map(atomKey).includes(atomKey(extendAtom)),
        "the reserved capability must start ungranted",
      );

      // Grow to `P2`; the service opens a wider generation whose readiness
      // flush the proxy holds so the candidate never publishes.
      gate.arm(".Park");
      await setGrants(runtime, baseline, [...p1, extendAtom]);
      const candidateId = await gate.barrierHeld();
      assert(candidateId > 0, "the candidate barrier must be attributed");

      // The candidate is admitted under a new context; the baseline digest set
      // was captured before growth, so the new attachment digest identifies the
      // candidate's physical generation.
      let candidateContext: string | undefined;
      await runtime.waitFor(async () => {
        const items = await attachmentsFor(runtime, providerId);
        candidateContext = items.find((item) =>
          !baselineContexts.has(item.contextDigest)
        )?.contextDigest;
        return candidateContext;
      }, { timeoutMs: 30_000, intervalMs: 250 });
      // Contexts issued up to and including the growth; the second revision must
      // issue a context outside this set for the local promotion to be real.
      const grownContexts = new Set(await issuedContexts());

      // Drive real served RPCs until the proxy positively attributes one to the
      // candidate. Baseline deliveries complete normally; distinct probes, no
      // retries of the same work, no baseline topology hacks.
      holdProbes = true;
      let candidateHandled = false;
      for (let probe = 0; probe < 16 && !candidateHandled; probe++) {
        const before = parkStarts;
        const response = caller.park({}).orThrow();
        response.catch(() => undefined);
        const delivery = await Promise.race([
          gate.waitForDelivery(".Park"),
          new Promise<"timeout">((resolve) =>
            setTimeout(() => resolve("timeout"), 15_000)
          ),
        ]);
        if (delivery === "timeout") {
          throw new Error("no real Park delivery was observed");
        }
        await runtime.waitFor(() => parkStarts > before, { timeoutMs: 15_000 });
        if (delivery.connectionId === candidateId) {
          heldPark = response;
          heldRelease = parkReleases[parkReleases.length - 1];
          candidateHandled = true;
          break;
        }
        // Baseline delivery: complete it and continue probing.
        parkReleases.shift()?.();
        await response;
      }
      assert(
        candidateHandled,
        `the candidate must accept at least one delivered RPC: ${
          JSON.stringify({
            candidateId,
            connections: gate.connections().map((connection) => ({
              id: connection.id,
              closed: connection.closed,
              subs: connection.subs.map((sub) => sub.subject.split(".").pop()),
              deliveries: connection.deliveries.length,
            })),
          })
        }`,
      );
      const parkStartsAtHold = parkStarts;

      // Second real grant revision, while the candidate is still unpublished.
      const grown = await grantBinding(runtime, providerId);
      const p2 = grown.grants.permissions;
      const p2Keys = p2.map(atomKey);
      assert(
        p2Keys.includes(atomKey(extendAtom)),
        "the first growth must retain the extend capability",
      );
      if (mode === "safe") {
        await setGrants(runtime, grown, [...p2, extend2Atom]);
      } else {
        await setGrants(runtime, grown, p1);
      }

      // Promotion proof: a real signed outbound service RPC on the still-covered
      // baseline. The wire `authorization-context` header must be a context the
      // server issued after the baseline, proving local promotion by use rather
      // than by issuance alone.
      const promotionDeadline = Date.now() + 120_000;
      let promotedContext: string | undefined;
      while (Date.now() < promotionDeadline) {
        void service.advance({}).orThrow().catch(() => undefined);
        await new Promise((resolve) => setTimeout(resolve, 1_000));
        const issued = new Set(await issuedContexts());
        promotedContext = gate.connections()
          .flatMap((connection) => connection.outboundContexts)
          .find((outbound) =>
            outbound.subject.endsWith(".Advance") &&
            outbound.context !== undefined &&
            !grownContexts.has(outbound.context) &&
            issued.has(outbound.context)
          )?.context;
        if (promotedContext) break;
      }
      assert(
        promotedContext,
        "a real outbound service RPC must carry a promoted context digest",
      );

      assert(
        candidateContext,
        "the candidate generation must be admitted under its own context",
      );

      // Release the candidate's readiness flush and observe the rejection. The
      // connections that exist now are the baseline and the unpublished
      // candidate; anything opened later belongs to a replacement generation.
      const preReleaseConnections = new Set(
        gate.connections().map((connection) => connection.id),
      );
      await gate.release();

      if (mode === "unsafe") {
        // The wider candidate is physically closed regardless of held work.
        await gate.waitForClose(candidateId);
        assert(
          gate.connection(candidateId)?.closed === true,
          "the unsafe candidate must physically close",
        );
        // The broker-backed attachment inventory no longer reports the
        // candidate's admitted context, while the baseline stays attached.
        await runtime.waitFor(async () => {
          const items = await attachmentsFor(runtime, providerId);
          return items.some((item) => item.contextDigest === candidateContext)
            ? undefined
            : true;
        }, { timeoutMs: 30_000, intervalMs: 250 });
        const remaining = await attachmentsFor(runtime, providerId);
        assert(
          remaining.some((item) => baselineContexts.has(item.contextDigest)),
          "the baseline generation must survive the unsafe close",
        );
      } else {
        // The safe rejection drains the wider candidate: the generation holds
        // the already-accepted callback and must complete it exactly once
        // before it can physically close.
        await new Promise((resolve) => setTimeout(resolve, 2_000));
        heldRelease?.();
        const parked = await heldPark!;
        assert(parked !== undefined, "the held callback must complete");
        assertEquals(
          parkStarts,
          parkStartsAtHold,
          "the accepted callback must not be restarted or duplicated",
        );
        // A successor generation covering the newest desired policy becomes
        // current when that newly granted capability is actually exercised, and
        // it serves real traffic. That is the observable rejection outcome.
        void service.extend2({}).orThrow().catch(() => undefined);
        await runtime.waitFor(async () => {
          const items = await attachmentsFor(runtime, providerId);
          return items.some((item) =>
              item.contextDigest !== candidateContext &&
              !baselineContexts.has(item.contextDigest)
            )
            ? true
            : undefined;
        }, { timeoutMs: 120_000, intervalMs: 250 });
        // New deliveries must not park any more; the drained candidate may still
        // serve the route until its physical close.
        holdProbes = false;
        // Prove a real new RPC is served by the successor's own physical
        // connection, not the drained candidate. The route is shared, so drive
        // distinct requests (each carries its own request id) until the proxy
        // attributes a delivery to a connection opened for the successor.
        const successorConnections = new Set(
          gate.connections()
            .filter((connection) =>
              !preReleaseConnections.has(connection.id) &&
              connection.subs.some((sub) => sub.subject.endsWith(".Park"))
            )
            .map((connection) => connection.id),
        );
        assert(
          successorConnections.size > 0,
          "the successor must open a connection serving the probed route",
        );
        let successorServed = false;
        for (let attempt = 0; attempt < 16 && !successorServed; attempt++) {
          const delivery = gate.waitForDelivery(".Park");
          const response = caller.park({}).orThrow();
          const served = await withTimeout(
            delivery,
            15_000,
            "no real Park delivery was observed on the successor attempt",
          );
          assert((await response).value.startsWith("parked-"));
          if (successorConnections.has(served.connectionId)) {
            successorServed = true;
          }
        }
        assert(
          successorServed,
          "the successor must serve a new RPC on its own physical connection",
        );
      }
    } finally {
      for (const release of parkReleases) release();
      await heldPark?.catch(() => undefined);
      database.close();
      await caller.connection.close();
      await service.stop();
      await serviceExit;
    }
  }, runtimeOptions);
}

Deno.test(
  "a safe unpublished candidate drains accepted work while a successor becomes current",
  async () => {
    await runCandidateRejection("safe");
  },
);

Deno.test(
  "an unsafe unpublished candidate is physically closed regardless of held work",
  async () => {
    await runCandidateRejection("unsafe");
  },
);
