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
              snapshot?: {
                id: string;
                revision: number;
                state: string;
                output?: { value: string };
                error?: { type: string; message: string };
              };
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
            throw new Error(
              `operation ended ${event.type}: ${
                JSON.stringify({
                  id: event.snapshot?.id,
                  revision: event.snapshot?.revision,
                  state: event.snapshot?.state,
                  error: event.snapshot?.error === undefined ? undefined : {
                    type: event.snapshot.error.type,
                    message: event.snapshot.error.message,
                  },
                })
              }`,
            );
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

Deno.test("an operation watch is fenced by its connection close", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "v3-operation-owner-provider",
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Provider.participant,
      seed: identity.seed,
    }).orThrow();
    await service.handleWork(async ({ op }) => {
      await op.started().orThrow();
      // Stay nonterminal until the owning caller stops.
      for (;;) {
        const accepted = await op.nextSignal("Continue").orThrow();
        await op.acknowledgeSignal(accepted.sequence).orThrow();
      }
    });
    const serviceExit = service.wait().then(
      () => ({ kind: "stopped" as const }),
      (error: unknown) => ({ kind: "failed" as const, error }),
    );
    let caller:
      | Awaited<
        ReturnType<
          typeof runtime.connectClient<typeof participants.Caller.participant>
        >
      >
      | undefined;
    const failures: unknown[] = [];
    try {
      caller = await runtime.connectClient({
        name: "v3-operation-owner-caller",
        contract: participants.Caller.participant,
      });
      const handle = await caller.work({ value: "ownership" }).start()
        .orThrow();
      const subscription = await handle.live({ updates: true }).orThrow();
      const iterator = subscription[Symbol.asyncIterator]();
      await iterator.next();
      // Attach the rejection handler before closing so the test never creates
      // its own unhandled rejection; an unowned close still shows as `threw`.
      const settled = iterator.next().then(
        (result) => ({ kind: "ended" as const, done: result.done === true }),
        (error: unknown) => ({ kind: "threw" as const, error }),
      );
      await caller.connection.close();
      assertEquals(
        await settled,
        { kind: "ended", done: true },
        "closing the transport must end an owned operation watch",
      );
      assertEquals(
        (await subscription.closed).reason,
        "cancelled",
        "the terminal outcome is the bounded local cancellation",
      );
    } catch (error) {
      failures.push(error);
    } finally {
      await caller?.connection.close().catch((error) => failures.push(error));
      await service.stop().catch((error) => failures.push(error));
      const exit = await serviceExit;
      if (exit.kind === "failed") failures.push(exit.error);
    }
    if (failures.length === 1) throw failures[0];
    if (failures.length > 1) {
      throw new AggregateError(
        failures,
        "Operation watch body or cleanup failed",
      );
    }
  });
});
