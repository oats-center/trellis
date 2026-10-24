import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import assert from "node:assert/strict";
import { test } from "node:test";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { createInterface } from "node:readline";
import { env, kill } from "node:process";
import { ulid } from "ulid";
import type { TrellisTestRuntime } from "@oatscenter/trellis-testkit";
import { z } from "zod";

import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";
import { spawnTracked, type ProcessExit } from "./_support/process.ts";

const BUSY_TOTAL = 1026n;
const TWO_HEARTBEATS_MS = 21_000;

type KeyState = {
  through: bigint;
  finish: boolean;
  fail: boolean;
  payloadBytes: number;
  paddingBytes: number;
  starts: number;
  active: number;
  cleanups: number;
  emitted: number;
  waiters: Array<() => void>;
};

type ProviderRuntime = Pick<
  TrellisTestRuntime,
  "oauthIssuer" | "caCertificate" | "registerService"
>;

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/** Start the in-process TypeScript `liveprobe` provider used by V3 pairings. */
async function startTsProvider(
  runtime: ProviderRuntime,
  replica?: { name: string; seed: string },
): Promise<{ stop: () => Promise<void>; exit: Promise<void> }> {
  const identity =
    replica ??
    (await runtime.registerService({
      name: `live-probe-ts-${ulid()}`,
      contract: participants.LiveProbeProvider.participant,
    }));
  const service = await TrellisService.connect({
    trellisUrl: runtime.oauthIssuer,
    participant: participants.LiveProbeProvider.participant,
    name: replica?.name,
    seed: identity.seed,
  }).orThrow();
  const keys = new Map<string, KeyState>();
  const keyOf = (runId: string, streamId: string) => `${runId}/${streamId}`;
  const status = (state: KeyState) => ({
    starts: BigInt(state.starts),
    active: BigInt(state.active),
    cleanups: BigInt(state.cleanups),
    emitted: BigInt(state.emitted),
  });
  const stateOf = (runId: string, streamId: string): KeyState => {
    const key = keyOf(runId, streamId);
    let state = keys.get(key);
    if (state === undefined) {
      state = {
        through: 0n,
        finish: false,
        fail: false,
        payloadBytes: 4,
        paddingBytes: 0,
        starts: 0,
        active: 0,
        cleanups: 0,
        emitted: 0,
        waiters: [],
      };
      keys.set(key, state);
    }
    return state;
  };

  await service.handleInspect(({ input }) => {
    const state = keys.get(keyOf(input.runId, input.streamId));
    return Result.ok(
      state === undefined
        ? { starts: 0n, active: 0n, cleanups: 0n, emitted: 0n }
        : status(state),
    );
  });
  await service.handleRelease(({ input }) => {
    const state = stateOf(input.runId, input.streamId);
    state.through = input.throughIndex;
    state.finish = input.finish;
    state.fail = input.fail;
    if (input.payloadBytes !== undefined) {
      state.payloadBytes = input.payloadBytes;
    }
    if (input.paddingBytes !== undefined) {
      state.paddingBytes = input.paddingBytes;
    }
    for (const wake of state.waiters.splice(0)) wake();
    return Result.ok(status(state));
  });
  await service.handleWatch(async ({ input, emit, signal }) => {
    const state = stateOf(input.runId, input.streamId);
    state.starts += 1;
    state.active += 1;
    const generation = BigInt(state.starts);
    try {
      let next = 1n;
      while (true) {
        if (signal.aborted) return;
        if (next <= state.through) {
          await emit({
            runId: input.runId,
            streamId: input.streamId,
            sourceGeneration: generation,
            index: next,
            payload: new Uint8Array(state.payloadBytes),
            padding: "p".repeat(state.paddingBytes),
          }).orThrow();
          state.emitted += 1;
          next += 1n;
          continue;
        }
        if (state.finish) return;
        if (state.fail) throw new Error("live probe source failed");
        await new Promise<void>((resolve) => {
          if (signal.aborted) {
            resolve();
            return;
          }
          state.waiters.push(resolve);
          signal.addEventListener("abort", () => resolve(), { once: true });
        });
      }
    } finally {
      state.active -= 1;
      state.cleanups += 1;
    }
  });

  const exit = service.wait();
  return {
    stop: () => service.stop(),
    exit: exit.then(() => undefined),
  };
}

/** Run one busy+quiet pairing against whichever provider the caller reaches. */
async function runProbeScenario(
  runtime: {
    waitFor: (
      fn: () => boolean,
      opts?: { timeoutMs?: number },
    ) => Promise<unknown>;
  },
  caller: {
    watch: (input: { runId: string; streamId: string }) => {
      orThrow: () => Promise<
        AsyncIterable<{ index: bigint }> & {
          [Symbol.asyncIterator](): AsyncIterator<{ index: bigint }>;
        }
      >;
    };
    release: (input: {
      runId: string;
      streamId: string;
      throughIndex: bigint;
      finish: boolean;
      fail: boolean;
    }) => { orThrow: () => Promise<unknown> };
    inspect: (input: { runId: string; streamId: string }) => {
      orThrow: () => Promise<{
        starts: bigint;
        active: bigint;
        cleanups: bigint;
        emitted: bigint;
      }>;
    };
  },
): Promise<void> {
  const runId = ulid();
  const busy = await caller.watch({ runId, streamId: "busy" }).orThrow();
  const quiet = await caller.watch({ runId, streamId: "quiet" }).orThrow();
  const busyIndexes: bigint[] = [];
  const quietIndexes: bigint[] = [];
  let quietDone = false;
  const busyTask = (async () => {
    for await (const frame of busy) busyIndexes.push(frame.index);
  })();
  const quietTask = (async () => {
    for await (const frame of quiet) quietIndexes.push(frame.index);
    quietDone = true;
  })();
  try {
    await caller
      .release({
        runId,
        streamId: "busy",
        throughIndex: 1025n,
        finish: false,
        fail: false,
      })
      .orThrow();
    await runtime.waitFor(() => busyIndexes.length >= 1025, {
      timeoutMs: 30_000,
    });

    const midpoint = await caller
      .inspect({ runId, streamId: "busy" })
      .orThrow();
    assert.deepEqual(midpoint.starts, 1n, "one busy source start");
    assert.deepEqual(midpoint.emitted, 1025n, "1025 busy frames emitted");

    await delay(TWO_HEARTBEATS_MS);

    await caller
      .release({
        runId,
        streamId: "quiet",
        throughIndex: 1n,
        finish: true,
        fail: false,
      })
      .orThrow();
    await runtime.waitFor(() => quietIndexes.length >= 1, {
      timeoutMs: 30_000,
    });
    await quietTask;
    assert(quietDone, "quiet source ends after its released frame");
    assert.deepEqual(
      quietIndexes,
      [1n],
      "quiet delivered exactly its released frame",
    );

    await caller
      .release({
        runId,
        streamId: "busy",
        throughIndex: BUSY_TOTAL,
        finish: true,
        fail: false,
      })
      .orThrow();
    await busyTask;
    assert.deepEqual(busyIndexes.length, 1026, "1026 busy frames delivered");
    for (let index = 0; index < busyIndexes.length; index += 1) {
      assert.deepEqual(
        busyIndexes[index],
        BigInt(index + 1),
        "frames stay ordered",
      );
    }

    const busyStatus = await caller
      .inspect({ runId, streamId: "busy" })
      .orThrow();
    assert.deepEqual(busyStatus.starts, 1n, "no busy restart");
    assert.deepEqual(busyStatus.active, 0n, "busy source fully cleaned up");
    assert.deepEqual(busyStatus.cleanups, 1n, "one busy cleanup");
    assert.deepEqual(busyStatus.emitted, 1026n, "exact busy emission count");
    const quietStatus = await caller
      .inspect({ runId, streamId: "quiet" })
      .orThrow();
    assert.deepEqual(quietStatus.starts, 1n, "no quiet restart");
    assert.deepEqual(quietStatus.active, 0n, "quiet source fully cleaned up");
    assert.deepEqual(quietStatus.cleanups, 1n, "one quiet cleanup");
    assert.deepEqual(quietStatus.emitted, 1n, "exact quiet emission count");
  } finally {
    await Promise.race([busyTask, delay(1_000)]);
    await Promise.race([quietTask, delay(1_000)]);
  }
}

test("V3 TypeScript caller drives TypeScript liveprobe provider", async () => {
  await withTrellisRuntime(async (runtime) => {
    const provider = await startTsProvider(runtime);
    try {
      const caller = await runtime.connectClient({
        name: "v3-ts-ts",
        contract: apps.LiveProbeCaller.request,
      });
      try {
        await runProbeScenario(runtime, caller);
      } finally {
        await caller.connection.close();
      }
    } finally {
      await provider.stop();
      await provider.exit;
    }
  });
});

test("V3 TypeScript caller drives Rust liveprobe provider", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "live-probe-rust",
      contract: participants.LiveProbeProvider.participant,
    });
    const process = spawnTracked("setsid", rustFixtureArgv("live_probe"), {
      env: {
        ...env,
        TRELLIS_URL: runtime.oauthIssuer,
        TRELLIS_CA_CERTIFICATE: runtime.caCertificate,
        TRELLIS_IDENTITY_SEED: identity.seed,
        CARGO_TARGET_DIR: fileURLToPath(
          new URL("../../target", import.meta.url),
        ),
      },
    });
    let providerOutput = "";
    const decoder = new TextDecoder();
    const capture = async (stream: AsyncIterable<Uint8Array>) => {
      for await (const chunk of stream) {
        providerOutput = (providerOutput + decoder.decode(chunk)).slice(
          -16_384,
        );
      }
    };
    const drain = Promise.all([
      capture(process.stdout),
      capture(process.stderr),
    ]);
    let exited = false;
    const status = process.exited.then((value) => {
      exited = true;
      return value;
    });
    try {
      const caller = await runtime.connectClient({
        name: "v3-ts-rust",
        contract: apps.LiveProbeCaller.request,
      });
      try {
        await runtime.waitFor(
          async () => {
            if (exited) {
              throw new Error(
                `Rust liveprobe provider exited: ${JSON.stringify(
                  await status,
                )}\n${providerOutput.trimEnd() || "<no provider output>"}`,
              );
            }
            const result = await caller.inspect(
              {
                runId: "readiness",
                streamId: "readiness",
              },
              { timeout: 1_000 },
            );
            return result.isOk();
          },
          { timeoutMs: 120_000 },
        );
        await runProbeScenario(runtime, caller);
      } finally {
        await caller.connection.close();
      }
    } finally {
      if (!exited && process.pid !== undefined) kill(-process.pid, "SIGTERM");
      await status;
      await drain;
    }
  });
});

/** Run the Rust `live_probe_caller` leg and return its bounded JSON summary. */
async function runRustLiveProbeCaller(
  runtime: TrellisTestRuntime,
  configDir: string,
): Promise<Record<string, number>> {
  const child = spawnTracked("setsid", rustFixtureArgv("live_probe_caller"), {
    env: {
      ...process.env,
      TRELLIS_URL: runtime.oauthIssuer,
      TRELLIS_CA_CERTIFICATE: runtime.caCertificate,
      XDG_CONFIG_HOME: join(runtime.workdir, configDir),
      CARGO_TARGET_DIR: fileURLToPath(new URL("../../target", import.meta.url)),
    },
  });
  child.stderr.pipe(process.stderr);
  const loginMarker = "NATIVE_LOGIN ";
  const resultMarker = "LIVE_PROBE_CALLER_RESULT ";
  let output = "";
  let result: Record<string, number> | undefined;
  for await (const line of createInterface({ input: child.stdout })) {
    output += `${line}\n`;
    if (line.startsWith(loginMarker)) {
      const login = z
        .object({
          clientId: z.string(),
          userCode: z.string(),
          verificationUri: z.string().url(),
        })
        .parse(JSON.parse(line.slice(loginMarker.length)));
      assert.equal(new URL(login.verificationUri).origin, runtime.oauthIssuer);
      await runtime.authorizeNativeUserCode({
        clientId: login.clientId,
        userCode: login.userCode,
        contract: apps.LiveProbeCaller.request,
      });
    }
    if (line.startsWith(resultMarker))
      result = z
        .record(z.string(), z.number())
        .parse(JSON.parse(line.slice(resultMarker.length)));
  }
  const status = await child.exited;
  assert.equal(status.code, 0, `Rust liveprobe caller failed: ${output}`);
  assert(result, `Rust liveprobe caller produced no result: ${output}`);
  return result;
}

function assertProbeSummary(summary: Record<string, number>): void {
  assert.deepEqual(summary.busyFrames, 1026, "1026 busy frames delivered");
  assert.deepEqual(summary.quietFrames, 1, "one quiet frame delivered");
  assert.deepEqual(summary.busyStarts, 1, "one busy source start");
  assert.deepEqual(summary.busyCleanups, 1, "one busy cleanup");
  assert.deepEqual(summary.busyActive, 0, "busy source fully cleaned up");
  assert.deepEqual(summary.busyEmitted, 1026, "exact busy emission count");
  assert.deepEqual(summary.quietStarts, 1, "one quiet source start");
  assert.deepEqual(summary.quietCleanups, 1, "one quiet cleanup");
  assert.deepEqual(summary.quietActive, 0, "quiet source fully cleaned up");
  assert.deepEqual(summary.quietEmitted, 1, "exact quiet emission count");
}

/** Start the Rust `live_probe` provider process. */
function startRustProvider(runtime: ProviderRuntime): {
  status: Promise<ProcessExit>;
  kill: () => void;
} {
  let pid: number | undefined;
  const statusPromise = runtime
    .registerService({
      name: "live-probe-rust",
      contract: participants.LiveProbeProvider.participant,
    })
    .then((identity) => {
      const child = spawnTracked("setsid", rustFixtureArgv("live_probe"), {
        env: {
          ...env,
          TRELLIS_URL: runtime.oauthIssuer,
          TRELLIS_CA_CERTIFICATE: runtime.caCertificate,
          TRELLIS_IDENTITY_SEED: identity.seed,
          CARGO_TARGET_DIR: fileURLToPath(
            new URL("../../target", import.meta.url),
          ),
        },
      });
      child.stdout.pipe(process.stdout);
      child.stderr.pipe(process.stderr);
      pid = child.pid;
      return child.exited;
    });
  return {
    status: statusPromise,
    kill: () => {
      if (pid !== undefined) process.kill(-pid, "SIGTERM");
    },
  };
}

test("V3 Rust caller drives TypeScript liveprobe provider", async () => {
  await withTrellisRuntime(async (runtime) => {
    const provider = await startTsProvider(runtime);
    const installer = await runtime.connectClient({
      name: "v3-rust-ts-installer",
      contract: apps.LiveProbeCaller.request,
    });
    try {
      await runtime.waitFor(
        async () => {
          const result = await installer.inspect(
            { runId: "readiness", streamId: "readiness" },
            { timeout: 1_000 },
          );
          return result.isOk();
        },
        { timeoutMs: 120_000 },
      );
      assertProbeSummary(
        await runRustLiveProbeCaller(runtime, "v3-rust-ts-caller"),
      );
    } finally {
      await installer.connection.close();
      await provider.stop();
      await provider.exit;
    }
  });
});

test("V3 Rust caller drives Rust liveprobe provider", async () => {
  await withTrellisRuntime(async (runtime) => {
    const provider = startRustProvider(runtime);
    const installer = await runtime.connectClient({
      name: "v3-rust-rust-installer",
      contract: apps.LiveProbeCaller.request,
    });
    try {
      await runtime.waitFor(
        async () => {
          const result = await installer.inspect(
            { runId: "readiness", streamId: "readiness" },
            { timeout: 1_000 },
          );
          return result.isOk();
        },
        { timeoutMs: 120_000 },
      );
      assertProbeSummary(
        await runRustLiveProbeCaller(runtime, "v3-rust-rust-caller"),
      );
    } finally {
      await installer.connection.close();
      provider.kill();
      await provider.status;
    }
  });
});

test("T18 one failing live source leaves another session and an RPC functional", async () => {
  await withTrellisRuntime(async (runtime) => {
    const provider = await startTsProvider(runtime);
    const caller = await runtime.connectClient({
      name: `t18-caller-${ulid()}`,
      contract: apps.LiveProbeCaller.request,
    });
    const runId = `t18-${ulid()}`;
    try {
      const failing = await caller
        .watch({ runId, streamId: "failing" })
        .orThrow();
      const survivor = await caller
        .watch({ runId, streamId: "survivor" })
        .orThrow();
      const failingPump = (async () => {
        for await (const _frame of failing) {
          /* drain to activation */
        }
      })().catch(() => undefined);
      const survivorFrames: bigint[] = [];
      const survivorTask = (async () => {
        for await (const frame of survivor) survivorFrames.push(frame.index);
      })();
      await runtime.waitFor(
        async () =>
          (await caller.inspect({ runId, streamId: "failing" })).orThrow()
            .active === 1n &&
          (await caller.inspect({ runId, streamId: "survivor" })).orThrow()
            .active === 1n,
        { timeoutMs: 30_000 },
      );

      // Fail one source and release one frame on the other.
      await caller
        .release({
          runId,
          streamId: "failing",
          throughIndex: 1n,
          finish: false,
          fail: true,
        })
        .orThrow();
      await caller
        .release({
          runId,
          streamId: "survivor",
          throughIndex: 1n,
          finish: false,
          fail: false,
        })
        .orThrow();

      await runtime.waitFor(() => survivorFrames.length >= 1, {
        timeoutMs: 30_000,
      });
      const failedEnd = await failing.closed;
      await failingPump;
      assert.deepEqual(
        failedEnd.reason,
        "source_error",
        "the failing source ends the session as a source error",
      );
      assert.deepEqual(survivorFrames, [1n], "the surviving session delivers");
      // An unrelated finite RPC on the same provider stays functional.
      const survivorStatus = await caller
        .inspect({
          runId,
          streamId: "survivor",
        })
        .orThrow();
      assert.deepEqual(
        survivorStatus.active,
        1n,
        "the survivor source is active",
      );
      assert.deepEqual(
        survivorStatus.emitted,
        1n,
        "exactly its released frame",
      );

      await survivor.close().orThrow();
      await survivorTask;
      await runtime.waitFor(
        async () =>
          (await caller.inspect({ runId, streamId: "survivor" })).orThrow()
            .active === 0n,
        { timeoutMs: 30_000 },
      );
    } finally {
      await caller.connection.close();
      await provider.stop();
      await provider.exit;
    }
  });
});

test("T17 owner-directed live controls route to the accepting replica", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: `t17-replica-${ulid()}`,
      contract: participants.LiveProbeProvider.participant,
    });
    // Two replicas of one selected deployment share the provider route.
    const replicaA = await startTsProvider(runtime, {
      name: `t17-a-${ulid()}`,
      seed: identity.seed,
    });
    const replicaB = await startTsProvider(runtime, {
      name: `t17-b-${ulid()}`,
      seed: identity.seed,
    });
    const caller = await runtime.connectClient({
      name: `t17-caller-${ulid()}`,
      contract: apps.LiveProbeCaller.request,
    });
    const runId = `t17-${ulid()}`;
    try {
      // The opening is accepted by exactly one replica; the owner-directed
      // close control must reach that same replica for the session to end
      // normally instead of stalling until the liveness bound.
      const feed = await caller.watch({ runId, streamId: "s" }).orThrow();
      const pump = (async () => {
        for await (const _frame of feed) {
          /* drain to activation */
        }
      })().catch(() => undefined);
      await feed.close().orThrow();
      const end = await feed.closed;
      assert.deepEqual(
        end.reason,
        "cancelled",
        "owner-directed close ends the session",
      );
      await pump;

      // The deployment still serves a fresh session afterwards.
      const second = await caller
        .watch({ runId: `${runId}-2`, streamId: "s" })
        .orThrow();
      const secondPump = (async () => {
        for await (const _frame of second) {
          /* drain to activation */
        }
      })().catch(() => undefined);
      await second.close().orThrow();
      const secondEnd = await second.closed;
      assert.deepEqual(
        secondEnd.reason,
        "cancelled",
        "the deployment still serves",
      );
      await secondPump;
    } finally {
      await caller.connection.close();
      await replicaA.stop();
      await replicaB.stop();
    }
  });
});

test("T18 one failing live source leaves another session and an RPC functional", async () => {
  await withTrellisRuntime(async (runtime) => {
    const provider = await startTsProvider(runtime);
    const caller = await runtime.connectClient({
      name: `t18-caller-${ulid()}`,
      contract: apps.LiveProbeCaller.request,
    });
    const runId = `t18-${ulid()}`;
    try {
      const failing = await caller
        .watch({ runId, streamId: "failing" })
        .orThrow();
      const survivor = await caller
        .watch({ runId, streamId: "survivor" })
        .orThrow();
      const failingPump = (async () => {
        for await (const _frame of failing) {
          /* drain to activation */
        }
      })().catch(() => undefined);
      const survivorFrames: bigint[] = [];
      const survivorTask = (async () => {
        for await (const frame of survivor) survivorFrames.push(frame.index);
      })();
      await runtime.waitFor(
        async () =>
          (await caller.inspect({ runId, streamId: "failing" })).orThrow()
            .active === 1n &&
          (await caller.inspect({ runId, streamId: "survivor" })).orThrow()
            .active === 1n,
        { timeoutMs: 30_000 },
      );

      // Fail one source and release one frame on the other.
      await caller
        .release({
          runId,
          streamId: "failing",
          throughIndex: 1n,
          finish: false,
          fail: true,
        })
        .orThrow();
      await caller
        .release({
          runId,
          streamId: "survivor",
          throughIndex: 1n,
          finish: false,
          fail: false,
        })
        .orThrow();

      await runtime.waitFor(() => survivorFrames.length >= 1, {
        timeoutMs: 30_000,
      });
      const failedEnd = await failing.closed;
      await failingPump;
      assert.deepEqual(
        failedEnd.reason,
        "source_error",
        "the failing source ends the session as a source error",
      );
      assert.deepEqual(survivorFrames, [1n], "the surviving session delivers");
      // An unrelated finite RPC on the same provider stays functional.
      const survivorStatus = await caller
        .inspect({
          runId,
          streamId: "survivor",
        })
        .orThrow();
      assert.deepEqual(
        survivorStatus.active,
        1n,
        "the survivor source is active",
      );
      assert.deepEqual(
        survivorStatus.emitted,
        1n,
        "exactly its released frame",
      );

      await survivor.close().orThrow();
      await survivorTask;
      await runtime.waitFor(
        async () =>
          (await caller.inspect({ runId, streamId: "survivor" })).orThrow()
            .active === 0n,
        { timeoutMs: 30_000 },
      );
    } finally {
      await caller.connection.close();
      await provider.stop();
      await provider.exit;
    }
  });
});
