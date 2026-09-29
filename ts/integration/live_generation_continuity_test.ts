/**
 * Live continuity across an automatic transport generation.
 *
 * A live observation accepted on the original generation must keep delivering
 * while the service adopts a wider generation on authority growth, and a new
 * observation opened afterwards must be served by the new generation. The
 * handoff must not cancel the accepted observation.
 */

import { assert, assertEquals } from "@std/assert";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import { connect, credsAuthenticator } from "@nats-io/transport-node";
import type { NatsConnection } from "@nats-io/nats-core";
import { join } from "@std/path";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

type Runtime = Parameters<Parameters<typeof withTrellisRuntime>[0]>[0];

/** One admitted attachment as reported by the production admin surface. */
type Attachment = {
  participantId: string;
  runtimeConnectionId: string;
  connectionId: string;
  contextDigest: string;
};

async function attachmentsFor(
  runtime: Runtime,
  participantId: string,
): Promise<Attachment[]> {
  const page = await runtime.callAdminRpc("authConnectionsList", {}) as {
    items: Attachment[];
  };
  return page.items.filter((item) => item.participantId === participantId);
}

/** One raw broker connection identity, joined to its admission context. */
type ConnzIdentity = { server: string; cid: number; digest: string };

/** Read the broker's own CONNZ inventory with per-connection auth identity. */
async function connzIdentities(nc: NatsConnection): Promise<ConnzIdentity[]> {
  const message = await nc.request(
    "$SYS.REQ.SERVER.PING.CONNZ",
    JSON.stringify({ auth: true }),
    { timeout: 5_000 },
  );
  const reply = message.json<{
    server?: { id?: string };
    data?: { connections?: Array<{ cid?: number; authorized_user?: string }> };
  }>();
  const server = reply.server?.id ?? "";
  const identities: ConnzIdentity[] = [];
  for (const connection of reply.data?.connections ?? []) {
    const user = connection.authorized_user ?? "";
    if (!user.startsWith("trellis.auth.v1:") || connection.cid === undefined) {
      continue;
    }
    identities.push({
      server,
      cid: connection.cid,
      digest: user.split(":")[1] ?? "",
    });
  }
  return identities;
}

/** The broker CONNZ identities for the attachments carrying `digests`. */
function connzFor(
  identities: ConnzIdentity[],
  digests: Set<string>,
): string[] {
  return identities
    .filter((identity) => digests.has(identity.digest))
    .map((identity) => `${identity.server}:${identity.cid}`);
}

Deno.test(
  "an accepted live observation survives growth and new intake uses the new generation",
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const providerContract = participants.Provider.participant;
      const providerId = providerContract.identity;

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
        name: "live-generation-provider",
        contract: providerContract,
      });
      const service = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: providerContract,
        name: "live-generation-provider",
        seed: instance.seed,
      }).orThrow();
      const serviceExit = service.wait().catch((error: unknown) => error);
      let systemNc: NatsConnection | undefined;

      let feeds = 0;
      let cancelled = 0;
      await service.handleEcho(({ input }) => Result.ok(input));
      await service.handleWatch(async ({ emit, signal }) => {
        const feed = ++feeds;
        let frame = 0;
        while (!signal.aborted) {
          await emit({ value: `feed-${feed}-${++frame}` }).orThrow();
          await new Promise((resolve) => setTimeout(resolve, 25));
        }
        cancelled += 1;
      });

      const caller = await runtime.connectClient({
        name: "live-generation-caller",
        contract: participants.Caller.participant,
        timeout: 120_000,
      });

      const firstAbort = new AbortController();
      const secondAbort = new AbortController();
      try {
        const sysNc = await connect({
          servers: runtime.natsUrl,
          authenticator: credsAuthenticator(
            await Deno.readFile(
              join(runtime.workdir, "nats/creds/system.creds"),
            ),
          ),
        });
        systemNc = sysNc;
        const [before] = await attachmentsFor(runtime, providerId);
        assert(before, "the provider must have a physical attachment");
        const logical = before.runtimeConnectionId;
        const g1Identity = connzFor(
          await connzIdentities(sysNc),
          new Set([before.contextDigest]),
        );
        assertEquals(
          g1Identity.length,
          1,
          "the original generation must have exactly one broker CONNZ identity",
        );

        // 1. Observe on the original generation.
        const firstFeed = await caller.watch({}, {
          signal: firstAbort.signal,
        }).orThrow();
        const first = firstFeed[Symbol.asyncIterator]();
        assert((await first.next()).value?.value?.startsWith("feed-"));
        const firstFeedId = feeds;
        assertEquals(firstFeedId, 1);

        // 2. Grow authority: the service adopts a wider generation.
        await runtime.contracts.apply({ contract: providerContract });
        await runtime.waitFor(async () => {
          const items = (await attachmentsFor(runtime, providerId)).filter(
            (item) => item.runtimeConnectionId === logical,
          );
          return items.length >= 2 ? true : undefined;
        }, { timeoutMs: 90_000 });

        // 3. The accepted observation keeps delivering: no cancellation, and
        //    frames keep arriving from the same feed.
        for (let i = 0; i < 4; i++) {
          const frame = (await first.next()).value?.value;
          assert(
            typeof frame === "string" &&
              frame.startsWith(`feed-${firstFeedId}-`),
            "the accepted observation must keep delivering its own feed",
          );
        }
        assertEquals(cancelled, 0, "growth must not cancel an accepted feed");

        // 4. New intake is served by the new generation.
        const secondFeed = await caller.watch({}, {
          signal: secondAbort.signal,
        }).orThrow();
        const second = secondFeed[Symbol.asyncIterator]();
        assert((await second.next()).value?.value?.startsWith("feed-"));
        assertEquals(feeds, 2, "a new observation must open a fresh feed");
        assertEquals(service.connection.status.phase, "connected");

        // 5. Close the original-generation observation while the newer
        //    generation is live. The bounded public close exchange completes
        //    against the owner's terminal receipt without a spurious non-owner
        //    response ending it.
        const closeReceipt = await firstFeed.close().orThrow();
        assertEquals(
          closeReceipt.remote,
          "confirmed",
          "the owner must confirm the terminal close",
        );
        assertEquals(
          closeReceipt.cleanup,
          "complete",
          "the closed session's cleanup must complete",
        );

        // 5a. The original generation is physically gone from the broker's own
        //     CONNZ inventory: the exact captured server/cid identity is absent,
        //     not merely a digest-filtered view.
        await runtime.waitFor(async () => {
          const identities = await connzIdentities(sysNc);
          const present = new Set(
            identities.map((identity) => `${identity.server}:${identity.cid}`),
          );
          return g1Identity.every((identity) => !present.has(identity))
            ? true
            : undefined;
        }, { timeoutMs: 60_000 });

        // 5c. A fresh RPC is still served after the retired socket is gone.
        assertEquals(await caller.echo({ value: "after-reap" }).orThrow(), {
          value: "after-reap",
        });

        // 6. The newer observation is unaffected by the terminal close across
        //    the overlap.
        const afterClose = (await second.next()).value?.value;
        assert(
          typeof afterClose === "string" && afterClose.startsWith("feed-2-"),
          "the newer observation keeps delivering after the old one closes",
        );

        // 7. A fresh observation still opens while the terminal receipt of the
        //    closed session is retained on the shared manager.
        const thirdAbort = new AbortController();
        try {
          const thirdFeed = await caller.watch({}, {
            signal: thirdAbort.signal,
          }).orThrow();
          const third = thirdFeed[Symbol.asyncIterator]();
          assert((await third.next()).value?.value?.startsWith("feed-"));
          assertEquals(
            feeds,
            3,
            "a new observation opens after a terminal close",
          );
        } finally {
          thirdAbort.abort();
        }
      } finally {
        firstAbort.abort();
        secondAbort.abort();
        await caller.connection.close().catch(() => undefined);
        await service.stop();
        await serviceExit;
        await systemNc?.close().catch(() => undefined);
      }
    }, {
      authorization: {
        contextLifetimeSeconds: 76,
        refreshLeadSeconds: 15,
        refreshJitterSeconds: 0,
        minimumContextLifetimeSeconds: 46,
      },
    });
  },
);
