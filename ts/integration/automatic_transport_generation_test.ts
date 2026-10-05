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
import { connect, credsAuthenticator } from "@nats-io/transport-node";
import type { NatsConnection } from "@nats-io/nats-core";
import { Result } from "@oatscenter/trellis";
import { TrellisClient } from "@oatscenter/trellis";
import { encodePermissionTargetWasm } from "@oatscenter/trellis/auth";
import { TrellisService } from "@oatscenter/trellis/service";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import {
  admittedConnections,
  brokerConnectionKey,
  completeBrokerInventory,
} from "./_support/broker_inventory.ts";
import { repoTrellisSource, withTrellisRuntime } from "./_support/runtime.ts";

type Runtime = Parameters<Parameters<typeof withTrellisRuntime>[0]>[0];

/** Qualified API identity the fixture declares for the growth capability. */
const GROWTH_API = "runtime-trellis.transport_growth@v1";

/**
 * One deployment carries one participant assignment, so the target lives in its
 * own deployment and nothing relies on the runtime default.
 */
const TARGET_DEPLOYMENT = "auto-generation-target";

/** Built server binary supplied by the live test harness. */

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
  trellis: { source: repoTrellisSource() },
};

/** One admitted attachment as reported by the production admin surface. */
type Attachment = {
  participantId: string;
  runtimeConnectionId: string;
  connectionId: string;
  contextDigest: string;
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
      // The first in-flight exchange is held open across the rollover; the
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
      const overlapping: Promise<unknown>[] = [];
      let systemNc: NatsConnection | undefined;

      // Authoritative persisted issuance: distinct context digests stored for
      // this caller, the same real-SQLite signal the Rust generation test uses.
      const database = createClient({
        url: `file:${
          join(runtime.workdir, "data", "trellis", "platform.sqlite")
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
        // A system-account connection reads the broker's own authenticated
        // connection inventory, so an exact server:cid can be proven absent.
        const system = await connect({
          servers: runtime.natsUrl,
          authenticator: credsAuthenticator(
            await Deno.readFile(
              join(runtime.workdir, "config/trellis/nats/creds/system.creds"),
            ),
          ),
        });
        systemNc = system;
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
        // Capture the original generation's exact broker identity from the
        // broker's own inventory before any growth, so its later absence is
        // unambiguous rather than inferred from a projection.
        const g1Connections = admittedConnections(
          await completeBrokerInventory(system),
          new Set([before.contextDigest]),
        );
        assertEquals(
          g1Connections.length,
          1,
          "the original generation must have one exact broker identity",
        );
        const g1Identity = brokerConnectionKey(g1Connections[0]);
        // The captured attachment's server is the broker's own identity for
        // that connection. Pin it, cross-checked against the connection's
        // reported server, so every later absence read must cover exactly this
        // server and can never pass on a partial or dropped discovery listing.
        const g1ServerId = g1Connections[0].server;
        assertEquals(
          system.info?.server_id,
          g1ServerId,
          "the captured attachment server must be the connection's own broker",
        );

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

        // Queue concurrent acquisitions before growth while the first request's
        // handler-arrival barrier proves real G1 work is held. The production
        // RPC intake executes this handler serially, so later arrivals cannot be
        // awaited until the held reply is released.
        for (let i = 0; i < 8; i++) {
          const request = caller.advance({}).orThrow();
          void request.catch(() => undefined);
          overlapping.push(request);
        }

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

        // More acquisitions overlap automatic growth/cutover while G1 work is
        // still held. No manager, lease counters, or scheduling hooks are used.
        for (let i = 0; i < 8; i++) {
          const request = caller.advance({}).orThrow();
          void request.catch(() => undefined);
          overlapping.push(request);
        }

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
        assert(
          (await completeBrokerInventory(system, {
            requiredServerIds: [g1ServerId],
          })).some((item) => brokerConnectionKey(item) === g1Identity),
          "G1 must remain physically attached while its real RPC reply is held",
        );

        // 10. The A exchange held across the rollover completes on its original
        //     generation without a second execution.
        blockAdvance = false;
        // Start acquisitions as the last held G1 replies are released. This
        // exercises real disposal/admission overlap, but cannot force the exact
        // zero-notification microtask interleaving deterministically.
        for (let i = 0; i < 8; i++) {
          const request = caller.advance({}).orThrow();
          void request.catch(() => undefined);
          overlapping.push(request);
        }
        releaseAdvance.resolve();
        const first = await inFlight;
        const completed = await Promise.all(overlapping);
        assertEquals(inFlightFailure, undefined);
        assertEquals(first, {});
        assertEquals(completed, Array.from({ length: 24 }, () => ({})));
        assertEquals(advanceCalls, 26);

        // 11. With every held lease drained, the original generation is gone
        //     from the broker's own complete authenticated inventory: the exact
        //     captured server:cid is absent while that exact server still
        //     reports a validated listing, proving the idle cache released its
        //     pin rather than merely a digest-filtered view.
        await runtime.waitFor(async () => {
          const present = new Set(
            (await completeBrokerInventory(system, {
              requiredServerIds: [g1ServerId],
            })).map(brokerConnectionKey),
          );
          return present.has(g1Identity) ? undefined : true;
        }, { timeoutMs: 60_000 });

        // 12. A keeps working after the rollover and the reap.
        const second = await caller.advance({}).orThrow();
        assertEquals(second, {});
        await runtime.waitFor(() => advanceCalls === 27, { timeoutMs: 30_000 });

        // 13. The logical connection stayed connected throughout.
        assertEquals(caller.connection.status.phase, "connected");
      } finally {
        blockAdvance = false;
        releaseAdvance.resolve();
        await inFlight?.catch(() => undefined);
        await Promise.allSettled(overlapping);
        await caller.connection.close().catch(() => undefined);
        await target.connection.close().catch(() => undefined);
        await targetExit;
        await systemNc?.close().catch(() => undefined);
        database.close();
      }
    }, runtimeOptions);
  },
);
