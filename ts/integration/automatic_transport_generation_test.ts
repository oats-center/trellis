/**
 * Automatic transport-generation acceptance for a finite client capability.
 *
 * A connected app/client whose signed authority grows from `A` to `A+B` must
 * adopt the wider transport without any application refresh call and without
 * returning `transport_upgrade_required`. Capability `A` keeps working on its
 * original physical attachment while the wider generation opens, an in-flight
 * `A` exchange survives the rollover, and the newly granted `B` executes exactly
 * once on the real target.
 *
 * A routine renewal is observed through the authoritative persisted issuance of
 * a distinct authorization context for the caller; the original attachment must
 * be unchanged and its covered capability must still run.
 *
 * `connectionId` is the broker's physical attachment; `runtimeConnectionId` is
 * the logical SDK connection, which stays stable across the generation change.
 */

import { createClient } from "@libsql/client";
import { assert, assertEquals } from "@std/assert";
import { fromFileUrl, join } from "@std/path";
import { Result } from "@oatscenter/trellis";
import { TrellisClient } from "@oatscenter/trellis";
import { encodePermissionTargetWasm } from "@oatscenter/trellis/auth";
import { TrellisService } from "@oatscenter/trellis/service";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

type Runtime = Parameters<Parameters<typeof withTrellisRuntime>[0]>[0];

/** Qualified API identity the fixture declares for the growth capability. */
const GROWTH_API = "runtime-trellis.transport_growth@v1";

/**
 * One deployment carries one participant assignment, so the target lives in its
 * own deployment and nothing relies on the runtime default.
 */
const TARGET_DEPLOYMENT = "auto-generation-target";

/** Built server binary supplied by the live test harness. */
function serverBinary(): string {
  return Deno.env.get("TRELLIS_TEST_SERVER_BIN") ??
    fromFileUrl(new URL("../../target/debug/trellis-server", import.meta.url));
}

/**
 * Short lifetimes so a scheduled refresh crosses the growth promptly, without
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

/** Reads the grant binding owned by one participant. */
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

/** Exact permission atom for one declared RPC surface. */
async function rpcAtom(name: string) {
  return {
    action: "call" as const,
    target: await encodePermissionTargetWasm({
      kind: "apiSurface",
      api: GROWTH_API,
      surface: "rpc",
      name,
    }),
  };
}

/** Stable identity of one permission atom, independent of wire encoding. */
function atomKey(atom: { action: string; target: Uint8Array }): string {
  return `${atom.action}:${btoa(String.fromCharCode(...atom.target))}`;
}

Deno.test(
  "a grown finite capability adopts a new generation without disrupting in-flight work",
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const targetContract = participants.TransportGrowthTarget.participant;
      const callerContract = participants.TransportGrowthCaller.participant;

      // 1. The target serves A and B under its own deployment.
      await runtime.contracts.apply({
        deployment: TARGET_DEPLOYMENT,
        contract: targetContract,
      });
      const targetInstance = await runtime.services.createInstance({
        deployment: TARGET_DEPLOYMENT,
        name: "auto-generation-target",
        contract: targetContract,
      });
      const target = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: targetContract,
        name: "auto-generation-target",
        seed: targetInstance.seed,
      }).orThrow();
      const targetExit = target.wait().catch((error: unknown) => error);
      let advanceCalls = 0;
      let extendCalls = 0;
      // Only the in-flight exchange is held open across the rollover; the
      // renewal probe and the post-rollover call complete normally.
      let blockAdvance = false;
      const releaseAdvance = Promise.withResolvers<void>();
      await target.handleAdvance(async () => {
        advanceCalls += 1;
        if (blockAdvance) await releaseAdvance.promise;
        return Result.ok({});
      });
      await target.handleExtend(() => {
        extendCalls += 1;
        return Result.ok({});
      });

      // 2. Install the caller app and consent to its public capability alone, so
      //    `extend` starts unapproved and must be grown through a real
      //    administrative grant revision.
      const callerKey = await runtime.registerClient({
        name: "auto-generation-caller",
        contract: callerContract,
      });
      await runtime.ensurePortalConsentPolicy(callerContract.identity, []);
      const auth = runtime.clientAuth(callerKey);
      const caller = await TrellisClient.connect({
        trellisUrl: runtime.trellisUrl,
        participant: callerContract,
        name: "auto-generation-caller",
        auth: auth.auth,
        onAuthRequired: auth.onAuthRequired,
        // The in-flight A exchange is deliberately held across growth, so the
        // request budget must outlast the automatic adoption.
        timeout: 120_000,
      }).orThrow();

      const callerId = callerContract.identity;
      const extendAtom = await rpcAtom("Extend");
      const advanceAtom = await rpcAtom("Advance");
      let inFlight: Promise<unknown> | undefined;

      // Authoritative persisted issuance: distinct context digests stored for
      // this caller, the same real-SQLite signal the Rust generation test uses.
      const database = createClient({
        url: `file:${
          join(runtime.workdir, "trellis", "trellis.sqlite.platform")
        }`,
      });
      const renewalCount = async (): Promise<number> => {
        const result = await database.execute({
          sql:
            "SELECT COUNT(DISTINCT context_digest) AS count FROM auth_authorization_contexts WHERE participant_id = ?",
          args: [callerId],
        });
        return Number(result.rows[0].count);
      };

      try {
        // 3. The public capability is approved at connect and the optional
        //    `extend` capability starts unapproved, so growth must revise the
        //    grant explicitly.
        const initial = await grantBinding(runtime, callerId);
        const initialKeys = initial.grants.permissions.map(atomKey);
        assert(
          initialKeys.includes(atomKey(advanceAtom)),
          "the public Advance capability must be granted at connect",
        );
        assert(
          !initialKeys.includes(atomKey(extendAtom)),
          "the optional extend capability must not be granted at connect",
        );

        const [before] = await attachmentsFor(runtime, callerId);
        assert(before, "the caller must have a physical attachment");
        const logical = before.runtimeConnectionId;
        const physical = before.connectionId;

        // 4. Wait for a real renewal: the server persists a distinct context
        //    for this caller. Nothing here relies on elapsed time.
        const issuedBefore = await renewalCount();
        await runtime.waitFor(
          async () => (await renewalCount()) > issuedBefore ? true : undefined,
          { timeoutMs: 45_000, intervalMs: 250 },
        );
        //    An unchanged effective policy must not open a generation or
        //    replace the physical attachment.
        const renewed = (await attachmentsFor(runtime, callerId)).filter(
          (item) => item.runtimeConnectionId === logical,
        );
        assertEquals(
          renewed.length,
          1,
          "unchanged-policy renewal must not add a generation",
        );
        assertEquals(
          renewed[0].connectionId,
          physical,
          "unchanged-policy renewal must not replace the attachment",
        );

        // 5. A covered operation still runs on that original attachment.
        const afterRenewal = await caller.advance({}).orThrow();
        assertEquals(afterRenewal, {});
        await runtime.waitFor(() => advanceCalls === 1, { timeoutMs: 30_000 });

        // 6. Start a real in-flight A exchange and keep it open across growth.
        //    The rejection is observed later, so it is never an unhandled
        //    dangling rejection while the test drives the rollover.
        blockAdvance = true;
        inFlight = caller.advance({}).orThrow();
        let inFlightFailure: unknown;
        inFlight.catch((error) => {
          inFlightFailure = error;
        });
        await runtime.waitFor(() => advanceCalls === 2, { timeoutMs: 30_000 });

        // 7. Grow B through an exact administrative grant revision that keeps
        //    the full public capability and adds the optional one, so the new
        //    desired policy is a strict superset of the admitted one.
        await runtime.callAdminRpc("authGrantsSet", {
          expectedRevision: initial.revision,
          expiresAt: initial.expiresAt,
          grants: {
            format: initial.grants.format,
            permissions: [...initial.grants.permissions, extendAtom],
          },
          idempotencyKey: crypto.randomUUID(),
          installedRevision: initial.installedRevision,
          ownerId: initial.ownerId,
          ownerKind: initial.ownerKind,
          participantId: initial.participantId,
          platformPrivileges: initial.platformPrivileges,
        });

        // 8. Trellis opens a second admitted generation under the same logical
        //    connection, with no application-side refresh call.
        const adopted = await runtime.waitFor(async () => {
          const items = (await attachmentsFor(runtime, callerId)).filter(
            (item) => item.runtimeConnectionId === logical,
          );
          return items.length >= 2 ? items : false;
        }, { timeoutMs: 90_000 });
        assert(
          adopted.some((item) => item.connectionId !== physical),
          "automatic adoption must add a physical generation",
        );

        // 9. The newly granted capability now runs exactly once on the target.
        const extended = await caller.extend({}).orThrow();
        assertEquals(extended, {});
        assertEquals(extendCalls, 1);

        // 10. The A exchange held across the rollover completes on its original
        //     generation without a second execution.
        blockAdvance = false;
        releaseAdvance.resolve();
        const first = await inFlight;
        assertEquals(inFlightFailure, undefined);
        assertEquals(first, {});
        assertEquals(advanceCalls, 2);

        // 11. A keeps working after the rollover.
        const second = await caller.advance({}).orThrow();
        assertEquals(second, {});
        await runtime.waitFor(() => advanceCalls === 3, { timeoutMs: 30_000 });

        // 12. The logical connection stayed connected throughout.
        assertEquals(caller.connection.status.phase, "connected");
      } finally {
        blockAdvance = false;
        releaseAdvance.resolve();
        await inFlight?.catch(() => undefined);
        await caller.connection.close().catch(() => undefined);
        await target.connection.close().catch(() => undefined);
        await targetExit;
        database.close();
      }
    }, runtimeOptions);
  },
);
