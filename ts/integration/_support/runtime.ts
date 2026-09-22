import { TrellisTestRuntime } from "@qlever-llc/trellis-test";
import type { TrellisTestRuntimeStartOptions } from "@qlever-llc/trellis-test";
import { fromFileUrl } from "@std/path";

const repoJsRoot = fromFileUrl(new URL("../../", import.meta.url));

const DEFAULT_TIMEOUTS = {
  startupMs: 60_000,
  reconciliationMs: 60_000,
  waitForMs: 10_000,
  shutdownMs: 10_000,
};

/** Returns the Trellis repo default runtime options for TypeScript integration tests. */
export function trellisRepoRuntimeOptions(
  options: Partial<TrellisTestRuntimeStartOptions> = {},
): TrellisTestRuntimeStartOptions {
  return {
    ...options,
    keepWorkdir: options.keepWorkdir ?? keepWorkdirFromEnv(),
    trellis: {
      command: options.trellis?.command ?? repoTrellisCommand(),
    },
    timeouts: {
      ...DEFAULT_TIMEOUTS,
      ...options.timeouts,
    },
  };
}

function repoTrellisCommand() {
  const server = Deno.env.get("TRELLIS_TEST_SERVER_BIN");
  if (server !== undefined) {
    return { cmd: server, args: ["--config", "{config}", "all"] };
  }
  return {
    cmd: "cargo",
    args: [
      "run",
      "--manifest-path",
      "../rust/Cargo.toml",
      "-p",
      "trellis-server",
      "--",
      "--config",
      "{config}",
      "all",
    ],
    cwd: repoJsRoot,
  };
}

/** Starts the repo-local Trellis runtime for TypeScript integration tests. */
export async function startTrellisRuntime(
  options: Partial<TrellisTestRuntimeStartOptions> = {},
): Promise<TrellisTestRuntime> {
  return await TrellisTestRuntime.start(trellisRepoRuntimeOptions(options));
}

/** Runs an integration test body with deterministic Trellis runtime cleanup. */
export async function withTrellisRuntime<T>(
  fn: (runtime: TrellisTestRuntime) => Promise<T>,
  options: Partial<TrellisTestRuntimeStartOptions> = {},
): Promise<T> {
  const runtime = await startTrellisRuntime(options);
  try {
    return await fn(runtime);
  } catch (cause) {
    throw new Error(
      `${
        cause instanceof Error ? cause.message : String(cause)
      }\n${runtime.controlPlaneOutput()}`,
      { cause },
    );
  } finally {
    await runtime.stop();
  }
}

function keepWorkdirFromEnv(): boolean {
  const value = Deno.env.get("TRELLIS_TEST_KEEP_WORKDIR")?.toLowerCase();
  return value === "1" || value === "true" || value === "yes";
}
