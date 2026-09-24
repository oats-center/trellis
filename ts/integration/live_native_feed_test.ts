import { TrellisService } from "@oatscenter/trellis/service";
import { assert, assertEquals } from "@std/assert";
import { fromFileUrl, join } from "@std/path";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";

async function completeRustLogin(
  runtime: {
    trellisUrl: string;
    workdir: string;
    completeClientAuth: (opts: {
      loginUrl: string;
      sessionKey: string;
      mode: "session_key";
    }) => Promise<unknown>;
  },
  argv: string[],
  configDir: string,
  loginMarker: string,
  doneMarker: string,
): Promise<void> {
  const child = new Deno.Command(argv[0], {
    args: argv.slice(1),
    env: {
      TRELLIS_URL: runtime.trellisUrl,
      XDG_CONFIG_HOME: join(runtime.workdir, configDir),
      CARGO_TARGET_DIR: fromFileUrl(
        new URL("../../target", import.meta.url),
      ),
    },
    stdout: "piped",
    stderr: "inherit",
  }).spawn();
  const reader = child.stdout.pipeThrough(new TextDecoderStream()).getReader();
  let output = "";
  while (!output.includes(loginMarker)) {
    const chunk = await reader.read();
    assert(!chunk.done, output);
    output += chunk.value;
  }
  const loginUrl = output.split(loginMarker)[1]?.trim().split(/\s/)[0];
  assert(loginUrl, output);
  await runtime.completeClientAuth({
    loginUrl,
    sessionKey: "completed-by-rust",
    mode: "session_key",
  });
  while (!output.includes(doneMarker)) {
    const chunk = await reader.read();
    assert(!chunk.done, output);
    output += chunk.value;
  }
  assert((await child.status).success, output);
}

Deno.test("NX01 rust caller receives Watch frames from rust provider", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "rust",
      contract: participants.OperationProvider.participant,
    });
    const process = new Deno.Command("setsid", {
      args: rustFixtureArgv("trellis-runtime-acceptance"),
      env: {
        TRELLIS_URL: runtime.trellisUrl,
        TRELLIS_IDENTITY_SEED: identity.seed,
        CARGO_TARGET_DIR: fromFileUrl(
          new URL("../../target", import.meta.url),
        ),
      },
      stdout: "inherit",
      stderr: "inherit",
    }).spawn();
    let exited = false;
    const status = process.status.then((value) => {
      exited = true;
      return value;
    });
    try {
      const client = await runtime.connectClient({
        name: "nx01-echo",
        contract: participants.Caller.participant,
      });
      await runtime.waitFor(async () => {
        if (exited) {
          throw new Error(
            `Rust provider exited: ${JSON.stringify(await status)}`,
          );
        }
        const result = await client.echo({ value: "from TypeScript" }, {
          timeout: 1000,
        });
        return result.isOk();
      }, { timeoutMs: 120_000 });

      await completeRustLogin(
        runtime,
        rustFixtureArgv("caller"),
        "rust-caller-config",
        "rust login ",
        "rust caller complete",
      );
    } finally {
      if (!exited) Deno.kill(-process.pid, "SIGTERM");
      await status;
    }
  });
});

Deno.test("NX03 empty finite Watch completes with no frames", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "rust",
      contract: participants.OperationProvider.participant,
    });
    const process = new Deno.Command("setsid", {
      args: rustFixtureArgv("trellis-runtime-acceptance"),
      env: {
        TRELLIS_URL: runtime.trellisUrl,
        TRELLIS_IDENTITY_SEED: identity.seed,
        TRELLIS_FEED_EMPTY: "1",
        CARGO_TARGET_DIR: fromFileUrl(
          new URL("../../target", import.meta.url),
        ),
      },
      stdout: "inherit",
      stderr: "inherit",
    }).spawn();
    let exited = false;
    const status = process.status.then((value) => {
      exited = true;
      return value;
    });
    try {
      const client = await runtime.connectClient({
        name: "nx03-echo",
        contract: participants.Caller.participant,
      });
      await runtime.waitFor(async () => {
        if (exited) {
          throw new Error(
            `Rust provider exited: ${JSON.stringify(await status)}`,
          );
        }
        const result = await client.echo({ value: "from TypeScript" }, {
          timeout: 1000,
        });
        return result.isOk();
      }, { timeoutMs: 120_000 });
      await completeRustLogin(
        runtime,
        rustFixtureArgv("empty_watch"),
        "empty-watch-config",
        "empty login ",
        "empty watch complete",
      );
    } finally {
      if (!exited) Deno.kill(-process.pid, "SIGTERM");
      await status;
    }
  });
});

Deno.test("L2 TypeScript caller receives rust Watch over live open", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "rust",
      contract: participants.OperationProvider.participant,
    });
    const process = new Deno.Command("setsid", {
      args: rustFixtureArgv("trellis-runtime-acceptance"),
      env: {
        TRELLIS_URL: runtime.trellisUrl,
        TRELLIS_IDENTITY_SEED: identity.seed,
        CARGO_TARGET_DIR: fromFileUrl(
          new URL("../../target", import.meta.url),
        ),
      },
      stdout: "inherit",
      stderr: "inherit",
    }).spawn();
    let exited = false;
    const status = process.status.then((value) => {
      exited = true;
      return value;
    });
    try {
      const client = await runtime.connectClient({
        name: "l2-ts-caller",
        contract: participants.Caller.participant,
      });
      await runtime.waitFor(async () => {
        if (exited) {
          throw new Error(
            `Rust provider exited: ${JSON.stringify(await status)}`,
          );
        }
        const result = await client.echo({ value: "from TypeScript" }, {
          timeout: 1000,
        });
        return result.isOk();
      }, { timeoutMs: 120_000 });
      const feed = await client.watch({}).orThrow();
      const iterator = feed[Symbol.asyncIterator]();
      const first = await iterator.next();
      assert(
        String((first.value as { value?: string } | undefined)?.value ?? "")
          .includes("rust-feed"),
        `unexpected first frame: ${JSON.stringify(first)}`,
      );
      await iterator.return?.();
    } finally {
      if (!exited) Deno.kill(-process.pid, "SIGTERM");
      await status;
    }
  });
});

Deno.test("L3 TypeScript provider serves Watch over live open", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "ts-feed-provider",
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
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
        contract: participants.Caller.participant,
      });
      const feed = await caller.watch({}).orThrow();
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

Deno.test("NX02 rust console client receives Health Watch", async () => {
  await withTrellisRuntime(async (runtime) => {
    await completeRustLogin(
      runtime,
      [
        "run",
        "--manifest-path",
        fromFileUrl(new URL("../../Cargo.toml", import.meta.url)),
        "-p",
        "trellis-runtime",
        "--example",
        "health_watch",
      ],
      "health-caller-config",
      "health login ",
      "health watch complete",
    );
  });
});

Deno.test("NX05 two TypeScript callers abort one other continues", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "nx05-feed-provider",
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
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
      contract: participants.Caller.participant,
    });
    const secondCaller = await runtime.connectClient({
      name: "nx05-caller-b",
      contract: participants.Caller.participant,
    });
    const firstAbort = new AbortController();
    const secondAbort = new AbortController();
    try {
      const first = await firstCaller.watch({}, { signal: firstAbort.signal })
        .orThrow();
      const second = await secondCaller.watch({}, {
        signal: secondAbort.signal,
      })
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
      assertEquals(cancelled, 1);
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

Deno.test("NX06 source starts once per session", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "nx06-feed-provider",
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
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
      contract: participants.Caller.participant,
    });
    try {
      const first = await caller.watch({}).orThrow();
      const second = await caller.watch({}).orThrow();
      await first[Symbol.asyncIterator]().next();
      await second[Symbol.asyncIterator]().next();
      assertEquals(starts, 2);
      await first[Symbol.asyncIterator]().return?.();
      await second[Symbol.asyncIterator]().return?.();
    } finally {
      await caller.connection.close();
      await service.stop();
      await serviceExit;
    }
  });
});

Deno.test("NX09 reconnect opens a new Watch without reviving the old session", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "nx09-feed-provider",
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
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
      contract: participants.Caller.participant,
    });
    try {
      const first = await firstCaller.watch({}).orThrow();
      const firstIterator = first[Symbol.asyncIterator]();
      assertEquals((await firstIterator.next()).value?.value, "gen-1");
      await firstCaller.connection.close();
      const leftover = await Promise.race([
        firstIterator.next().then((item) => item.done === true),
        new Promise<boolean>((resolve) =>
          setTimeout(() => resolve(false), 2_000)
        ),
      ]);
      assert(leftover, "closed connection must end the old iterator");
      const secondCaller = await runtime.connectClient({
        name: "nx09-caller-b",
        contract: participants.Caller.participant,
      });
      try {
        const second = await secondCaller.watch({}).orThrow();
        const secondIterator = second[Symbol.asyncIterator]();
        assertEquals((await secondIterator.next()).value?.value, "gen-2");
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

Deno.test("NX10 setup timeout releases consumer admission", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "nx10-feed-provider",
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Provider.participant,
      seed: identity.seed,
    }).orThrow();
    const serviceExit = service.wait();
    const caller = await runtime.connectClient({
      name: "nx10-caller",
      contract: participants.Caller.participant,
    });
    try {
      const missing = await caller.watch({});
      assert(missing.isErr(), "watch without a handler must fail setup");
      await service.handleWatch(async ({ emit }) => {
        await emit({ value: "nx10" }).orThrow();
      });
      const feed = await caller.watch({}).orThrow();
      assertEquals(
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

Deno.test("NX08 consumer drop does not throw", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "nx08-feed-provider",
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
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
      contract: participants.Caller.participant,
    });
    try {
      const feed = await caller.watch({}).orThrow();
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
