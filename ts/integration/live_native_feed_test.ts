import type { TrellisTestRuntime } from "@oatscenter/trellis-testkit";
import { TrellisService } from "@oatscenter/trellis/service";
import assert from "node:assert/strict";
import { join } from "node:path";
import { Readable } from "node:stream";
import { TextDecoderStream } from "node:stream/web";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { request as cliRequest } from "../packages/trellis-testkit/trellis/apps/cli/mod.js";
import { spawnTracked } from "./_support/process.ts";
import {
  authorizeRustNativeLogin,
  rustFixtureArgv,
  withTrellisRuntime,
} from "./_support/runtime.ts";

async function completeRustLogin(
  runtime: TrellisTestRuntime,
  argv: string[],
  configDir: string,
  doneMarker: string,
): Promise<void> {
  const child = spawnTracked(argv[0], argv.slice(1), {
    env: {
      ...process.env,
      TRELLIS_URL: runtime.oauthIssuer,
      XDG_CONFIG_HOME: join(runtime.workdir, configDir),
      CARGO_TARGET_DIR: fileURLToPath(new URL("../../target", import.meta.url)),
    },
  });
  child.stderr.pipe(process.stderr);
  const reader = Readable.toWeb(child.stdout)
    .pipeThrough(new TextDecoderStream())
    .getReader();
  let output = "";
  while (!/^NATIVE_LOGIN .+\n/m.test(output)) {
    const chunk = await reader.read();
    assert(!chunk.done, output);
    output += chunk.value;
  }
  await authorizeRustNativeLogin(runtime, output, apps.Caller.request);
  while (!output.includes(doneMarker)) {
    const chunk = await reader.read();
    assert(!chunk.done, output);
    output += chunk.value;
  }
  assert.equal((await child.exited).code, 0, output);
}

test("NX01 rust caller receives Watch frames from rust provider", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "rust",
      contract: participants.OperationProvider.participant,
    });
    const providerProcess = spawnTracked(
      "setsid",
      rustFixtureArgv("trellis-runtime-acceptance"),
      {
        env: {
          ...process.env,
          TRELLIS_URL: runtime.oauthIssuer,
          TRELLIS_IDENTITY_SEED: identity.seed,
          CARGO_TARGET_DIR: fileURLToPath(
            new URL("../../target", import.meta.url),
          ),
        },
      },
    );
    let providerOutput = "";
    const decoder = new TextDecoder();
    const capture = async (stream: Readable) => {
      for await (const chunk of stream) {
        providerOutput = (providerOutput + decoder.decode(chunk)).slice(
          -16_384,
        );
      }
    };
    const drain = Promise.all([
      capture(providerProcess.stdout),
      capture(providerProcess.stderr),
    ]);
    let exited = false;
    const status = providerProcess.exited.then((value) => {
      exited = true;
      return value;
    });
    try {
      const client = await runtime.connectClient({
        name: "nx01-echo",
        contract: apps.Caller.request,
      });
      await runtime.waitFor(
        async () => {
          if (exited) {
            throw new Error(
              `Rust provider exited: ${JSON.stringify(await status)}\n${
                providerOutput.trimEnd() || "<no provider output>"
              }`,
            );
          }
          const result = await client.echo(
            { value: "from TypeScript" },
            {
              timeout: 1000,
            },
          );
          return result.isOk();
        },
        { timeoutMs: 120_000 },
      );

      await completeRustLogin(
        runtime,
        rustFixtureArgv("caller"),
        "rust-caller-config",
        "rust caller complete",
      );
    } finally {
      if (!exited && providerProcess.pid !== undefined)
        process.kill(-providerProcess.pid, "SIGTERM");
      await status;
      await drain;
    }
  });
});

test("NX03 empty finite Watch completes with no frames", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "rust",
      contract: participants.OperationProvider.participant,
    });
    const providerProcess = spawnTracked(
      "setsid",
      rustFixtureArgv("trellis-runtime-acceptance"),
      {
        env: {
          ...process.env,
          TRELLIS_URL: runtime.oauthIssuer,
          TRELLIS_IDENTITY_SEED: identity.seed,
          TRELLIS_FEED_EMPTY: "1",
          CARGO_TARGET_DIR: fileURLToPath(
            new URL("../../target", import.meta.url),
          ),
        },
      },
    );
    providerProcess.stdout.pipe(process.stdout);
    providerProcess.stderr.pipe(process.stderr);
    let exited = false;
    const status = providerProcess.exited.then((value) => {
      exited = true;
      return value;
    });
    try {
      const client = await runtime.connectClient({
        name: "nx03-echo",
        contract: apps.Caller.request,
      });
      await runtime.waitFor(
        async () => {
          if (exited) {
            throw new Error(
              `Rust provider exited: ${JSON.stringify(await status)}`,
            );
          }
          const result = await client.echo(
            { value: "from TypeScript" },
            {
              timeout: 1000,
            },
          );
          return result.isOk();
        },
        { timeoutMs: 120_000 },
      );
      await completeRustLogin(
        runtime,
        rustFixtureArgv("empty_watch"),
        "empty-watch-config",
        "empty watch complete",
      );
    } finally {
      if (!exited && providerProcess.pid !== undefined)
        process.kill(-providerProcess.pid, "SIGTERM");
      await status;
    }
  });
});

test("L2 TypeScript caller receives rust Watch over live open", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "rust",
      contract: participants.OperationProvider.participant,
    });
    const providerProcess = spawnTracked(
      "setsid",
      rustFixtureArgv("trellis-runtime-acceptance"),
      {
        env: {
          ...process.env,
          TRELLIS_URL: runtime.oauthIssuer,
          TRELLIS_IDENTITY_SEED: identity.seed,
          CARGO_TARGET_DIR: fileURLToPath(
            new URL("../../target", import.meta.url),
          ),
        },
      },
    );
    providerProcess.stdout.pipe(process.stdout);
    providerProcess.stderr.pipe(process.stderr);
    let exited = false;
    const status = providerProcess.exited.then((value) => {
      exited = true;
      return value;
    });
    try {
      const client = await runtime.connectClient({
        name: "l2-ts-caller",
        contract: apps.Caller.request,
      });
      await runtime.waitFor(
        async () => {
          if (exited) {
            throw new Error(
              `Rust provider exited: ${JSON.stringify(await status)}`,
            );
          }
          const result = await client.echo(
            { value: "from TypeScript" },
            {
              timeout: 1000,
            },
          );
          return result.isOk();
        },
        { timeoutMs: 120_000 },
      );
      const feed = await client.runtimeWatch({}).orThrow();
      const iterator = feed[Symbol.asyncIterator]();
      const first = await iterator.next();
      assert(
        String(
          (first.value as { value?: string } | undefined)?.value ?? "",
        ).includes("rust-feed"),
        `unexpected first frame: ${JSON.stringify(first)}`,
      );
      await iterator.return?.();
    } finally {
      if (!exited && providerProcess.pid !== undefined)
        process.kill(-providerProcess.pid, "SIGTERM");
      await status;
    }
  });
});

test("L3 TypeScript provider serves Watch over live open", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "ts-feed-provider",
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.oauthIssuer,
      participant: participants.Provider.participant,
      seed: identity.seed,
    }).orThrow();
    await service.handleWatch(async ({ emit, signal }) => {
      await emit({ value: "ts-feed-1" }).orThrow();
      await new Promise<void>((resolve) => {
        if (signal.aborted) {
          resolve();
          return;
        }
        signal.addEventListener("abort", () => resolve(), { once: true });
      });
    });
    const serviceExit = service.wait();
    try {
      const caller = await runtime.connectClient({
        name: "l3-ts-caller",
        contract: apps.Caller.request,
      });
      const feed = await caller.runtimeWatch({}).orThrow();
      const first = await feed[Symbol.asyncIterator]().next();
      assert(
        (first.value as { value?: string } | undefined)?.value === "ts-feed-1",
        `unexpected first frame: ${JSON.stringify(first)}`,
      );
      await feed[Symbol.asyncIterator]().return?.();
      await caller.connection.close();
    } finally {
      await service.stop();
      await serviceExit;
    }
  });
});

test("NX02 rust console client receives Health Watch", async () => {
  await withTrellisRuntime(async (runtime) => {
    const child = spawnTracked(
      process.env.TRELLIS_TEST_HEALTH_WATCH_BIN ??
        fileURLToPath(
          new URL("../../target/debug/examples/health_watch", import.meta.url),
        ),
      [],
      {
        env: { ...process.env, TRELLIS_URL: runtime.oauthIssuer },
      },
    );
    child.stderr.pipe(process.stderr);
    const reader = Readable.toWeb(child.stdout)
      .pipeThrough(new TextDecoderStream())
      .getReader();
    let output = "";
    let code = output.match(/enter code (\S+)\. Approve/);
    while (!code) {
      const chunk = await reader.read();
      assert(!chunk.done, output);
      output += chunk.value;
      code = output.match(/enter code (\S+)\. Approve/);
    }
    await runtime.authorizeNativeUserCode({
      clientId: "trellis.cli",
      userCode: code[1],
      contract: cliRequest,
    });
    while (!output.includes("health watch complete")) {
      const chunk = await reader.read();
      assert(!chunk.done, output);
      output += chunk.value;
    }
    assert.equal((await child.exited).code, 0, output);
  });
});

test("NX05 two TypeScript callers abort one other continues", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "nx05-feed-provider",
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.oauthIssuer,
      participant: participants.Provider.participant,
      seed: identity.seed,
    }).orThrow();
    let cancelled = 0;
    let nextFeed = 0;
    await service.handleWatch(async ({ emit, signal }) => {
      const feed = ++nextFeed;
      let frame = 0;
      while (!signal.aborted) {
        await emit({ value: `feed-${feed}-${++frame}` }).orThrow();
        await new Promise((resolve) => setTimeout(resolve, 25));
      }
      cancelled += 1;
    });
    const serviceExit = service.wait();
    const firstCaller = await runtime.connectClient({
      name: "nx05-caller-a",
      contract: apps.Caller.request,
    });
    const secondCaller = await runtime.connectClient({
      name: "nx05-caller-b",
      contract: apps.Caller.request,
    });
    const firstAbort = new AbortController();
    const secondAbort = new AbortController();
    try {
      const first = await firstCaller
        .runtimeWatch({}, { signal: firstAbort.signal })
        .orThrow();
      const second = await secondCaller
        .runtimeWatch(
          {},
          {
            signal: secondAbort.signal,
          },
        )
        .orThrow();
      const firstIterator = first[Symbol.asyncIterator]();
      const secondIterator = second[Symbol.asyncIterator]();
      assert((await firstIterator.next()).value?.value.startsWith("feed-"));
      assert((await secondIterator.next()).value?.value.startsWith("feed-"));
      firstAbort.abort();
      await firstIterator.return?.();
      await firstCaller.connection.close();
      await runtime.waitFor(() => cancelled === 1);
      assert((await secondIterator.next()).value?.value.startsWith("feed-"));
      assert.deepEqual(cancelled, 1);
    } finally {
      firstAbort.abort();
      secondAbort.abort();
      await firstCaller.connection.close();
      await secondCaller.connection.close();
      await service.stop();
      await serviceExit;
    }
  });
});

test("NX06 source starts once per session", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "nx06-feed-provider",
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.oauthIssuer,
      participant: participants.Provider.participant,
      seed: identity.seed,
    }).orThrow();
    let starts = 0;
    await service.handleWatch(async ({ emit, signal }) => {
      starts += 1;
      await emit({ value: `start-${starts}` }).orThrow();
      await new Promise<void>((resolve) => {
        if (signal.aborted) {
          resolve();
          return;
        }
        signal.addEventListener("abort", () => resolve(), { once: true });
      });
    });
    const serviceExit = service.wait();
    const caller = await runtime.connectClient({
      name: "nx06-caller",
      contract: apps.Caller.request,
    });
    try {
      const first = await caller.runtimeWatch({}).orThrow();
      const second = await caller.runtimeWatch({}).orThrow();
      await first[Symbol.asyncIterator]().next();
      await second[Symbol.asyncIterator]().next();
      assert.deepEqual(starts, 2);
      await first[Symbol.asyncIterator]().return?.();
      await second[Symbol.asyncIterator]().return?.();
    } finally {
      await caller.connection.close();
      await service.stop();
      await serviceExit;
    }
  });
});

test("NX09 reconnect opens a new Watch without reviving the old session", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "nx09-feed-provider",
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.oauthIssuer,
      participant: participants.Provider.participant,
      seed: identity.seed,
    }).orThrow();
    let starts = 0;
    await service.handleWatch(async ({ emit, signal }) => {
      starts += 1;
      await emit({ value: `gen-${starts}` }).orThrow();
      await new Promise<void>((resolve) => {
        if (signal.aborted) {
          resolve();
          return;
        }
        signal.addEventListener("abort", () => resolve(), { once: true });
      });
    });
    const serviceExit = service.wait();
    const firstCaller = await runtime.connectClient({
      name: "nx09-caller-a",
      contract: apps.Caller.request,
    });
    try {
      const first = await firstCaller.runtimeWatch({}).orThrow();
      const firstIterator = first[Symbol.asyncIterator]();
      assert.deepEqual((await firstIterator.next()).value?.value, "gen-1");
      await firstCaller.connection.close();
      const leftover = await Promise.race([
        firstIterator.next().then((item) => item.done === true),
        new Promise<boolean>((resolve) =>
          setTimeout(() => resolve(false), 2_000),
        ),
      ]);
      assert(leftover, "closed connection must end the old iterator");
      const secondCaller = await runtime.connectClient({
        name: "nx09-caller-b",
        contract: apps.Caller.request,
      });
      try {
        const second = await secondCaller.runtimeWatch({}).orThrow();
        const secondIterator = second[Symbol.asyncIterator]();
        assert.deepEqual((await secondIterator.next()).value?.value, "gen-2");
        await secondIterator.return?.();
      } finally {
        await secondCaller.connection.close();
      }
    } finally {
      await firstCaller.connection.close();
      await service.stop();
      await serviceExit;
    }
  });
});

test("NX10 setup timeout releases consumer admission", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "nx10-feed-provider",
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.oauthIssuer,
      participant: participants.Provider.participant,
      seed: identity.seed,
    }).orThrow();
    const serviceExit = service.wait();
    const caller = await runtime.connectClient({
      name: "nx10-caller",
      contract: apps.Caller.request,
    });
    try {
      const missing = await caller.runtimeWatch({});
      assert(missing.isErr(), "watch without a handler must fail setup");
      await service.handleWatch(async ({ emit }) => {
        await emit({ value: "nx10" }).orThrow();
      });
      const feed = await caller.runtimeWatch({}).orThrow();
      assert.deepEqual(
        (await feed[Symbol.asyncIterator]().next()).value?.value,
        "nx10",
      );
      await feed[Symbol.asyncIterator]().return?.();
    } finally {
      await caller.connection.close();
      await service.stop();
      await serviceExit;
    }
  });
});

test("NX08 consumer drop does not throw", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "nx08-feed-provider",
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.oauthIssuer,
      participant: participants.Provider.participant,
      seed: identity.seed,
    }).orThrow();
    await service.handleWatch(async ({ emit, signal }) => {
      await emit({ value: "nx08-1" }).orThrow();
      await new Promise<void>((resolve) => {
        if (signal.aborted) {
          resolve();
          return;
        }
        signal.addEventListener("abort", () => resolve(), { once: true });
      });
    });
    const serviceExit = service.wait();
    const caller = await runtime.connectClient({
      name: "nx08-caller",
      contract: apps.Caller.request,
    });
    try {
      const feed = await caller.runtimeWatch({}).orThrow();
      const iterator = feed[Symbol.asyncIterator]();
      await iterator.next();
      await iterator.return?.();
      await caller.connection.close();
    } finally {
      await service.stop();
      await serviceExit;
    }
  });
});
