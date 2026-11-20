/** Live openings and activations end within their deadlines. */
import assert from "node:assert/strict";
import { join } from "node:path";
import { test } from "node:test";
import { TransportError } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
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

const WATCH = apis.runtime.API.actions["live:Watch"].subject;

test("a Live opening resend that wakes after the caller's timeout is never sent", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const gate = runtime.nativeTransportGate();
      const openingsSent = () =>
        gate
          .connections()
          .flatMap((connection) => connection.outboundContexts)
          .filter((publication) => publication.subject === WATCH).length;
      const identity = await runtime.registerService({
        name: "live-deadline-provider",
        contract: participants.Provider.participant,
      });
      const client = await runtime.connectClient({
        name: "live-deadline-caller",
        contract: apps.Caller.request,
        timeout: 3_000,
      });
      let stopService: (() => Promise<void>) | undefined;
      try {
        // With no provider the broker answers no-responders and the opening is
        // resent after 100, 200, 400, and 800 ms; its last resend waits
        // 1000 ms and would go out at about 2.5 s. A caller whose event loop
        // is busy from 2 s until after its 3 s timeout wakes with that resend
        // overdue.
        const started = Date.now();
        const late = client.runtimeWatch({});
        await new Promise((resolve) => setTimeout(resolve, 2_000));
        const sentBeforeStall = openingsSent();
        assert(sentBeforeStall >= 2, "the opening was resending");
        while (Date.now() < started + 3_200);
        const outcome = await late;
        assert(outcome.isErr());
        assert(outcome.error instanceof TransportError, String(outcome.error));
        assert.equal(outcome.error.code, "trellis.live.setup_timeout");

        const service = await TrellisService.connect({
          trellisUrl: runtime.trellisUrl,
          participant: participants.Provider.participant,
          seed: identity.seed,
        }).orThrow();
        stopService = async () => {
          await service.stop();
          await service.wait();
        };
        await service.handleWatch(async ({ emit }) => {
          await emit({ value: "ready" }).orThrow();
        });

        // A later opening on the same connection reaches the provider, so
        // anything the late opening had sent passed the transport before it.
        const ready = await client.runtimeWatch({}).orThrow();
        try {
          const event = await ready[Symbol.asyncIterator]().next();
          assert(!event.done);
          assert.deepEqual(event.value, { value: "ready" });
        } finally {
          await ready[Symbol.asyncDispose]();
        }
        assert.equal(
          openingsSent() - sentBeforeStall,
          1,
          "an opening was sent after the caller's timeout",
        );
      } finally {
        await client.connection.close();
        await stopService?.();
      }
    },
    { interruptibleNativeProxy: true },
  );
});

test("a Live session whose data subscription is refused during activation ends at once", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const gate = runtime.nativeTransportGate();
      const identity = await runtime.registerService({
        name: "live-refused-provider",
        contract: participants.Provider.participant,
      });
      const service = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: participants.Provider.participant,
        seed: identity.seed,
      }).orThrow();
      await service.handleWatch(async ({ emit }) => {
        await emit({ value: "ready" }).orThrow();
      });
      const client = await runtime.connectClient({
        name: "live-refused-caller",
        contract: apps.Caller.request,
      });
      try {
        const observation = await client.runtimeWatch({}).orThrow();
        // The broker refuses the caller's subscription to the session's frames,
        // which the caller opens as it starts to activate.
        const refused = gate.refuseNextSubscription("live.v1.data.");
        const started = Date.now();
        const first = observation.next().then(
          (next) => next,
          (error: unknown) => error,
        );
        assert.match(await refused, /^live\.v1\.data\./);
        const end = await Promise.race([
          observation.closed,
          new Promise<"open">((resolve) =>
            setTimeout(() => resolve("open"), 10_000),
          ),
        ]);
        assert(end !== "open", "the refused Live session never ended");
        assert.equal(end.reason, "disconnected");
        assert.match(String(end.error), /data subscription failed/);
        // Its 15 s activation reservation never runs out first.
        const elapsed = Date.now() - started;
        assert(elapsed < 3_000, `ended ${elapsed} ms after activation began`);
        assert(
          (await first) instanceof Error,
          "the caller's read reports the end",
        );
      } finally {
        await client.connection.close();
        await service.stop();
        await service.wait();
      }
    },
    { interruptibleNativeProxy: true },
  );
});

test("a Rust Live session whose data subscription is refused during activation ends at once", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const gate = runtime.nativeTransportGate();
      const identity = await runtime.registerService({
        name: "live-refused-rust-provider",
        contract: participants.Provider.participant,
      });
      const service = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: participants.Provider.participant,
        seed: identity.seed,
      }).orThrow();
      await service.handleWatch(async ({ emit }) => {
        await emit({ value: "ready" }).orThrow();
      });
      const callerArgv = rustFixtureArgv("caller");
      const caller = spawnTracked(callerArgv[0], callerArgv.slice(1), {
        env: {
          ...process.env,
          TRELLIS_URL: runtime.trellisUrl,
          TRELLIS_CA_CERTIFICATE: runtime.caCertificate,
          TRELLIS_CALLER_MODE: "live-activation",
          XDG_CONFIG_HOME: join(runtime.workdir, "rust-caller-config"),
        },
      });
      caller.stderr.pipe(process.stderr);
      let output = "";
      caller.stdout.on("data", (chunk: Buffer) => {
        output += chunk.toString();
      });
      try {
        await runtime.waitFor(() => /^NATIVE_LOGIN .+\n/m.test(output), {
          timeoutMs: 60_000,
        });
        await authorizeRustNativeLogin(runtime, output, apps.Caller.request);
        await runtime.waitFor(() => output.includes("prepared\n"), {
          timeoutMs: 60_000,
        });
        // The broker refuses the caller's subscription to the session's frames,
        // which the caller opens as it starts to activate.
        const refused = gate.refuseNextSubscription("live.v1.data.");
        caller.stdin.write("activate\n");
        assert.match(await refused, /^live\.v1\.data\./);
        const end = await runtime.waitFor(
          () => {
            const line = output
              .split("\n")
              .find((candidate) => candidate.startsWith('{"error"'));
            return line
              ? (JSON.parse(line) as {
                  first: string;
                  reason: string;
                  error: string | null;
                  ms: number;
                })
              : false;
          },
          { timeoutMs: 20_000 },
        );
        assert.equal(end.reason, "disconnected");
        assert.match(String(end.error), /data subscription failed/);
        // Its 15 s activation reservation never runs out first.
        assert(end.ms < 3_000, `ended ${end.ms} ms after activation began`);
        assert.match(
          end.first,
          /^Some\(Err\(/,
          "the caller's read reports the end",
        );
      } finally {
        caller.kill("SIGKILL");
        await caller.exited;
        await service.stop();
        await service.wait();
      }
    },
    { interruptibleNativeProxy: true },
  );
});

test("every Rust Live session in a burst of data subscription refusals ends at once", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const gate = runtime.nativeTransportGate();
      const identity = await runtime.registerService({
        name: "live-refused-burst-provider",
        contract: participants.Provider.participant,
      });
      // A provider holds at most 16 sessions per caller, so two replicas of
      // one deployment carry more sessions than that on one caller connection.
      const replicas = await Promise.all(
        ["a", "b"].map(async (replica) => {
          const service = await TrellisService.connect({
            trellisUrl: runtime.trellisUrl,
            participant: participants.Provider.participant,
            name: `live-refused-burst-${replica}`,
            seed: identity.seed,
          }).orThrow();
          await service.handleWatch(async ({ emit }) => {
            await emit({ value: "ready" }).orThrow();
          });
          return service;
        }),
      );
      // More refusals at once than any fixed-size backlog of refusal notices
      // on the connection would hold.
      const sessions = 24;
      const callerArgv = rustFixtureArgv("caller");
      const caller = spawnTracked(callerArgv[0], callerArgv.slice(1), {
        env: {
          ...process.env,
          TRELLIS_URL: runtime.trellisUrl,
          TRELLIS_CA_CERTIFICATE: runtime.caCertificate,
          TRELLIS_CALLER_MODE: "live-activation",
          TRELLIS_CALLER_SESSIONS: String(sessions),
          XDG_CONFIG_HOME: join(runtime.workdir, "rust-caller-config"),
        },
      });
      caller.stderr.pipe(process.stderr);
      let output = "";
      caller.stdout.on("data", (chunk: Buffer) => {
        output += chunk.toString();
      });
      try {
        await runtime.waitFor(() => /^NATIVE_LOGIN .+\n/m.test(output), {
          timeoutMs: 60_000,
        });
        await authorizeRustNativeLogin(runtime, output, apps.Caller.request);
        await runtime.waitFor(() => output.includes("prepared\n"), {
          timeoutMs: 60_000,
        });
        // The broker refuses every session's subscription to its frames, which
        // all the sessions open together as they start to activate.
        const refused = gate.refuseNextSubscriptions("live.v1.data.", sessions);
        caller.stdin.write("activate\n");
        assert.equal(new Set(await refused).size, sessions);
        const ends = await runtime.waitFor(
          () => {
            const lines = output
              .split("\n")
              .filter((candidate) => candidate.startsWith('{"error"'));
            return lines.length === sessions
              ? lines.map(
                  (line) =>
                    JSON.parse(line) as {
                      reason: string;
                      error: string | null;
                      ms: number;
                    },
                )
              : false;
          },
          { timeoutMs: 30_000 },
        );
        for (const end of ends) {
          assert.equal(end.reason, "disconnected", JSON.stringify(end));
          assert.match(String(end.error), /data subscription failed/);
          // Its 15 s activation reservation never runs out first.
          assert(end.ms < 3_000, `ended ${end.ms} ms after activation began`);
        }
      } finally {
        caller.kill("SIGKILL");
        await caller.exited;
        await Promise.all(
          replicas.map(async (service) => {
            await service.stop();
            await service.wait();
          }),
        );
      }
    },
    { interruptibleNativeProxy: true },
  );
});
