/**
 * TS service provider generation handoff.
 *
 * A generated fixture `Provider` service connects with its optional `extras`
 * resource declined. After a real deployment consent approves it, the service's
 * provider authority grows and Trellis must move its generic provider RPC intake
 * to a new physical generation automatically — no application refresh — while an
 * in-flight handler accepted on the old generation finishes and the logical RPC
 * surface never goes down.
 *
 * `connectionId` is the broker's physical attachment; `runtimeConnectionId` is
 * the SDK logical connection and stays stable across the generation change.
 */

import { assert, assertEquals } from "@std/assert";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
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

      let echoCalls = 0;
      let blockEcho = false;
      let releaseEcho: (() => void) | undefined;
      try {
        // 2. The provider serves the public RPC from its logical core.
        await service.handleEcho(async ({ input }) => {
          echoCalls += 1;
          if (blockEcho) {
            await new Promise<void>((resolve) => {
              releaseEcho = resolve;
            });
          }
          return Result.ok(input);
        });

        const caller = await runtime.connectClient({
          name: "service-generation-caller",
          contract: participants.Caller.participant,
          // The held exchange deliberately outlasts the automatic adoption.
          timeout: 120_000,
        });

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

        const [initial] = await attachmentsFor(runtime, providerId);
        assert(initial, "the service must have a physical attachment");
        const logical = initial.runtimeConnectionId;
        const physical = initial.connectionId;

        // 4. Hold one accepted RPC handler open across the rollover.
        blockEcho = true;
        const inFlight = caller.echo({ value: "held" }).orThrow();
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

        // 6. The call accepted on the old generation still completes.
        blockEcho = false;
        releaseEcho?.();
        assertEquals(await inFlight, { value: "held" });
        assertEquals(inFlightFailure, undefined);
        assertEquals(echoCalls, 2);

        // 7. The retained resource handles follow the new generation.
        assertEquals(await records.get("pre").orThrow(), { value: "pre" });
        await records.put("post", { value: "post" });
        assertEquals(await records.get("post").orThrow(), { value: "post" });
        const preFile = (await files.get("pre")).orThrow();
        assertEquals(
          new TextDecoder().decode(await preFile.bytes().orThrow()),
          "pre-file",
        );
        await files.put("post", new TextEncoder().encode("post-file"));

        // 8. New intake is served after the old generation's intake was retired.
        assertEquals(await caller.echo({ value: "after" }).orThrow(), {
          value: "after",
        });
        await runtime.waitFor(() => echoCalls === 3, { timeoutMs: 30_000 });

        // 9. The logical connection stayed available and stable throughout.
        assertEquals(service.connection.status.phase, "connected");
        assert(
          (await attachmentsFor(runtime, providerId)).every(
            (item) => item.runtimeConnectionId === logical,
          ),
          "the logical connection identity must stay stable across growth",
        );

        await caller.connection.close().catch(() => undefined);
      } finally {
        blockEcho = false;
        releaseEcho?.();
        await service.stop();
        await serviceExit;
      }
    }, runtimeOptions);
  },
);
