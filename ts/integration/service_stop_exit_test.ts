import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { join } from "node:path";
import { test } from "node:test";
import { pathToFileURL } from "node:url";

import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

const fixture = pathToFileURL(
  join(
    import.meta.dirname,
    "../../integration/fixtures/runtime/packages/runtime-trellis/index.js",
  ),
).href;
const service = pathToFileURL(
  join(import.meta.dirname, "../packages/trellis/service/mod.ts"),
).href;

/**
 * A service that stops while it owns an operation, then has nothing left to do.
 * It exits only when nothing it started still holds the process open.
 */
const program = `
import { TrellisService } from ${JSON.stringify(service)};
import { participants } from ${JSON.stringify(fixture)};
const [trellisUrl, seed] = process.argv.slice(1);
const service = await TrellisService.connect({
  trellisUrl,
  participant: participants.Provider.participant,
  seed,
}).orThrow();
await service.handleWork(async ({ op }) => {
  await op.started().orThrow();
  console.log("STARTED");
  await service.stop();
  console.log("STOPPED");
  return await new Promise(() => {});
});
console.log("READY");
`;

test("a service that stops while it owns an operation lets its process exit", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "stop-exit-provider",
      contract: participants.Provider.participant,
    });
    const child = spawn(
      process.execPath,
      [
        "--experimental-transform-types",
        "--no-warnings",
        "--input-type=module",
        "-e",
        program,
        runtime.trellisUrl,
        identity.seed,
      ],
      {
        stdio: ["ignore", "pipe", "inherit"],
        env: { ...process.env, NODE_EXTRA_CA_CERTS: runtime.caCertificate },
      },
    );
    let output = "";
    let stoppedAt: number | undefined;
    child.stdout.on("data", (chunk: Buffer) => {
      output += chunk.toString();
      if (stoppedAt === undefined && output.includes("STOPPED"))
        stoppedAt = Date.now();
    });
    const exited = new Promise<number | null>((resolve) =>
      child.once("exit", (code) => resolve(code)),
    );
    try {
      const caller = await runtime.connectClient({
        name: "stop-exit-caller",
        contract: apps.Caller.request,
      });
      try {
        await runtime.waitFor(() => output.includes("READY"), {
          timeoutMs: 60_000,
        });
        await caller.work({ value: "held" }).start().orThrow();
        await runtime.waitFor(() => stoppedAt, { timeoutMs: 60_000 });
        // Its lease renews every ten seconds while it runs; a stopped service
        // must not keep renewing, or keep its process alive to try.
        const code = await Promise.race([
          exited,
          new Promise<"alive">((resolve) =>
            setTimeout(() => resolve("alive"), 5_000),
          ),
        ]);
        assert.notEqual(
          code,
          "alive",
          "the stopped service's process kept running",
        );
        assert.equal(code, 0, output);
      } finally {
        await caller.connection.close();
      }
    } finally {
      child.kill();
    }
  });
});
