/** Completed flushes leave nothing behind on a long-lived connection. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { text } from "node:stream/consumers";
import { getHeapSnapshot } from "node:v8";
import { connectNativeSession } from "@oatscenter/trellis/native";
import { apps } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

/**
 * Count the promise reactions alive in this process. A heap snapshot collects
 * garbage first, so only reactions still reachable from a pending promise,
 * such as one attached to a connection's close promise, are counted.
 */
async function livePromiseReactions(): Promise<number> {
  const snapshot = JSON.parse(await text(getHeapSnapshot())) as {
    snapshot: { meta: { node_fields: string[] } };
    nodes: number[];
    strings: string[];
  };
  const fields = snapshot.snapshot.meta.node_fields.length;
  const name = snapshot.snapshot.meta.node_fields.indexOf("name");
  let count = 0;
  for (let index = 0; index < snapshot.nodes.length; index += fields)
    if (
      snapshot.strings[snapshot.nodes[index + name]] ===
      "system / PromiseReaction"
    )
      count++;
  return count;
}

test(
  "thousands of completed flushes on one connection do not grow its retained close reactions",
  { timeout: 90_000 },
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const login = await runtime.registerClient({
        name: "flush-retention",
        contract: apps.DirectoryCaller.request,
      });
      const session = await connectNativeSession(login).orThrow();
      try {
        const nc = session.manager.currentNats();
        for (let flush = 0; flush < 100; flush++) await nc.flush();
        const before = await livePromiseReactions();
        const flushes = 3_000;
        for (let flush = 0; flush < flushes; flush++) await nc.flush();
        const after = await livePromiseReactions();
        // Retaining even one reaction per flush would add 3,000.
        assert(
          after - before < flushes / 10,
          `live promise reactions grew from ${before} to ${after} over ${flushes} flushes`,
        );
      } finally {
        await session.manager.close();
      }
    });
  },
);
