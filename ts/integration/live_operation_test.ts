import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import { assertEquals } from "@std/assert";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

const UPDATES = 129;
const BATCH = 16;
const TWO_HEARTBEATS_MS = 21_000;

Deno.test("V3 Operation observation delivers typed updates then terminal", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "v3-operation-provider",
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Provider.participant,
      seed: identity.seed,
    }).orThrow();
    let executions = 0;
    await service.handleWork(async ({ input, op }) => {
      executions += 1;
      await op.started().orThrow();
      let emitted = 0;
      while (emitted < UPDATES) {
        // Each application acknowledgement releases at most one 16-update
        // batch, so the observer is active before any transient update flows.
        const accepted = await op.nextSignal("Continue").orThrow();
        const batch = Math.min(BATCH, UPDATES - emitted);
        for (let index = 0; index < batch; index += 1) {
          emitted += 1;
          await op.emitUpdate({
            value: `u${emitted}`,
            nested: {
              count: BigInt(emitted),
              payload: new Uint8Array([1, 2, emitted % 251]),
            },
          }).orThrow();
        }
        await op.acknowledgeSignal(accepted.sequence).orThrow();
      }
      // Stay nonterminal across two fresh observer heartbeat exchanges, then
      // complete through the ordinary operation lifecycle.
      const finish = await op.nextSignal("Continue").orThrow();
      await op.acknowledgeSignal(finish.sequence).orThrow();
      await new Promise((resolve) => setTimeout(resolve, TWO_HEARTBEATS_MS));
      return await op.complete({ value: `done:${input.value}` }).orThrow();
    });
    const serviceExit = service.wait();
    const caller = await runtime.connectClient({
      name: "v3-operation-caller",
      contract: participants.Caller.participant,
    });
    try {
      const handle = await caller.work({ value: "observe" }).start()
        .orThrow();
      const subscription = await handle.live({ updates: true }).orThrow();
      const updates: string[] = [];
      let terminalState: string | undefined;
      let terminalOutput: string | undefined;
      const pump = (async () => {
        for await (
          const event of subscription as AsyncIterable<
            {
              type: string;
              update?: { value: string };
              snapshot?: { state: string; output?: { value: string } };
            }
          >
        ) {
          if (event.type === "update") {
            updates.push(event.update?.value ?? "");
          } else if (event.type === "completed") {
            terminalState = event.snapshot?.state;
            terminalOutput = event.snapshot?.output?.value;
            // Do not break: let the normal transport END complete the stream.
          } else if (event.type === "failed" || event.type === "cancelled") {
            throw new Error(`operation ended ${event.type}`);
          }
        }
      })();
      const batches = Math.ceil(UPDATES / BATCH);
      for (let index = 0; index < batches; index += 1) {
        await handle.signal("Continue", { value: "continue" }).orThrow();
        const expected = Math.min((index + 1) * BATCH, UPDATES);
        await runtime.waitFor(() => updates.length >= expected, {
          timeoutMs: 30_000,
        });
      }
      await handle.signal("Continue", { value: "continue" }).orThrow();
      await pump;

      assertEquals(updates.length, UPDATES, "every typed update delivered");
      for (let index = 0; index < UPDATES; index += 1) {
        assertEquals(updates[index], `u${index + 1}`, "updates stay ordered");
      }
      assertEquals(terminalState, "completed", "terminal business snapshot");
      assertEquals(terminalOutput, "done:observe", "terminal business output");
      const outcome = await subscription.closed;
      assertEquals(
        outcome.reason,
        "complete",
        "the subscription completes with a normal transport END",
      );
      assertEquals(
        executions,
        1,
        "one business execution, no observer restart",
      );
    } finally {
      await caller.connection.close();
      await service.stop();
      await serviceExit;
    }
  });
});
