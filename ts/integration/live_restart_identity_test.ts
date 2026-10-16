/**
 * Real-boundary built-in live provider restart identity.
 *
 * Proves the managed live-provider seed material is stable across a control
 * plane restart (no silent rotation) while the built-in Health Feed continues
 * to serve real frames afterwards.
 */

import assert from "node:assert/strict";
import { globSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";
import { apps } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";

import { withTrellisRuntime } from "./_support/runtime.ts";

/** Reads every managed live-provider seed file keyed by absolute path. */
function seedBytes(workdir: string): Record<string, string> {
  const seeds: Record<string, string> = {};
  for (const entry of globSync("**/live-providers/*.seed", { cwd: workdir })) {
    const path = join(workdir, entry);
    seeds[path] = readFileSync(path, "utf8");
  }
  return seeds;
}

test("BI05 live provider identity is stable across a control-plane restart", async () => {
  await withTrellisRuntime(async (runtime) => {
    const client = await runtime.connectClient({
      name: "bi05-before",
      contract: apps.Observer.request,
    });
    const feed = await client.healthWatch({}).orThrow();
    const first = await feed[Symbol.asyncIterator]().next();
    assert(!first.done, "health feed must serve before restart");
    await feed[Symbol.asyncIterator]().return?.();
    await client.connection.close();

    const before = seedBytes(runtime.workdir);
    assert(
      Object.keys(before).length > 0,
      "managed live provider seeds must exist on disk",
    );

    await runtime.restart();

    assert.deepEqual(
      seedBytes(runtime.workdir),
      before,
      "live provider seeds must not rotate across a restart",
    );

    const restarted = await runtime.connectClient({
      name: "bi05-after",
      contract: apps.Observer.request,
    });
    const feedAfter = await restarted.healthWatch({}).orThrow();
    const frame = await feedAfter[Symbol.asyncIterator]().next();
    assert(!frame.done, "health feed must serve after restart");
    await feedAfter[Symbol.asyncIterator]().return?.();
    await restarted.connection.close();
  });
});
