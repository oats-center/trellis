/**
 * An application handler's failure stays the failure of that one call. A Live
 * caller is told only the reference of an unexpected failure, and the text
 * goes to one log line under it; an ephemeral event listener goes on after a
 * handler fails or panics. Each case reads the real log of the provider.
 */
import assert from "node:assert/strict";
import { test } from "node:test";

import { Result, UnexpectedError } from "@oatscenter/result";
import { TrellisService } from "@oatscenter/trellis/service";

import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import {
  apps as v2Apps,
  participants as v2Participants,
} from "../../integration/fixtures/runtime/v2/packages/runtime-trellis/index.js";
import type { LoggerLike } from "../packages/trellis/globals.ts";
import { spawnTracked } from "./_support/process.ts";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";

const REFERENCE =
  /^An unexpected error has occurred\. Reference: ([0-9A-Z]{26})$/;

/** How a Live session ended, once its caller has read it to the end. */
async function endOf(
  view: AsyncIterable<unknown> & {
    closed: Promise<{ reason: string; error?: { message: string } }>;
  },
) {
  try {
    for await (const _ of view) {
      // Drain the session.
    }
  } catch {
    // The failure is read from `closed`.
  }
  return await view.closed;
}

/** The reference a failed Live session ended with. */
function referenceOf(end: {
  reason: string;
  error?: { message: string };
}): string {
  assert.equal(end.reason, "source_error", JSON.stringify(end));
  const match = REFERENCE.exec(end.error?.message ?? "");
  assert(match, `the caller sees only a reference: ${end.error?.message}`);
  return match[1]!;
}

test("a TypeScript Live handler's failure reaches its caller only as a reference", async () => {
  await withTrellisRuntime(async (runtime) => {
    const lines: Record<string, unknown>[] = [];
    const record = (...args: unknown[]) => {
      if (typeof args[0] === "object" && args[0] !== null) {
        lines.push(args[0] as Record<string, unknown>);
      }
    };
    const log: LoggerLike = {
      child: () => log,
      trace: () => {},
      debug: () => {},
      info: record,
      warn: record,
      error: record,
    };
    const identity = await runtime.registerService({
      name: "live-failures-ts",
      contract: participants.LiveV1Provider.participant,
    });
    const provider = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.LiveV1Provider.participant,
      seed: identity.seed,
      runtime: { log },
    }).orThrow();
    try {
      await provider.handleWatch(async ({ input, emit }) => {
        if (input.value === "fail") {
          throw new Error("fixture live failure: the store is unreachable");
        }
        if (input.value === "result") {
          return Result.err(
            new UnexpectedError({ cause: new Error("fixture live result") }),
          );
        }
        await emit({ value: `v1:${input.value}` }).orThrow();
        return Result.ok(undefined);
      });
      const caller = await runtime.connectClient({
        name: "live-failures-ts-caller",
        contract: apps.LiveV1Caller.request,
      });
      for (const [value, text] of [
        ["fail", "fixture live failure: the store is unreachable"],
        ["result", "fixture live result"],
      ] as const) {
        const view = await caller.watch({ value }).orThrow();
        const reference = referenceOf(await endOf(view));
        const logged = lines.filter((line) => line.error_id === reference);
        assert.equal(logged.length, 1, JSON.stringify(lines));
        assert(
          JSON.stringify(logged[0]).includes(text),
          JSON.stringify(logged),
        );
      }
      // The provider goes on serving.
      const view = await caller.watch({ value: "after" }).orThrow();
      const received = [];
      for await (const event of view) received.push(event);
      assert.deepEqual(received, [{ value: "v1:after" }]);
    } finally {
      await provider.stop();
    }
  });
});

test("a Rust Live handler's error or panic reaches its caller only as a reference", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "live-failures-rust",
      contract: v2Participants.LiveV2Provider.participant,
    });
    const argv = rustFixtureArgv("live_application_versions");
    const child = spawnTracked(argv[0]!, argv.slice(1), {
      env: {
        ...process.env,
        TRELLIS_URL: runtime.trellisUrl,
        TRELLIS_CA_CERTIFICATE: runtime.caCertificatePath,
        TRELLIS_IDENTITY_SEED: identity.seed,
        RUST_LOG: "error",
        NO_COLOR: "1",
      },
    });
    let output = "";
    child.stdout.on("data", (chunk: Buffer) => (output += chunk.toString()));
    child.stderr.on("data", (chunk: Buffer) => (output += chunk.toString()));
    try {
      const caller = await runtime.connectClient({
        name: "live-failures-rust-caller",
        contract: v2Apps.LiveV2Caller.request,
      });
      await runtime.waitFor(
        async () =>
          (await caller.echo({ value: "ready" }, { timeout: 1_000 })).isOk(),
        { timeoutMs: 60_000 },
      );
      for (const [value, text] of [
        ["fail", "fixture live failure: the store is unreachable"],
        ["panic", "fixture live panic"],
        ["panic-open", "fixture live open panic"],
      ] as const) {
        const view = await caller.watch({ value }).orThrow();
        const reference = referenceOf(await endOf(view));
        await runtime.waitFor(() => output.includes(reference));
        const logged = output
          .split("\n")
          .filter((line) => line.includes(reference));
        assert.equal(logged.length, 1, output);
        assert.match(logged[0]!, / ERROR /);
        assert(logged[0]!.includes(text), logged[0]);
      }
      const view = await caller.watch({ value: "document" }).orThrow();
      const received = [];
      for await (const event of view) received.push(event);
      assert.deepEqual(received, [{ value: "v2:document" }]);
    } finally {
      child.stdin.end();
      await child.exited;
    }
  });
});

test("a Rust ephemeral event listener goes on after its handler fails or panics", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "ephemeral-failures-rust",
      contract: participants.EventSubscriberService.participant,
    });
    const argv = rustFixtureArgv("events");
    const child = spawnTracked(argv[0]!, argv.slice(1), {
      env: {
        ...process.env,
        TRELLIS_URL: runtime.trellisUrl,
        TRELLIS_CA_CERTIFICATE: runtime.caCertificatePath,
        TRELLIS_IDENTITY_SEED: identity.seed,
        EPHEMERAL_FAILURES: "true",
        RUST_LOG: "warn",
        NO_COLOR: "1",
      },
    });
    let output = "";
    child.stdout.on("data", (chunk: Buffer) => (output += chunk.toString()));
    child.stderr.on("data", (chunk: Buffer) => (output += chunk.toString()));
    try {
      await runtime.waitFor(() => output.includes("EPHEMERAL_READY"), {
        timeoutMs: 60_000,
      });
      const alpha = await runtime.connectClient({
        name: "ephemeral-failures-publisher",
        contract: apps.Alpha.request,
      });
      for (const value of ["fail", "panic", "after"]) {
        await alpha.publishAlpha({ site: "one", value }).orThrow();
      }
      await runtime.waitFor(() => output.includes("EPHEMERAL_HANDLED after"), {
        timeoutMs: 30_000,
      });
      const failures = output
        .split("\n")
        .filter((line) => line.includes("event handler failed"));
      assert.equal(failures.length, 2, output);
      assert(failures[0]!.includes("fixture ephemeral failure"), failures[0]);
      assert(failures[1]!.includes("fixture ephemeral panic"), failures[1]);
      assert(!output.includes("RUN_ENDED"), output);
    } finally {
      child.kill("SIGTERM");
      await child.exited;
    }
  });
});
