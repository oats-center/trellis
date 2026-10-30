// Mixed-language concurrency proof: independent Rust test processes and a
// TypeScript `TrellisTestRuntime` must be alive on the same host at the same
// time, each with its own automatically assigned ports, synchronized by a
// barrier so the overlap is real rather than sequential.
import assert from "node:assert/strict";
import { mkdtemp, readFile, rm, stat, writeFile } from "node:fs/promises";
import { spawnTracked, type TrackedProcess } from "./_support/process.ts";
import { tmpdir } from "node:os";
import { test } from "node:test";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { startTrellisRuntime } from "./_support/runtime.ts";

const repoRoot = fileURLToPath(new URL("../../", import.meta.url));

async function waitForFile(path: string, timeoutMs: number): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      await stat(path);
      return;
    } catch {
      // Not written yet; keep polling.
    }
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  throw new Error(`timed out waiting for ${path}`);
}

test("mixed Rust processes and a TypeScript runtime overlap on one host", async () => {
  const dir = await mkdtemp(join(tmpdir(), "trellis-mixed-"));
  const prebuiltLive = process.env["TRELLIS_TESTKIT_LIVE_BIN"];
  if (prebuiltLive === undefined) {
    throw new Error(
      "TRELLIS_TESTKIT_LIVE_BIN must point at the prebuilt testkit live binary; run `npm run test:integration`",
    );
  }
  const baseEnv = {
    ...process.env,
    TRELLIS_TEST_CLI_BIN:
      process.env["TRELLIS_TEST_CLI_BIN"] ??
      join(repoRoot, "target/debug/trellis"),
    TRELLIS_TEST_SERVER_BIN:
      process.env["TRELLIS_TEST_SERVER_BIN"] ??
      join(repoRoot, "target/debug/trellis-server"),
  };

  const runtime = await startTrellisRuntime();
  const children: TrackedProcess[] = [];
  const releases: string[] = [];
  try {
    // Every Rust child publishes its endpoints only after its runtime is live.
    for (let index = 0; index < 2; index++) {
      const endpoints = join(dir, `endpoints-${index}.txt`);
      const release = join(dir, `release-${index}`);
      releases.push(release);
      const childArgv = [
        prebuiltLive,
        "--ignored",
        "--exact",
        "runtime_endpoints_child",
      ];
      const child = spawnTracked("setsid", childArgv, {
        env: {
          ...baseEnv,
          TRELLIS_TEST_CHILD_ENDPOINTS_FILE: endpoints,
          TRELLIS_TEST_CHILD_RELEASE_FILE: release,
        },
      });
      child.stdout.pipe(process.stdout);
      child.stderr.pipe(process.stderr);
      children.push(child);
      await waitForFile(endpoints, 240_000);
    }

    // The TypeScript runtime and both Rust children are live simultaneously.
    const endpoints = [runtime.consoleUrl, runtime.natsUrl];
    for (let index = 0; index < 2; index++) {
      const lines = (
        await readFile(join(dir, `endpoints-${index}.txt`), "utf8")
      )
        .trim()
        .split("\n");
      endpoints.push(...lines);
    }
    assert.deepEqual(
      new Set(endpoints).size,
      endpoints.length,
      "every runtime endpoint must be distinct",
    );
    assert(runtime.natsUrl.length > 0);

    // Release the children and require clean exits.
    for (const release of releases) {
      await writeFile(release, "go");
    }
    for (const child of children) {
      const status = await child.exited;
      assert.equal(status.code, 0, "Rust child must exit cleanly");
    }
  } finally {
    for (const release of releases) {
      try {
        await writeFile(release, "go");
      } catch {
        // Already released or removed.
      }
    }
    for (const child of children) {
      try {
        child.kill("SIGKILL");
      } catch {
        // Already exited.
      }
    }
    await Promise.all(children.map((child) => child.exited));
    await runtime.stop();
    await rm(dir, { recursive: true }).catch(() => {});
  }
});
