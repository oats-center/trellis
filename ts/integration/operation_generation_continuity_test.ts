/**
 * Operation generation ownership across an automatic transport generation.
 *
 * An operation accepted on the original generation keeps its execution,
 * control, and observation working through authority growth and completes
 * exactly once on release. A new operation started after growth is accepted by
 * the new generation's retired-only-on-the-old-generation intake, and its own
 * control signal and observation are served through the new generation.
 */

import { assert, assertEquals } from "@std/assert";
import { TrellisService } from "@oatscenter/trellis/service";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

type Runtime = Parameters<Parameters<typeof withTrellisRuntime>[0]>[0];

type Attachment = {
  participantId: string;
  runtimeConnectionId: string;
  connectionId: string;
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

Deno.test(
  "an accepted operation survives growth with control and observation, and a post-growth operation is served by the new generation",
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
        name: "operation-generation-provider",
        contract: providerContract,
      });
      const service = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: providerContract,
        name: "operation-generation-provider",
        seed: instance.seed,
      }).orThrow();
      const serviceExit = service.wait().catch((error: unknown) => error);

      const releaseHeld = Promise.withResolvers<void>();
      const executions = new Map<string, number>();
      await service.handleWork(async ({ input, op }) => {
        const value = input.value;
        executions.set(value, (executions.get(value) ?? 0) + 1);
        await op.started().orThrow();
        if (value === "held") {
          await releaseHeld.promise;
        } else if (value === "fresh") {
          // Stay nonterminal until the caller signals through the new
          // generation, so the test exercises post-growth control.
          const signal = await op.nextSignal("Continue").orThrow();
          await op.acknowledgeSignal(signal.sequence).orThrow();
        }
        return await op.complete({ value: `done:${value}` }).orThrow();
      });

      const caller = await runtime.connectClient({
        name: "operation-generation-caller",
        contract: participants.Caller.participant,
        timeout: 120_000,
      });

      try {
        const before = (await attachmentsFor(runtime, providerId))[0];
        assert(before, "the provider must have a physical attachment");

        // 1. Start an operation held open on the original generation, observed
        //    through that generation's live provider.
        const held = await caller.work({ value: "held" }).start().orThrow();
        const heldSubscription = await held.live({ updates: true }).orThrow();
        let heldTerminal: string | undefined;
        const heldPump = (async () => {
          for await (
            const event of heldSubscription as AsyncIterable<
              {
                type: string;
                snapshot?: { state: string; output?: { value: string } };
              }
            >
          ) {
            if (event.type === "completed") {
              heldTerminal = event.snapshot?.output?.value;
            } else if (event.type === "failed" || event.type === "cancelled") {
              throw new Error(`held operation ended ${event.type}`);
            }
          }
        })();
        await runtime.waitFor(() => executions.get("held") === 1, {
          timeoutMs: 30_000,
        });

        // 2. Grow authority: the service adopts a wider generation while the
        //    operation is still running on the original one.
        await runtime.contracts.apply({ contract: providerContract });
        await runtime.waitFor(async () => {
          const items = (await attachmentsFor(runtime, providerId)).filter(
            (item) => item.runtimeConnectionId === before.runtimeConnectionId,
          );
          return items.length >= 2 ? true : undefined;
        }, { timeoutMs: 90_000 });

        // 3. Control still reaches the accepted operation after growth.
        await held.signal("Continue", { value: "continue" }).orThrow();

        // 4. A post-growth operation is served by the new generation: its intake
        //    is only still installed there (the old generation's generic intake
        //    was retired at cutover), and its control signal and observation are
        //    served through that generation.
        const fresh = await caller.work({ value: "fresh" }).start().orThrow();
        const freshSubscription = await fresh.live({ updates: true })
          .orThrow();
        let freshTerminal: string | undefined;
        const freshPump = (async () => {
          for await (
            const event of freshSubscription as AsyncIterable<
              {
                type: string;
                snapshot?: { state: string; output?: { value: string } };
              }
            >
          ) {
            if (event.type === "completed") {
              freshTerminal = event.snapshot?.output?.value;
            } else if (event.type === "failed" || event.type === "cancelled") {
              throw new Error(`fresh operation ended ${event.type}`);
            }
          }
        })();
        await runtime.waitFor(() => executions.get("fresh") === 1, {
          timeoutMs: 30_000,
        });
        // The fresh operation only completes after this signal, so a successful
        // completion proves control reached it after growth.
        await fresh.signal("Continue", { value: "continue" }).orThrow();
        const freshResult = await fresh.wait().orThrow();
        assertEquals(freshResult.state, "completed");
        assertEquals(freshResult.output, { value: "done:fresh" });
        await freshPump;
        assertEquals(freshTerminal, "done:fresh");
        assertEquals(executions.get("fresh"), 1);

        // 5. The held operation completes exactly once on release.
        releaseHeld.resolve();
        const terminal = await held.wait().orThrow();
        assertEquals(terminal.state, "completed");
        assertEquals(terminal.output, { value: "done:held" });
        await heldPump;
        assertEquals(heldTerminal, "done:held");
        assertEquals(executions.get("held"), 1);
        assertEquals(service.connection.status.phase, "connected");
      } finally {
        releaseHeld.resolve();
        await caller.connection.close().catch(() => undefined);
        await service.stop();
        await serviceExit;
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
