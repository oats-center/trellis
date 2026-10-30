import assert from "node:assert/strict";
import { join } from "node:path";
import { Readable, Writable } from "node:stream";
import { TextDecoderStream } from "node:stream/web";
import { test } from "node:test";
import {
  apis,
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { spawnTracked } from "./_support/process.ts";
import {
  authorizeRustNativeLogin,
  rustFixtureArgv,
  withTrellisRuntime,
} from "./_support/runtime.ts";

test("a Rust upload cancellation settles within the client timeout when its provider stops answering", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const identity = await runtime.registerService({
        name: "unresponsive-upload-provider",
        contract: participants.OperationProvider.participant,
      });
      // Its own process group, so the whole provider can be frozen.
      const provider = spawnTracked(
        "setsid",
        rustFixtureArgv("operation_storage_upload"),
        {
          env: {
            ...process.env,
            TRELLIS_URL: runtime.trellisUrl,
            TRELLIS_CA_CERTIFICATE: runtime.caCertificate,
            TRELLIS_IDENTITY_SEED: identity.seed,
          },
        },
      );
      provider.stderr.pipe(process.stderr);
      provider.stdout.resume();
      const callerArgv = rustFixtureArgv("caller");
      const caller = spawnTracked(callerArgv[0], callerArgv.slice(1), {
        env: {
          ...process.env,
          TRELLIS_URL: runtime.trellisUrl,
          TRELLIS_CA_CERTIFICATE: runtime.caCertificate,
          TRELLIS_CALLER_MODE: "cancel-unresponsive-upload",
          XDG_CONFIG_HOME: join(runtime.workdir, "rust-caller-config"),
        },
      });
      caller.stderr.pipe(process.stderr);
      const writer = Writable.toWeb(caller.stdin).getWriter();
      let output = "";
      const reading = (async () => {
        for await (const text of Readable.toWeb(caller.stdout).pipeThrough(
          new TextDecoderStream(),
        ))
          output += text;
      })();
      let frozen = false;
      try {
        await runtime.waitFor(
          () =>
            runtime
              .nativeTransportGate()
              .connections()
              .some((connection) =>
                connection.subs.some(
                  (sub) =>
                    sub.subject ===
                    apis.runtime.API.actions["operation:Upload"].subject,
                ),
              ),
          { timeoutMs: 60_000 },
        );
        await runtime.waitFor(() => /^NATIVE_LOGIN .+\n/m.test(output), {
          timeoutMs: 60_000,
        });
        await authorizeRustNativeLogin(runtime, output, apps.Caller.request);
        await runtime.waitFor(() => output.includes("uploading\n"), {
          timeoutMs: 60_000,
        });
        // The provider stays connected but can no longer confirm anything.
        process.kill(-provider.pid!, "SIGSTOP");
        frozen = true;
        await writer.write(new TextEncoder().encode("cancel\n"));
        const settled = await runtime.waitFor(
          () => {
            const line = output
              .split("\n")
              .find((candidate) => candidate.startsWith('{"cancelled"'));
            return line
              ? (JSON.parse(line) as { cancelled: string; ms: number })
              : false;
          },
          // The caller's client timeout is 5s; grant expiry is much later.
          { timeoutMs: 20_000 },
        );
        assert.equal(settled.cancelled, "Some(Timeout)");
        assert(settled.ms < 15_000, `cancellation took ${settled.ms}ms`);
      } finally {
        if (frozen) process.kill(-provider.pid!, "SIGCONT");
        writer.releaseLock();
        caller.kill("SIGKILL");
        process.kill(-provider.pid!, "SIGKILL");
        await Promise.allSettled([caller.exited, provider.exited, reading]);
      }
    },
    { interruptibleNativeProxy: true },
  );
});
