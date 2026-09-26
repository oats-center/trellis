// Mixed-language concurrency proof: independent Rust test processes and a
// TypeScript `TrellisTestRuntime` must be alive on the same host at the same
// time, each with its own automatically assigned ports, synchronized by a
// barrier so the overlap is real rather than sequential.
import { assert, assertEquals } from "@std/assert";
import { fromFileUrl, join } from "@std/path";

import { startTrellisRuntime } from "./_support/runtime.ts";

const repoRoot = fromFileUrl(new URL("../../", import.meta.url));
const fixtureManifest = join(
  repoRoot,
  "integration/fixtures/testkit/Cargo.toml",
);

function cargoArgs(extra: string[]): string[] {
  const patch = (crate: string, path: string) => [
    "--config",
    `patch.crates-io.${crate}.path=${
      JSON.stringify(join(repoRoot, "crates", path))
    }`,
  ];
  return [
    "test",
    "--manifest-path",
    fixtureManifest,
    ...patch("trellis-protocol", "protocol"),
    ...patch("trellis-rs", "trellis"),
    ...patch("trellis-testkit", "trellis-test"),
    "--test",
    "live",
    ...extra,
  ];
}

async function waitForFile(path: string, timeoutMs: number): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      await Deno.stat(path);
      return;
    } catch {
      // Not written yet; keep polling.
    }
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  throw new Error(`timed out waiting for ${path}`);
}

Deno.test(
  "mixed Rust processes and a TypeScript runtime overlap on one host",
  async () => {
    const dir = await Deno.makeTempDir({ prefix: "trellis-mixed-" });
    const baseEnv = {
      ...Deno.env.toObject(),
      CARGO_TARGET_DIR: join(repoRoot, "target"),
      TRELLIS_TEST_CLI_BIN: Deno.env.get("TRELLIS_TEST_CLI_BIN") ??
        join(repoRoot, "target/debug/trellis"),
      TRELLIS_TEST_SERVER_BIN: Deno.env.get("TRELLIS_TEST_SERVER_BIN") ??
        join(repoRoot, "target/debug/trellis-server"),
    };
    // The build job exports the prebuilt test binary; only local development
    // compiles it here.
    const prebuiltLive = Deno.env.get("TRELLIS_TESTKIT_LIVE_BIN");
    if (prebuiltLive === undefined) {
      if (Deno.env.get("CI")) {
        throw new Error(
          "TRELLIS_TESTKIT_LIVE_BIN must point at the prebuilt testkit live binary in CI",
        );
      }
      const build = await new Deno.Command("cargo", {
        args: cargoArgs(["--no-run"]),
        env: baseEnv,
        stdout: "inherit",
        stderr: "inherit",
      }).output();
      assertEquals(
        build.success,
        true,
        "failed to build the Rust child test binary",
      );
    }

    const runtime = await startTrellisRuntime();
    const children: Deno.ChildProcess[] = [];
    const releases: string[] = [];
    try {
      // Every Rust child publishes its endpoints only after its runtime is live.
      for (let index = 0; index < 2; index++) {
        const endpoints = join(dir, `endpoints-${index}.txt`);
        const release = join(dir, `release-${index}`);
        releases.push(release);
        const childArgv = prebuiltLive === undefined
          ? [
            "cargo",
            ...cargoArgs([
              "--",
              "--ignored",
              "--exact",
              "runtime_endpoints_child",
            ]),
          ]
          : [prebuiltLive, "--ignored", "--exact", "runtime_endpoints_child"];
        children.push(
          new Deno.Command("setsid", {
            args: childArgv,
            env: {
              ...baseEnv,
              TRELLIS_TEST_CHILD_ENDPOINTS_FILE: endpoints,
              TRELLIS_TEST_CHILD_RELEASE_FILE: release,
            },
            stdout: "inherit",
            stderr: "inherit",
          }).spawn(),
        );
        await waitForFile(endpoints, 240_000);
      }

      // The TypeScript runtime and both Rust children are live simultaneously.
      const endpoints = [runtime.trellisUrl, runtime.natsUrl];
      for (let index = 0; index < 2; index++) {
        const lines =
          (await Deno.readTextFile(join(dir, `endpoints-${index}.txt`)))
            .trim()
            .split("\n");
        endpoints.push(...lines);
      }
      assertEquals(
        new Set(endpoints).size,
        endpoints.length,
        "every runtime endpoint must be distinct",
      );
      assert(runtime.natsUrl.length > 0);

      // Release the children and require clean exits.
      for (const release of releases) {
        await Deno.writeTextFile(release, "go");
      }
      for (const child of children) {
        const status = await child.status;
        assertEquals(status.success, true, "Rust child must exit cleanly");
      }
    } finally {
      for (const release of releases) {
        try {
          await Deno.writeTextFile(release, "go");
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
      await runtime.stop();
      await Deno.remove(dir, { recursive: true }).catch(() => {});
    }
  },
);
