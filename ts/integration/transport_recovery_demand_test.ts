import assert from "node:assert/strict";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import type { TrellisTestRuntime } from "@oatscenter/trellis-testkit";
import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

/**
 * Wait, with nothing asking for the transport, until the client's reconnect
 * attempts on the native path are at least 3.5 s apart: the idle backoff has
 * reached its long steps and the next scheduled attempt is 8 s away. Returns
 * the gaps between the attempts it saw.
 */
async function awaitIdleBackoff(
  runtime: TrellisTestRuntime,
): Promise<number[]> {
  const gaps: number[] = [];
  let seen = runtime.nativeConnectionAttempts();
  let seenAt: number | undefined;
  const deadline = Date.now() + 30_000;
  while (Date.now() < deadline) {
    await delay(10);
    const attempts = runtime.nativeConnectionAttempts();
    if (attempts === seen) continue;
    const now = Date.now();
    if (seenAt !== undefined) gaps.push(now - seenAt);
    seen = attempts;
    seenAt = now;
    if ((gaps.at(-1) ?? 0) >= 3_500) return gaps;
  }
  throw new Error(`idle backoff never reached 4 s; gaps ${gaps.join(", ")}`);
}

test(
  "waiting requests retry a lost broker at once, at most once per second, while an idle client backs off",
  { timeout: 120_000 },
  async () => {
    await withTrellisRuntime(
      async (runtime) => {
        await runtime.registerService({
          name: "recovery-demand-provider",
          contract: participants.Provider.participant,
        });
        const client = await runtime.connectClient({
          name: "recovery-demand",
          contract: apps.Caller.request,
          timeout: 3_000,
        });
        try {
          await client.state.saved.set({ value: "kept" }).orThrow();

          // Restart the broker and Trellis while the native path stays down,
          // so the outage outlasts the restart and the idle client climbs its
          // backoff without any request asking for the transport.
          runtime.interruptNativeTransport();
          await runtime.restart();
          const idleGaps = await awaitIdleBackoff(runtime);
          assert(
            idleGaps.every((gap) => gap >= 800),
            `idle client retried faster than its backoff: ${idleGaps.join(", ")}`,
          );

          // The broker is back and the next idle attempt is 8 s away. A
          // request with a 3 s timeout succeeds only because it asks for an
          // attempt right away.
          runtime.restoreNativeTransport();
          const stored = await client.state.saved.get().orThrow();
          assert.deepEqual(stored?.value, { value: "kept" });

          // A burst of requests during an outage retries about once per
          // second, not once per request and not on the idle schedule.
          runtime.interruptNativeTransport();
          await awaitIdleBackoff(runtime);
          const before = runtime.nativeConnectionAttempts();
          const burst = await Promise.all(
            Array.from({ length: 20 }, () => client.state.saved.get()),
          );
          const attempts = runtime.nativeConnectionAttempts() - before;
          assert(burst.every((result) => result.isErr()));
          assert(
            attempts >= 2 && attempts <= 4,
            `20 requests waiting 3 s made ${attempts} attempts`,
          );

          runtime.restoreNativeTransport();
          const recovered = await client.state.saved.get().orThrow();
          assert.deepEqual(recovered?.value, { value: "kept" });
        } finally {
          runtime.restoreNativeTransport();
          await client.connection.close();
        }
      },
      { interruptibleNativeProxy: true },
    );
  },
);
