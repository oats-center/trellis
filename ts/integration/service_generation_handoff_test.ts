/**
 * TS service provider generation handoff.
 *
 * A generated fixture `Provider` service connects with its optional `extras`
 * resource declined. After a real deployment consent approves it, the service's
 * provider authority grows and Trellis must move its generic provider RPC intake
 * to a new physical generation automatically — no application refresh — while an
 * in-flight handler accepted on the old generation finishes, an accepted Live
 * observation keeps delivering, and the logical RPC surface never goes down.
 *
 * Once every accepted lease on the original generation is relinquished, that
 * exact physical attachment must be reaped from the broker. A following ordinary
 * authorization renewal must then be installed in place on the grown
 * generation: the provider's own generated request must carry the renewed
 * context digest, with no extra physical attachment.
 *
 * `connectionId` is the broker's physical attachment; `runtimeConnectionId` is
 * the SDK logical connection and stays stable across the generation change.
 */

import { createClient } from "@libsql/client";
import { assert, assertEquals } from "@std/assert";
import { join } from "@std/path";
import { connect, credsAuthenticator } from "@nats-io/transport-node";
import type { NatsConnection } from "@nats-io/nats-core";
import {
  type LiveSubscription,
  Result,
  type SessionCaller,
} from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import {
  admittedConnections,
  brokerConnectionKey,
  completeBrokerInventory,
} from "./_support/broker_inventory.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

type Runtime = Parameters<Parameters<typeof withTrellisRuntime>[0]>[0];

/** Short lifetimes so growth converges promptly. */
const runtimeOptions = {
  authorization: {
    contextLifetimeSeconds: 76,
    refreshLeadSeconds: 15,
    refreshJitterSeconds: 0,
    minimumContextLifetimeSeconds: 46,
  },
};

/**
 * Payload prefix that marks a provider-owned renewal probe. A probe is issued by
 * the provider to its own mounted target so the handler can read the exact
 * context digest its own generated request carries.
 */
const RENEWAL_PROBE_PREFIX = "renewal-probe:";

/** One admitted attachment as reported by the production admin surface. */
type Attachment = {
  participantId: string;
  runtimeConnectionId: string;
  connectionId: string;
  contextDigest: string;
  connectedAt: bigint;
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

Deno.test(
  "a TS service provider moves RPC intake to a new generation after authority growth",
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const providerContract = participants.Provider.participant;
      const providerId = providerContract.identity;

      // 1. Deploy the provider with its optional resource declined.
      await runtime.contracts.install({ contract: providerContract });
      const requested = await runtime.contracts.requestApply({
        contract: providerContract,
      });
      if (requested.status === "approval_required") {
        await runtime.contracts.approveApply(requested.pendingId, {
          excludeResources: ["extras"],
        });
      }
      const instance = await runtime.services.createInstance({
        name: "service-generation-provider",
        contract: providerContract,
      });
      const service = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: providerContract,
        name: "service-generation-provider",
        seed: instance.seed,
      }).orThrow();
      const serviceExit = service.wait().catch((error: unknown) => error);

      // Authoritative persisted issuance: distinct context digests stored for
      // this provider, the same real-SQLite signal the Rust generation test
      // uses. Issuance alone is not installation proof; the provider's own
      // generated request below proves the digest is installed.
      const database = createClient({
        url: `file:${
          join(runtime.workdir, "trellis", "trellis.sqlite.platform")
        }`,
      });
      const issuedProviderDigests = async (): Promise<Set<string>> => {
        const result = await database.execute({
          sql:
            "SELECT DISTINCT context_digest AS digest FROM auth_authorization_contexts WHERE participant_id = ?",
          args: [providerId],
        });
        return new Set(result.rows.map((row) => String(row.digest)));
      };

      let echoCalls = 0;
      let blockEcho = false;
      let releaseEcho: (() => void) | undefined;
      const probeCallers: SessionCaller[] = [];
      let feeds = 0;
      let cancelled = 0;
      const firstFeedContinuation = Promise.withResolvers<void>();

      let systemNc: NatsConnection | undefined;
      let callerClose: (() => Promise<void>) | undefined;
      let inFlight: Promise<unknown> | undefined;
      const firstAbort = new AbortController();
      const freshAbort = new AbortController();
      let firstFeed: LiveSubscription<{ value: string }> | undefined;
      let freshFeed: LiveSubscription<{ value: string }> | undefined;

      try {
        // 2. The provider serves the public RPC and Live source from its
        //    logical core. The self-probe reads the caller projection its own
        //    generated request is verified under.
        await service.handleEcho(async ({ input, context }) => {
          if (input.value.startsWith(RENEWAL_PROBE_PREFIX)) {
            probeCallers.push(context.caller);
            return Result.ok(input);
          }
          echoCalls += 1;
          if (blockEcho) {
            await new Promise<void>((resolve) => {
              releaseEcho = resolve;
            });
          }
          return Result.ok(input);
        });
        await service.handleWatch(async ({ emit, signal }) => {
          const feed = ++feeds;
          let frame = 0;
          try {
            while (!signal.aborted) {
              await emit({ value: `feed-${feed}-${++frame}` }).orThrow();
              if (feed === 1 && frame === 1) {
                await firstFeedContinuation.promise;
              }
              await new Promise((resolve) => setTimeout(resolve, 25));
            }
          } finally {
            cancelled += 1;
          }
        });

        const caller = await runtime.connectClient({
          name: "service-generation-caller",
          contract: participants.Caller.participant,
          // The held exchange deliberately outlasts the automatic adoption.
          timeout: 120_000,
        });
        callerClose = () => caller.connection.close().catch(() => undefined);

        // 3. Baseline RPC and retained resource handles on the initial
        //    generation. The handles are held across the whole rollover.
        assertEquals(await caller.echo({ value: "before" }).orThrow(), {
          value: "before",
        });
        await runtime.waitFor(() => echoCalls === 1, { timeoutMs: 30_000 });
        const records = service.kv.records;
        assert(records, "the required KV must be bound");
        await records.put("pre", { value: "pre" });
        const files = await service.store.files.open().orThrow();
        await files.put("pre", new TextEncoder().encode("pre-file"));

        // 3a. An accepted Live observation opens on the initial generation and
        //     starts delivering its own feed, alongside the held Echo.
        firstFeed = await caller.watch({}, { signal: firstAbort.signal })
          .orThrow();
        const firstFrames = firstFeed[Symbol.asyncIterator]();
        const baselineFrame = (await firstFrames.next()).value?.value;
        assert(
          typeof baselineFrame === "string" &&
            baselineFrame.startsWith("feed-1-"),
          "the baseline observation must deliver its own feed",
        );
        assertEquals(feeds, 1, "the baseline observation must open one feed");
        assertEquals(cancelled, 0, "an accepted feed must not be cancelled");

        // 3b. Capture the original generation's exact broker identity from the
        //     broker's own authenticated inventory while its admitted digest is
        //     known, so its later absence is unambiguous.
        const [initial] = await attachmentsFor(runtime, providerId);
        assert(initial, "the service must have a physical attachment");
        const logical = initial.runtimeConnectionId;
        const physical = initial.connectionId;
        const system = await connect({
          servers: runtime.natsUrl,
          authenticator: credsAuthenticator(
            await Deno.readFile(
              join(runtime.workdir, "nats/creds/system.creds"),
            ),
          ),
        });
        systemNc = system;
        const g1Connections = admittedConnections(
          await completeBrokerInventory(system),
          new Set([initial.contextDigest]),
        );
        assertEquals(
          g1Connections.length,
          1,
          "the original generation must have one exact broker identity",
        );
        const g1Identity = brokerConnectionKey(g1Connections[0]);
        // Pin the captured attachment's server for every later absence read, so
        // it can never pass on a partial or dropped discovery listing.
        const g1ServerId = g1Connections[0].server;
        assertEquals(
          system.info?.server_id,
          g1ServerId,
          "the captured attachment server must be the connection's own broker",
        );

        // 4. Hold one accepted RPC handler open across the rollover.
        blockEcho = true;
        inFlight = caller.echo({ value: "held" }).orThrow();
        let inFlightFailure: unknown;
        inFlight.catch((error) => {
          inFlightFailure = error;
        });
        await runtime.waitFor(() => echoCalls === 2, { timeoutMs: 30_000 });

        // 5. Grow authority through the real deployment consent. The service
        //    adopts automatically; no application refresh call is made.
        await runtime.contracts.apply({ contract: providerContract });
        const adopted = await runtime.waitFor(async () => {
          const items = (await attachmentsFor(runtime, providerId)).filter(
            (item) => item.runtimeConnectionId === logical,
          );
          return items.length >= 2 ? items : false;
        }, { timeoutMs: 90_000 });
        assert(
          adopted.some((item) => item.connectionId !== physical),
          "authority growth must adopt a new physical generation",
        );

        // 6. The call accepted on the old generation still completes once.
        blockEcho = false;
        releaseEcho?.();
        assertEquals(await inFlight, { value: "held" });
        assertEquals(inFlightFailure, undefined);
        assertEquals(echoCalls, 2);

        // 7. The retained resource handles follow the new generation and the
        //    newly approved optional resource materializes on it.
        assertEquals(await records.get("pre").orThrow(), { value: "pre" });
        await records.put("post", { value: "post" });
        assertEquals(await records.get("post").orThrow(), { value: "post" });
        const preFile = (await files.get("pre")).orThrow();
        assertEquals(
          new TextDecoder().decode(await preFile.bytes().orThrow()),
          "pre-file",
        );
        await files.put("post", new TextEncoder().encode("post-file"));
        const extras = await runtime.waitFor(
          () => service.kv.extras ?? false,
          { timeoutMs: 60_000 },
        );
        await extras.put("grown", { value: "grown" });
        assertEquals(await extras.get("grown").orThrow(), { value: "grown" });

        // 7a. Only successful grown-generation work releases the original
        //     application's producer. These frames cannot have been buffered
        //     before growth; the same accepted observation must deliver them.
        firstFeedContinuation.resolve();
        for (let i = 0; i < 4; i++) {
          const frame = (await firstFrames.next()).value?.value;
          assertEquals(
            frame,
            `feed-1-${i + 2}`,
            "the accepted observation must deliver newly produced post-growth frames",
          );
        }
        assertEquals(cancelled, 0, "growth must not cancel an accepted feed");
        assert(
          (await attachmentsFor(runtime, providerId)).some((item) =>
            item.connectionId === physical
          ),
          "the observation's original attachment must remain while it is open",
        );

        // 8. Explicitly close the baseline observation with the supported
        //    confirmed/complete receipt, await its terminal, and observe the
        //    owner's own terminal rather than inferring one from scheduling.
        const baselineClose = await firstFeed.close().orThrow();
        assertEquals(
          baselineClose.remote,
          "confirmed",
          "the owner must confirm the terminal close",
        );
        assertEquals(
          baselineClose.cleanup,
          "complete",
          "the closed session's cleanup must complete",
        );
        await firstFeed.closed;
        await runtime.waitFor(() => cancelled === 1, { timeoutMs: 30_000 });
        firstFeed = undefined;

        // 9. With every accepted lease relinquished, the original generation is
        //    gone from the broker's own complete authenticated inventory: the
        //    exact captured server:cid is absent while that exact server still
        //    reports a validated listing.
        await runtime.waitFor(async () => {
          const present = new Set(
            (await completeBrokerInventory(system, {
              requiredServerIds: [g1ServerId],
            })).map(brokerConnectionKey),
          );
          return present.has(g1Identity) ? undefined : true;
        }, { timeoutMs: 60_000 });

        // 10. New intake is served by the surviving grown generation, and its
        //    exact broker identity is captured under the grown context.
        assertEquals(await caller.echo({ value: "after-reap" }).orThrow(), {
          value: "after-reap",
        });
        await runtime.waitFor(() => echoCalls === 3, { timeoutMs: 30_000 });
        const surviving = await attachmentsFor(runtime, providerId);
        assertEquals(
          surviving.length,
          1,
          "exactly one provider attachment must survive the reap",
        );
        assertEquals(
          surviving[0].runtimeConnectionId,
          logical,
          "the logical connection identity must stay stable across the reap",
        );
        const grownDigest = surviving[0].contextDigest;
        const grownPhysical = surviving[0].connectionId;
        assert(
          grownPhysical !== physical,
          "the surviving attachment must be the grown generation",
        );
        const g2Connections = admittedConnections(
          await completeBrokerInventory(system, {
            requiredServerIds: [g1ServerId],
          }),
          new Set([grownDigest]),
        );
        assertEquals(
          g2Connections.length,
          1,
          "the grown generation must have one exact broker identity",
        );
        const g2Identity = brokerConnectionKey(g2Connections[0]);
        const g2ServerId = g2Connections[0].server;

        // 11. After the reap, wait for a new ordinary authorization renewal and
        //     prove it is actually installed: the provider's own generated
        //     request to its mounted target carries a distinct post-reap issued
        //     context digest. The baseline is captured only after the reap, so a
        //     prior pre-growth renewal cannot satisfy it, and a transient probe
        //     failure is retried within the bounded wait.
        const issuedAtReap = await issuedProviderDigests();
        const installedDigest = await runtime.waitFor(async () => {
          const fresh = [...await issuedProviderDigests()].filter((digest) =>
            !issuedAtReap.has(digest) && digest !== grownDigest
          );
          if (fresh.length === 0) return undefined;
          try {
            await service.echo({
              value: `${RENEWAL_PROBE_PREFIX}${crypto.randomUUID()}`,
            }).orThrow();
          } catch {
            return undefined;
          }
          const probe = probeCallers.at(-1);
          if (!probe || probe.type !== "verified") return undefined;
          return fresh.includes(probe.contextDigest)
            ? probe.contextDigest
            : undefined;
        }, { timeoutMs: 90_000, intervalMs: 1_000 });
        assert(
          installedDigest !== grownDigest,
          "the installed renewal must carry a distinct context",
        );

        // 12. The renewal was installed in place: no extra provider socket, the
        //     grown generation's exact broker identity is unchanged, and the
        //     reaped original generation stays absent.
        const afterRenewal = await attachmentsFor(runtime, providerId);
        assertEquals(
          afterRenewal.length,
          1,
          "an ordinary renewal must not open another socket",
        );
        assertEquals(
          afterRenewal[0].connectionId,
          grownPhysical,
          "an ordinary renewal must not replace the physical attachment",
        );
        const finalInventory = await completeBrokerInventory(system, {
          requiredServerIds: [g1ServerId, g2ServerId],
        });
        const finalPresent = new Set(finalInventory.map(brokerConnectionKey));
        assert(
          !finalPresent.has(g1Identity),
          "the reaped original generation must stay absent",
        );
        assertEquals(
          admittedConnections(finalInventory, new Set([grownDigest])).map(
            brokerConnectionKey,
          ),
          [g2Identity],
          "the grown generation's exact broker identity must survive in place",
        );

        // 13. Fresh ordinary intake and a fresh observation both work after the
        //     installed renewal, and the fresh observation delivers a real event
        //     after the older receiving generation drained.
        assertEquals(await caller.echo({ value: "post-renewal" }).orThrow(), {
          value: "post-renewal",
        });
        await runtime.waitFor(() => echoCalls === 4, { timeoutMs: 30_000 });
        freshFeed = await caller.watch({}, { signal: freshAbort.signal })
          .orThrow();
        const freshFrames = freshFeed[Symbol.asyncIterator]();
        const freshFrame = (await freshFrames.next()).value?.value;
        assert(
          typeof freshFrame === "string" && freshFrame.startsWith("feed-2-"),
          "the fresh observation must deliver a real event",
        );
        assertEquals(feeds, 2, "the fresh observation must open a new feed");
        const freshClose = await freshFeed.close().orThrow();
        assertEquals(
          freshClose.remote,
          "confirmed",
          "the owner must confirm the fresh close",
        );
        assertEquals(
          freshClose.cleanup,
          "complete",
          "the fresh session's cleanup must complete",
        );
        await freshFeed.closed;
        await runtime.waitFor(() => cancelled === 2, { timeoutMs: 30_000 });
        freshFeed = undefined;

        // 14. The retained resource handles are still usable and the logical
        //     connection stayed available and stable throughout.
        assertEquals(await records.get("post").orThrow(), { value: "post" });
        const postFile = (await files.get("post")).orThrow();
        assertEquals(
          new TextDecoder().decode(await postFile.bytes().orThrow()),
          "post-file",
        );
        assertEquals(service.connection.status.phase, "connected");
        assert(
          (await attachmentsFor(runtime, providerId)).every(
            (item) => item.runtimeConnectionId === logical,
          ),
          "the logical connection identity must stay stable across growth",
        );
      } finally {
        blockEcho = false;
        releaseEcho?.();
        firstFeedContinuation.resolve();
        await inFlight?.catch(() => undefined);
        firstAbort.abort();
        freshAbort.abort();
        await firstFeed?.close().orThrow().catch(() => undefined);
        await freshFeed?.close().orThrow().catch(() => undefined);
        await callerClose?.();
        await service.stop();
        await serviceExit;
        await systemNc?.close().catch(() => undefined);
        database.close();
      }
    }, runtimeOptions);
  },
);
