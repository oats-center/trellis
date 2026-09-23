/**
 * Real-boundary live flow acceptance.
 *
 * Ordered handoff under a real consumption pause, bounded window pressure and
 * contiguous credit behaviour across an actual broker, provider and caller.
 */

import { TrellisService } from "@oatscenter/trellis/service";
import { assertEquals } from "@std/assert";
import { fromFileUrl } from "@std/path";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

/** Built server binary supplied by the live test harness. */
function serverBinary(): string {
  return Deno.env.get("TRELLIS_TEST_SERVER_BIN") ??
    fromFileUrl(
      new URL("../../target/debug/trellis-server", import.meta.url),
    );
}

const runtimeOptions = {
  trellis: {
    command: {
      cmd: serverBinary(),
      args: ["--config", "{config}", "all"],
    },
  },
};

Deno.test("T03 a consumption pause fills the window and resumes in order", async () => {
  await withTrellisRuntime(async (runtime) => {
    const totalFrames = 200;
    let emitted = 0;
    const identity = await runtime.registerService({
      name: `live-flow-${crypto.randomUUID()}`,
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Provider.participant,
      name: `live-flow-${crypto.randomUUID()}`,
      seed: identity.seed,
    }).orThrow();
    const serviceExit = service.wait().catch((error: unknown) => error);
    await service.handleWatch(async ({ emit }) => {
      // Emit as fast as credit allows; the bounded window must apply
      // backpressure while the consumer is paused.
      for (let frame = 1; frame <= totalFrames; frame++) {
        await emit({ value: `frame-${frame}` }).orThrow();
        emitted += 1;
      }
    });

    const client = await runtime.connectClient({
      name: "live-flow-caller",
      contract: participants.Caller.participant,
    });
    try {
      const handle = await client.watch({}).orThrow();
      const iterator = handle[Symbol.asyncIterator]();
      const seen: string[] = [];
      // Consume a few, then pause long enough for the provider to fill its
      // credit window and block awaiting more.
      for (let index = 0; index < 5; index++) {
        const next = await iterator.next();
        if (next.done) break;
        seen.push((next.value as { value: string }).value);
      }
      await new Promise((resolve) => setTimeout(resolve, 1_500));
      for (;;) {
        const next = await iterator.next();
        if (next.done) break;
        seen.push((next.value as { value: string }).value);
      }

      assertEquals(emitted, totalFrames);
      assertEquals(seen.length, totalFrames);
      for (let index = 0; index < totalFrames; index++) {
        assertEquals(seen[index], `frame-${index + 1}`);
      }
    } finally {
      await client.connection.close();
      await service.stop();
      assertEquals(await serviceExit, undefined);
    }
  }, runtimeOptions);
});

Deno.test("T12 large bodies stay ordered under the bounded byte window", async () => {
  await withTrellisRuntime(async (runtime) => {
    const totalFrames = 24;
    const padding = "p".repeat(8_192);
    let emitted = 0;
    const identity = await runtime.registerService({
      name: `live-flow-${crypto.randomUUID()}`,
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Provider.participant,
      name: `live-flow-${crypto.randomUUID()}`,
      seed: identity.seed,
    }).orThrow();
    const serviceExit = service.wait().catch((error: unknown) => error);
    await service.handleWatch(async ({ emit }) => {
      for (let frame = 1; frame <= totalFrames; frame++) {
        await emit({ value: `${padding}:${frame}` }).orThrow();
        emitted += 1;
      }
    });

    const client = await runtime.connectClient({
      name: "live-flow-caller",
      contract: participants.Caller.participant,
    });
    try {
      const handle = await client.watch({}).orThrow();
      const seen: string[] = [];
      for await (const value of handle) {
        seen.push((value as { value: string }).value);
      }
      assertEquals(emitted, totalFrames);
      assertEquals(seen.length, totalFrames);
      // Raw ingress and decoded ownership share one byte budget; every frame
      // still arrives exactly once and in order.
      for (let index = 0; index < totalFrames; index++) {
        assertEquals(seen[index], `${padding}:${index + 1}`);
      }
    } finally {
      await client.connection.close();
      await service.stop();
      assertEquals(await serviceExit, undefined);
    }
  }, runtimeOptions);
});
