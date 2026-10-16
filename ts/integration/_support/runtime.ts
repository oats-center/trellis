import { TrellisTestRuntime } from "@oatscenter/trellis-testkit";
import type {
  TrellisTestClientParticipant,
  TrellisTestRuntimeStartOptions,
} from "@oatscenter/trellis-testkit";
import { join } from "node:path";
import { z } from "zod";

const DEFAULT_TIMEOUTS = {
  startupMs: 60_000,
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
      ...options.trellis,
      source: options.trellis?.source ?? repoTrellisSource(),
    },
    timeouts: {
      ...DEFAULT_TIMEOUTS,
      ...options.timeouts,
    },
  };
}

/** Resolve the producer-built CLI/server pair without compiling or downloading in execution jobs. */
export function repoTrellisSource(): {
  kind: "path";
  cli: string;
  server: string;
} {
  const { TRELLIS_TEST_SERVER_BIN: server, TRELLIS_TEST_CLI_BIN: cli } = z
    .object({
      TRELLIS_TEST_SERVER_BIN: z.string().min(1),
      TRELLIS_TEST_CLI_BIN: z.string().min(1),
    })
    .parse(process.env);
  return { kind: "path", cli, server };
}

/**
 * Resolves a Rust helper/provider executable under `integration/fixtures/runtime`.
 *
 * The single integration entrypoint (`npm run test:integration`) builds these
 * once and points `TRELLIS_TEST_FIXTURE_BIN_DIR` at them, so tests never compile
 * Rust themselves and local and CI launch the same binaries.
 */
export function rustFixtureArgv(bin: string, extra: string[] = []): string[] {
  const dir = z.string().min(1).parse(process.env.TRELLIS_TEST_FIXTURE_BIN_DIR);
  return [join(dir, bin), ...extra];
}

/** Approves the ordinary Device Code request emitted by a native Rust fixture. */
export async function authorizeRustNativeLogin(
  runtime: TrellisTestRuntime,
  output: string,
  contract: TrellisTestClientParticipant,
): Promise<void> {
  const line = output
    .split("\n")
    .find((line) => line.startsWith("NATIVE_LOGIN "));
  if (!line)
    throw new Error(`Rust fixture did not emit a native login: ${output}`);
  const login = z
    .object({ clientId: z.string().min(1), userCode: z.string().min(1) })
    .parse(JSON.parse(line.slice("NATIVE_LOGIN ".length)));
  await runtime.authorizeNativeUserCode({ ...login, contract });
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
  const value = z
    .string()
    .optional()
    .parse(process.env.TRELLIS_TEST_KEEP_WORKDIR)
    ?.toLowerCase();
  return value === "1" || value === "true" || value === "yes";
}
