import { TrellisTestRuntime } from "@oatscenter/trellis-testkit";
import type { TrellisTestRuntimeStartOptions } from "@oatscenter/trellis-testkit";
import { join } from "@std/path";

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
  if (server === undefined) {
    throw new Error(
      "TRELLIS_TEST_SERVER_BIN must point at the prebuilt trellis-server; run `deno task test:integration`",
    );
  }
  return { cmd: server, args: ["--config", "{config}", "all"] };
}

/**
 * Resolves a Rust helper/provider executable under `integration/fixtures/runtime`.
 *
 * The single integration entrypoint (`deno task test:integration`) builds these
 * once and points `TRELLIS_TEST_FIXTURE_BIN_DIR` at them, so tests never compile
 * Rust themselves and local and CI launch the same binaries.
 */
export function rustFixtureArgv(bin: string, extra: string[] = []): string[] {
  const dir = Deno.env.get("TRELLIS_TEST_FIXTURE_BIN_DIR");
  if (dir === undefined) {
    throw new Error(
      `TRELLIS_TEST_FIXTURE_BIN_DIR must point at the prebuilt fixtures (missing ${bin}); run \`deno task test:integration\``,
    );
  }
  return [join(dir, bin), ...extra];
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
