/**
 * Real-boundary superseded coverage-setup acceptance.
 *
 * A connected service holds an *idle* retained coverage watch for a caller's
 * verified context: a real accepted RPC caused the provider cache to verify and
 * retain that digest, and the watch pins the service's current physical
 * generation with its own lease even though no request still borrows it.
 *
 * The service then grows its own authority twice, monotonically:
 *
 * - `P1`: a wider generation `G2` opens. Its post-subscription readiness flush
 *   is held by the test transport proxy, so `G2` is provably not published yet;
 *   the exact proxy connection is captured. The real post-subscription JetStream
 *   `CONSUMER.INFO` reply that the verified idle caller digest's migration
 *   performs on `G2` is then withheld, so that migration setup is stalled while
 *   the old `G1` binding stays authoritative.
 * - While `G2` setup is held, the old coverage still verifies real application
 *   traffic: a real caller RPC is accepted and held on the published `G2`
 *   connection, keeping that socket alive independently of the provisional
 *   coverage pin.
 * - `P2`: a third wider generation `G3` opens while the `G2` `CONSUMER.INFO`
 *   reply is still unreleased and the held `G2` callback is still unresolved.
 *
 * The exact `G1` broker `server:cid` must then be absent from the validated
 * complete broker inventory: the superseding publication must abort the stalled
 * `G2` attempt, and its own settlement must wake the coverage reconcile so `G3`
 * replaces the idle `G1` watch — never waiting out the superseded setup. After
 * the withheld reply is released, the held callback completes exactly once,
 * fresh work continues on `G3`, and the stale `G2` socket reaps.
 *
 * This proves the supersession path, not old-watch revocation during setup,
 * retry-on-same-target failure, cold-load late publication, or physical-death
 * handling; those remain separate cases.
 */

import { encodePermissionTargetWasm } from "@oatscenter/trellis/auth";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import { connect, credsAuthenticator } from "@nats-io/transport-node";
import { type ConsumerInfo, jetstream } from "@nats-io/jetstream";
import type { NatsConnection } from "@nats-io/nats-core";
import { assert, assertEquals } from "@std/assert";
import { fromFileUrl, join } from "@std/path";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import type {
  NativeTransportGate,
  ResponseHoldBarrier,
} from "../../ts/packages/trellis-testkit/src/native_gate.ts";
import {
  admittedConnections,
  brokerConnectionKey,
  completeBrokerInventory,
} from "./_support/broker_inventory.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

type Runtime = Parameters<Parameters<typeof withTrellisRuntime>[0]>[0];

/** Reject with `message` if `promise` has not settled within `ms`. */
async function withTimeout<T>(
  promise: Promise<T>,
  ms: number,
  message: string,
): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => reject(new Error(message)), ms);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

/** Qualified API identity carrying the optional growth capabilities. */
const GROWTH_API = "runtime-trellis.transport_growth@v1";
const CAPABILITY_EXTEND = `${GROWTH_API}::extend`;
const CAPABILITY_EXTEND2 = `${GROWTH_API}::extend2`;
const PROVIDER_DEPLOYMENT = "coverage-supersession-provider";

/** The exact real JetStream reply whose withholding stalls one migration watch. */
const CONSUMER_INFO_PREFIX = "$JS.API.CONSUMER.INFO.";

/** Built server binary supplied by the live test harness. */
function serverBinary(): string {
  return Deno.env.get("TRELLIS_TEST_SERVER_BIN") ??
    fromFileUrl(new URL("../../target/debug/trellis-server", import.meta.url));
}

/**
 * Long lifetimes: growth is driven by real administrative grant revisions and
 * their change hint, so no scheduled renewal is needed during the test. The
 * bounded supersession wait therefore cannot be "repaired" by an unrelated
 * periodic refresh.
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

Deno.test(
  "a superseded coverage setup releases an idle old-generation watch",
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const providerContract =
        participants.TransportGrowthOperationProvider.participant;
      const callerContract = participants.TransportGrowthCaller.participant;
      const providerId = providerContract.identity;

      // The provider starts with both optional capabilities withheld, so its
      // first generation carries only the public surface.
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
        name: "coverage-supersession-provider",
        contract: providerContract,
      });

      const service = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: providerContract,
        name: "coverage-supersession-provider",
        seed: providerInstance.seed,
      }).orThrow();
      const serviceExit = service.wait().catch((error: unknown) => error);

      // `Park` is served on every generation; probes identify which physical
      // generation accepts a real delivery. A held probe's accepted callback
      // keeps that generation alive independently of any coverage watch.
      let parkStarts = 0;
      let parkIssued = 0;
      let advanceStarts = 0;
      let advanceIssued = 0;
      let idlePeerDigest: string | undefined;
      const requestStarts = new Map<string, number>();
      let holdProbes = false;
      const parkReleases: (() => void)[] = [];
      await service.handlePark(({ context }) => {
        assertEquals(context.caller.type, "verified");
        assert(context.caller.type === "verified");
        idlePeerDigest ??= context.caller.contextDigest;
        assert(context.requestId, "real Park requests must carry an identity");
        requestStarts.set(
          context.requestId,
          (requestStarts.get(context.requestId) ?? 0) + 1,
        );
        parkStarts += 1;
        const value = `parked-${parkStarts}`;
        if (!holdProbes) return Result.ok({ value });
        return new Promise((resolve) => {
          parkReleases.push(() => resolve(Result.ok({ value })));
        });
      });
      await service.handleAdvance(({ context }) => {
        assertEquals(context.caller.type, "verified");
        assert(
          context.requestId,
          "real Advance requests must carry an identity",
        );
        requestStarts.set(
          context.requestId,
          (requestStarts.get(context.requestId) ?? 0) + 1,
        );
        advanceStarts += 1;
        return Result.ok({});
      });
      await service.handleExtend(() => Result.ok({}));
      await service.handleExtend2(() => Result.ok({}));

      const caller = await runtime.connectClient({
        name: "coverage-supersession-caller",
        contract: callerContract,
        timeout: 30_000,
      });

      const gate: NativeTransportGate = runtime.nativeTransportGate();
      const system: NatsConnection = await connect({
        servers: runtime.natsUrl,
        authenticator: credsAuthenticator(
          await Deno.readFile(join(runtime.workdir, "nats/creds/system.creds")),
        ),
      });

      let infoHold: ResponseHoldBarrier | undefined;
      let heldG2: Promise<unknown> | undefined;
      let heldG2Release: (() => void) | undefined;
      let infoReleased = false;
      try {
        // Baseline established under `G1`. This real accepted RPC makes the
        // service's provider cache verify and retain the caller's context
        // digest as an idle coverage watch pinned to `G1`.
        parkIssued += 1;
        assertEquals((await caller.park({}).orThrow()).value, "parked-1");
        assert(
          idlePeerDigest,
          "the accepted caller must expose its verified digest",
        );
        await runtime.waitFor(() => parkStarts === 1, { timeoutMs: 30_000 });

        const [before] = await attachmentsFor(runtime, providerId);
        assert(before, "the provider must have a physical attachment");
        const logical = before.runtimeConnectionId;
        const g1Digest = before.contextDigest;
        // Capture the original generation's exact broker identity from the
        // broker's own validated inventory before any growth, so its later
        // absence is unambiguous.
        const g1Connections = admittedConnections(
          await completeBrokerInventory(system),
          new Set([g1Digest]),
        );
        assertEquals(
          g1Connections.length,
          1,
          "the original generation must have one exact broker identity",
        );
        const g1Identity = brokerConnectionKey(g1Connections[0]);
        const g1ServerId = g1Connections[0].server;
        assertEquals(
          system.info?.server_id,
          g1ServerId,
          "the captured attachment server must be the connection's own broker",
        );
        // The service's own `G1` proxy connection is the one already serving
        // the probed RPC route; the caller never subscribes `.Park`.
        const g1ConnectionId = gate.connections()
          .find((connection) =>
            connection.subs.some((sub) => sub.subject.endsWith(".Park"))
          )?.id;
        assert(
          g1ConnectionId !== undefined,
          "the original service generation must be observable on the proxy",
        );

        const baseline = await grantBinding(runtime, providerId);
        const p0 = baseline.grants.permissions;
        const extendAtom = await rpcAtom("Extend");
        const extend2Atom = await rpcAtom("Extend2");
        assert(
          !p0.map(atomKey).includes(atomKey(extendAtom)),
          "the first optional capability must start ungranted",
        );
        assert(
          !p0.map(atomKey).includes(atomKey(extend2Atom)),
          "the second optional capability must start ungranted",
        );

        // --- P1: open G2, hold its readiness flush, then publish it. ---
        gate.arm(".Park");
        await setGrants(runtime, baseline, [...p0, extendAtom]);
        const g2ConnectionId = await gate.barrierHeld();
        assert(
          g2ConnectionId > 0,
          "the P1 candidate barrier must be attributed",
        );
        assert(
          g2ConnectionId !== g1ConnectionId,
          "the P1 candidate is a distinct physical generation",
        );

        // Select the baseline peer's subscribed migration watch on exactly G2;
        // unrelated digest INFO and pre-subscription replies remain untouched.
        let migrationInfo: ConsumerInfo | undefined;
        infoHold = gate.armResponseHold(
          CONSUMER_INFO_PREFIX,
          g2ConnectionId,
          (body) => {
            const info = JSON.parse(
              new TextDecoder().decode(body),
            ) as ConsumerInfo;
            if (
              info.config?.filter_subject?.endsWith(
                `.revocation.${idlePeerDigest}`,
              ) &&
              info.push_bound === true
            ) {
              migrationInfo = info;
              return true;
            }
            return false;
          },
        );
        await gate.release();
        const heldInfo = await withTimeout(
          infoHold.held,
          30_000,
          "the G2 migration watch INFO reply was never withheld",
        );
        assertEquals(
          heldInfo.connectionId,
          g2ConnectionId,
          "the withheld migration INFO must be on the published G2 socket",
        );
        assert(
          migrationInfo,
          "the exact idle-peer post-subscription INFO must match",
        );
        const consumerCreated = Date.parse(migrationInfo.created);
        assert(
          Number.isFinite(consumerCreated),
          "the real consumer must report created time",
        );
        // The pinned JetStream API starts its default 5s request timer in
        // BaseApiClientImpl._request; PushConsumer.info delegates to that API.
        // Creation precedes getPushConsumer, subscription/flush, and this INFO.
        // Both processes use the host clock. Leave 250ms for timestamp rounding
        // and require the complete inventory to finish before this deadline.
        const infoTimeout = jetstream(system).getOptions().timeout;
        assert(infoTimeout !== undefined);
        const absenceDeadline = consumerCreated + infoTimeout - 250;

        const g2Attachment = await runtime.waitFor(async () => {
          const items = (await attachmentsFor(runtime, providerId)).filter(
            (item) => item.runtimeConnectionId === logical,
          );
          return items.find((item) =>
            item.connectionId !== before.connectionId
          );
        }, { timeoutMs: 30_000, intervalMs: 250 });
        const g2Digest = g2Attachment.contextDigest;
        assert(
          g2Digest !== g1Digest,
          "the published G2 generation must be admitted under its own context",
        );

        // While G2 setup is stalled, the old G1 coverage still verifies real
        // work. G1's generic intake is retired on publication, so new accepted
        // callbacks are served by G2; hold one to keep that socket alive.
        holdProbes = true;
        let heldG2Settled = false;
        for (let probe = 0; probe < 24 && !heldG2; probe++) {
          const before = parkStarts;
          const delivery = gate.waitForDelivery(".Park");
          parkIssued += 1;
          const response = caller.park({}).orThrow();
          response.then(() => {
            heldG2Settled = true;
          }, () => {
            heldG2Settled = true;
          });
          const served = await withTimeout(
            delivery,
            10_000,
            "no real Park delivery was observed",
          );
          // The handler records its release only once it has entered.
          await runtime.waitFor(() => parkStarts > before, {
            timeoutMs: 15_000,
          });
          if (served.connectionId === g2ConnectionId) {
            heldG2 = response;
            // Detach this probe's release so a later probe can never release
            // the deliberately held G2 callback.
            heldG2Release = parkReleases.pop();
            // Earlier completed probes already set the flag; from here only the
            // deliberately held callback may settle it.
            heldG2Settled = false;
          } else {
            parkReleases.shift()?.();
            await response;
          }
        }
        assert(
          heldG2,
          "the published G2 generation must accept a real held RPC",
        );
        assert(!heldG2Settled, "the held G2 RPC must remain unresolved");
        const heldG2Value = `parked-${parkStarts}`;

        // --- P2: open G3 while the G2 INFO reply is still withheld. ---
        const grown = await grantBinding(runtime, providerId);
        const p1 = grown.grants.permissions;
        assert(
          p1.map(atomKey).includes(atomKey(extendAtom)),
          "the P1 binding must retain the first optional capability",
        );
        gate.arm(".Park");
        await setGrants(runtime, grown, [...p1, extend2Atom]);
        const g3ConnectionId = await withTimeout(
          gate.barrierHeld(),
          Math.max(1, absenceDeadline - Date.now()),
          "P2 readiness was not held before the earliest ordinary INFO timeout",
        );
        assert(
          g3ConnectionId !== g1ConnectionId &&
            g3ConnectionId !== g2ConnectionId,
          "P2 readiness must identify exactly the distinct G3 physical connection",
        );
        await gate.release();

        // Real G3-delivered work, attributed to the successor's own connection.
        // Use an independent, unheld route: G2's Park intake serially awaits
        // its accepted callback, so a Park probe queued there would deadlock
        // this proof until cleanup rather than test successor progress.
        holdProbes = false;
        let servedG3 = false;
        while (!servedG3 && Date.now() < absenceDeadline) {
          const delivery = gate.waitForDelivery(".Advance");
          advanceIssued += 1;
          const response = caller.advance({}).orThrow();
          void response.catch(() => undefined);
          const [served, result] = await withTimeout(
            Promise.all([delivery, response]),
            Math.max(1, absenceDeadline - Date.now()),
            "no real Advance delivery was observed for the successor",
          );
          assertEquals(result, {});
          servedG3 = served.connectionId === g3ConnectionId;
          if (!servedG3) {
            await new Promise((resolve) => setTimeout(resolve, 25));
          }
        }
        assert(
          servedG3,
          "the successor generation must serve real work on its own connection",
        );

        // The functional progress discrimination: the exact G1 broker socket is
        // gone while the G2 INFO is still unreleased and the held G2 callback
        // is still unresolved. G3 replaced the idle G1 watch without waiting
        // for the stalled, superseded G2 setup.
        let afterSupersession = await withTimeout(
          completeBrokerInventory(system, { requiredServerIds: [g1ServerId] }),
          Math.max(1, absenceDeadline - Date.now()),
          "G1 complete inventory missed the pre-INFO-timeout deadline",
        );
        try {
          while (
            Date.now() < absenceDeadline &&
            afterSupersession.map(brokerConnectionKey).includes(g1Identity)
          ) {
            afterSupersession = await withTimeout(
              completeBrokerInventory(system, {
                requiredServerIds: [g1ServerId],
              }),
              Math.max(1, absenceDeadline - Date.now()),
              "G1 complete inventory missed the pre-INFO-timeout deadline",
            );
          }
        } catch (error) {
          console.log(
            `G1 progress failed ${
              Date.now() - consumerCreated
            }ms after consumer creation; ` +
              `deadline ${
                infoTimeout - 250
              }ms; last complete inventory retains G1=${
                afterSupersession.map(brokerConnectionKey).includes(g1Identity)
              }; ` +
              `G3 served=${servedG3}; INFO released=${infoReleased}; G2 settled=${heldG2Settled}`,
          );
          throw error;
        }
        assert(
          !afterSupersession.map(brokerConnectionKey).includes(g1Identity),
          "the exact superseded G1 broker identity must be absent",
        );
        assert(
          Date.now() < absenceDeadline,
          "G1 absence must precede any ordinary INFO timeout",
        );
        assert(!infoReleased, "the selected G2 INFO must still be withheld");
        console.log(
          `G1 absent ${
            Date.now() - consumerCreated
          }ms after consumer creation; earliest INFO timeout ${infoTimeout}ms`,
        );
        assert(
          !heldG2Settled,
          "the held G2 RPC must still be unresolved when G1 is gone",
        );
        assert(
          admittedConnections(afterSupersession, new Set([g2Digest])).length >=
            1,
          "the held G2 attachment must still be present",
        );

        // Release the withheld G2 INFO reply and complete the accepted callback
        // exactly once. A stale superseded setup must not strand or rebind the
        // coverage watch to G2.
        // Real work still serves on G3 while both G2 holds remain in place.
        const liveDigestDelivery = gate.waitForDelivery(".Advance");
        advanceIssued += 1;
        const liveDigest = caller.advance({}).orThrow();
        assertEquals(await liveDigest, {});
        assertEquals((await liveDigestDelivery).connectionId, g3ConnectionId);
        assert(!heldG2Settled);
        assert(!infoReleased);
        infoReleased = true;
        await infoHold.release();
        heldG2Release?.();
        const parked = await withTimeout(
          heldG2,
          10_000,
          "held G2 callback completion",
        );
        assertEquals(parked, { value: heldG2Value });

        const freshDelivery = gate.waitForDelivery(".Park");
        parkIssued += 1;
        const fresh = caller.park({}).orThrow();
        void fresh.catch(() => undefined);
        const servedFresh = await withTimeout(
          freshDelivery,
          15_000,
          "no fresh Park delivery was observed on the successor",
        );
        assert((await fresh).value.startsWith("parked-"));
        assertEquals(
          servedFresh.connectionId,
          g3ConnectionId,
          "fresh work must continue on the successor, not the stale G2 setup",
        );
        assertEquals(
          parkStarts,
          parkIssued,
          "each issued Park must start exactly once",
        );
        assertEquals(
          advanceStarts,
          advanceIssued,
          "each issued Advance must start exactly once",
        );
        assertEquals(
          requestStarts.size,
          parkIssued + advanceIssued,
          "each RPC must have its own request identity",
        );
        for (const count of requestStarts.values()) assertEquals(count, 1);

        // With the held callback complete, the stale G2 generation has no
        // remaining work and its exact broker socket reaps.
        const g2Identities = admittedConnections(
          afterSupersession,
          new Set([g2Digest]),
        );
        assert(g2Identities.length >= 1, "the held G2 attachment must exist");
        const g2Identity = brokerConnectionKey(g2Identities[0]);
        await runtime.waitFor(async () => {
          const present = new Set(
            (await completeBrokerInventory(system, {
              requiredServerIds: [g1ServerId],
            })).map(brokerConnectionKey),
          );
          return present.has(g2Identity) ? undefined : true;
        }, { timeoutMs: 60_000, intervalMs: 500 });
      } finally {
        await gate.release().catch(() => undefined);
        for (const release of parkReleases) release();
        heldG2Release?.();
        await infoHold?.release().catch(() => undefined);
        await withTimeout(caller.connection.close(), 10_000, "caller close")
          .catch(() => undefined);
        if (heldG2) {
          await withTimeout(heldG2, 10_000, "held G2 cleanup").catch(() =>
            undefined
          );
        }
        await withTimeout(service.stop(), 10_000, "service stop request").catch(
          () => undefined,
        );
        await withTimeout(serviceExit, 20_000, "service stop").catch(
          () => undefined,
        );
        await system.close().catch(() => undefined);
      }
    }, runtimeOptions);
  },
);
