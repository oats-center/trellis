/**
 * Real-boundary authorization-refresh continuity acceptance.
 *
 * Short-lived CONNECT credentials renew without replacing the physical NATS
 * attachment: RPCs continue, the connection remains connected, and retained
 * Live sessions keep their handler and protocol state.
 *
 * Cases exercise scheduled renewal and continued authenticated serving.
 * The short lifetime is a test fixture, not a production default.
 */

import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { env, kill, stdout, stderr } from "node:process";
import { ulid } from "ulid";
import { spawnTracked } from "./_support/process.ts";

import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import {
  repoTrellisSource,
  rustFixtureArgv,
  withTrellisRuntime,
} from "./_support/runtime.ts";

/** Built server binary supplied by the live test harness. */

/**
 * Supported short-lived authority. The observation extends beyond its initial
 * signed lifetime and spans repeated proactive renewals. A successful RPC alone
 * does not prove renewal, since an admitted socket may remain open; the native
 * provider case also observes a changed context on its authenticated replies.
 */
const shortAuthorizationLifetimes = {
  contextLifetimeSeconds: 38,
};

const runtimeOptions = {
  authorization: shortAuthorizationLifetimes,
  trellis: { source: repoTrellisSource() },
};

/** Observe genuine expiry and two scheduled renewals in a short isolated runtime. */
const OBSERVATION_MS = 48_000;

const fastRuntimeOptions = {
  ...runtimeOptions,
  authorization: {
    contextLifetimeSeconds: 28,
    allowedClockSkewSeconds: 2,
  },
};

test("verified short-lived authority remains usable after delayed admission", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const hold = runtime
        .nativeTransportGate()
        .armResponseHold("trellis.auth.admission.");
      const registering = runtime.registerService({
        name: `delayed-short-authority-${ulid()}`,
        contract: participants.Provider.participant,
      });
      try {
        await Promise.race([
          hold.held,
          registering.then(() => {
            throw new Error("service registered before its admission was held");
          }),
        ]);
        // Move the real 61-second signed authority inside the old fixed
        // 60-second lead while its authenticated reply remains valid.
        await new Promise((resolve) => setTimeout(resolve, 2_100));
        await hold.release();
        const identity = await registering;
        const service = await TrellisService.connect({
          trellisUrl: runtime.oauthIssuer,
          participant: participants.Provider.participant,
          name: "delayed-short-authority",
          seed: identity.seed,
        }).orThrow();
        const exited = service.wait().catch((cause: unknown) => cause);
        try {
          await service.handleEcho(({ input }) => Result.ok(input));
          const caller = await runtime.connectClient({
            name: "delayed-short-authority-caller",
            contract: apps.Caller.request,
          });
          try {
            assert.deepEqual(
              (await caller.echo({ value: "admitted after delay" }).orThrow())
                .value,
              "admitted after delay",
            );
          } finally {
            await caller.connection.close();
          }
        } finally {
          await service.stop();
          await exited;
        }
      } finally {
        await hold.release();
      }
    },
    {
      ...runtimeOptions,
      interruptibleNativeProxy: true,
      authorization: {
        ...shortAuthorizationLifetimes,
        contextLifetimeSeconds: 30,
      },
    },
  );
});

/** Connects one provider service whose Watch source emits continuously. */
async function startContinuousProvider(
  runtime: Parameters<Parameters<typeof withTrellisRuntime>[0]>[0],
  onStart: () => void,
  onCleanup: () => void,
  mayEmit: () => boolean = () => true,
) {
  const identity = await runtime.registerService({
    name: `refresh-live-${ulid()}`,
    contract: participants.Provider.participant,
  });
  const service = await TrellisService.connect({
    trellisUrl: runtime.oauthIssuer,
    participant: participants.Provider.participant,
    name: `refresh-live-${ulid()}`,
    seed: identity.seed,
  }).orThrow();
  const exit = service.wait().catch((error: unknown) => error);
  await service.handleWatch(async ({ emit, signal }) => {
    onStart();
    try {
      while (!signal.aborted) {
        if (mayEmit()) await emit({ value: `frame-${Date.now()}` }).orThrow();
        await new Promise((resolve) => setTimeout(resolve, 200));
      }
    } finally {
      onCleanup();
    }
  });
  return { service, exit };
}

test("authorization refresh keeps a live RPC client connected and unsuspended", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: `refresh-rpc-${ulid()}`,
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.oauthIssuer,
      participant: participants.Provider.participant,
      name: `refresh-rpc-${ulid()}`,
      seed: identity.seed,
    }).orThrow();
    const serviceExit = service.wait().catch((error: unknown) => error);
    await service.handleEcho(({ input }) => Result.ok(input));
    const client = await runtime.connectClient({
      name: "refresh-rpc-caller",
      contract: apps.Caller.request,
    });
    const phases: string[] = [];
    const unsubscribe = client.connection.subscribe((status) =>
      phases.push(status.phase),
    );
    try {
      assert.deepEqual(
        (await client.echo({ value: "before" }).orThrow()).value,
        "before",
      );

      // Issue real RPCs across scheduled refreshes and the expiration of
      // multiple routing credentials without replacing the attachment.
      const failures: unknown[] = [];
      const startedAt = Date.now();
      let requests = 0;
      while (Date.now() - startedAt < OBSERVATION_MS) {
        const result = await client.echo({ value: `during-${requests}` });
        if (result.isErr()) failures.push(result.error);
        requests += 1;
        await new Promise((resolve) => setTimeout(resolve, 100));
      }
      assert(requests > 0, "the case must issue real RPCs");
      assert.deepEqual(failures, []);

      // The retained administrator must also remain usable for ordinary client
      // admission after these refreshes, not merely the already-open caller.
      const admitted = await runtime.connectClient({
        name: "refresh-rpc-later-caller",
        contract: apps.Caller.request,
      });
      try {
        assert.deepEqual(
          (await admitted.echo({ value: "later admission" }).orThrow()).value,
          "later admission",
        );
      } finally {
        await admitted.connection.close();
      }

      assert.deepEqual(
        (await client.echo({ value: "after" }).orThrow()).value,
        "after",
      );
      assert.deepEqual(client.connection.status.phase, "connected");
      for (const phase of phases) {
        assert.deepEqual(
          phase,
          "connected",
          "renewing admission credentials must not transition the connection",
        );
      }
    } finally {
      unsubscribe();
      await client.connection.close();
      await service.stop();
      assert.deepEqual(await serviceExit, undefined);
    }
  }, fastRuntimeOptions);
});

test("an active live observation continues across consumer and provider authorization refresh", async () => {
  await withTrellisRuntime(async (runtime) => {
    let starts = 0;
    let cleanups = 0;
    let mayEmit = true;
    // The consumer and the provider share the same proactive refresh schedule,
    // so this window rotates both endpoints of the session. There is no
    // independent public trigger for one endpoint alone.
    const { service, exit } = await startContinuousProvider(
      runtime,
      () => (starts += 1),
      () => (cleanups += 1),
      () => mayEmit,
    );
    const client = await runtime.connectClient({
      name: "refresh-live-caller",
      contract: apps.Caller.request,
    });
    try {
      const handle = await client.runtimeWatch({}).orThrow();
      const frames: string[] = [];
      let ended = false;
      let sessionError: unknown;
      const drain = (async () => {
        try {
          for await (const value of handle) frames.push(value.value);
        } catch (error) {
          sessionError = error;
        } finally {
          ended = true;
        }
      })();
      await runtime.waitFor(() => frames.length >= 2, { timeoutMs: 20_000 });
      const established = frames.length;

      // First observe DATA across a refresh, then leave the same session quiet
      // across the next credential expiry before resuming its source.
      await new Promise((resolve) => setTimeout(resolve, 26_000));
      assert(frames.length > established, "DATA must continue after renewal");
      mayEmit = false;
      await new Promise((resolve) =>
        setTimeout(resolve, OBSERVATION_MS - 26_000),
      );
      if (sessionError) throw sessionError;
      assert.deepEqual(ended, false, "the quiet session must remain active");
      const beforeResume = frames.length;
      mayEmit = true;
      await runtime.waitFor(() => frames.length > beforeResume, {
        timeoutMs: 15_000,
      });
      assert.deepEqual(starts, 1, "the provider handler must not be restarted");
      assert.deepEqual(ended, false, "the session must not report terminal");
      assert.deepEqual(client.connection.status.phase, "connected");

      await handle.return?.();
      await drain;
      if (sessionError) throw sessionError;
      await runtime.waitFor(() => cleanups === 1, { timeoutMs: 15_000 });
    } finally {
      await client.connection.close();
      await service.stop();
      // A normally closed session settles its provider source; the clean stop
      // is the expected terminal, not a lost-authority failure.
      assert.deepEqual(await exit, undefined);
    }
  }, fastRuntimeOptions);
});

/**
 * Issue the caller context later than the provider's so the provider's own
 * scheduled renewal lands while the retained caller digest is unchanged.
 */
const CALLER_CONTEXT_OFFSET_MS = 20_000;

/**
 * Observe across the provider's scheduled renewal (31s after issuance) and
 * past its original context expiry (46s after issuance), while stopping before
 * the offset caller context would refresh (51s after issuance). Crossing the
 * provider expiry is what proves the renewal happened: a provider that failed
 * to renew in place would lose its own guard and terminate the session there.
 */
const RUST_RENEWAL_OBSERVATION_MS = 26_000;

test("a Rust provider live session renews its authorization in place without interrupting an unchanged caller context", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const identity = await runtime.registerService({
        name: `refresh-live-rust-${ulid()}`,
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
      process.stdout.pipe(stdout);
      process.stderr.pipe(stderr);
      let exited = false;
      const processStatus = process.exited.then((status) => {
        exited = true;
        return status;
      });
      const installer = await runtime.connectClient({
        name: `refresh-live-rust-installer-${ulid()}`,
        contract: apps.LiveProbeCaller.request,
      });
      try {
        // The Rust provider serves no RPC until it is connected, so a successful
        // Inspect is the readiness signal the renewal window is measured from.
        await runtime.waitFor(
          async () => {
            if (exited) {
              throw new Error(
                `Rust liveprobe provider exited: ${JSON.stringify(
                  await processStatus,
                )}`,
              );
            }
            return (
              await installer.inspect(
                { runId: "readiness", streamId: "readiness" },
                { timeout: 1_000 },
              )
            ).isOk();
          },
          { timeoutMs: 120_000 },
        );

        const gate = runtime.nativeTransportGate();
        const providerTransport = await runtime.waitFor(
          () =>
            gate
              .connections()
              .find(
                (connection) =>
                  !connection.closed &&
                  connection.subs.some((sub) =>
                    sub.subject.endsWith(".Inspect"),
                  ),
              ) ?? false,
        );
        const originalContext = providerTransport.outboundContexts.findLast(
          (message) => message.context,
        )?.context;
        assert(
          originalContext,
          "readiness must have an authenticated provider reply",
        );

        await new Promise((resolve) =>
          setTimeout(resolve, CALLER_CONTEXT_OFFSET_MS),
        );

        const caller = await runtime.connectClient({
          name: `refresh-live-rust-caller-${ulid()}`,
          contract: apps.LiveProbeCaller.request,
        });
        try {
          const runId = `refresh-live-rust-${ulid()}`;
          const feed = await caller
            .watch({ runId, streamId: "quiet" })
            .orThrow();
          const frames: bigint[] = [];
          let ended = false;
          let sessionError: unknown;
          const drain = (async () => {
            try {
              for await (const frame of feed) frames.push(frame.index);
            } catch (error) {
              sessionError = error;
            } finally {
              ended = true;
            }
          })();

          // Drive one released frame at a time across the provider's renewal. A
          // guard that cannot rebind the unchanged caller context on the new
          // epoch stops delivering and terminates the session in this window.
          let next = 1n;
          const startedAt = Date.now();
          let loopError: unknown;
          try {
            while (Date.now() - startedAt < RUST_RENEWAL_OBSERVATION_MS) {
              await caller
                .release({
                  runId,
                  streamId: "quiet",
                  throughIndex: next,
                  finish: false,
                  fail: false,
                })
                .orThrow();
              await runtime.waitFor(() => frames.length >= Number(next), {
                timeoutMs: 10_000,
              });
              next += 1n;
              await new Promise((resolve) => setTimeout(resolve, 500));
            }
          } catch (error) {
            loopError = error;
          }

          if (sessionError) throw sessionError;
          if (loopError) throw loopError;
          assert(
            frames.length >= 2,
            "the Rust provider session must deliver frames across its renewal",
          );
          assert.deepEqual(
            ended,
            false,
            "the session must not report terminal",
          );

          const status = await installer
            .inspect({ runId, streamId: "quiet" })
            .orThrow();
          assert.deepEqual(
            status.starts,
            1n,
            "the provider handler must not be restarted",
          );
          assert.deepEqual(
            status.active,
            1n,
            "the provider source must stay active",
          );
          assert(
            gate
              .connections()
              .find((connection) => connection.id === providerTransport.id)
              ?.outboundContexts.some(
                (message) =>
                  message.context && message.context !== originalContext,
              ),
            "the retained provider must actually renew its authenticated response context",
          );

          await feed.return?.();
          await drain;
        } finally {
          await caller.connection.close();
        }
      } finally {
        await installer.connection.close();
        if (!exited && process.pid !== undefined) kill(-process.pid, "SIGTERM");
        await processStatus;
      }
    },
    { ...runtimeOptions, interruptibleNativeProxy: true },
  );
});

/**
 * A real native-path outage case: the participant's actual TCP path to NATS is
 * cut while the proactive refresh fires, then restored on the same endpoint.
 * Both proof windows are anchored to the moment the path is actually cut, not
 * to caller connection, so slow provider readiness cannot shorten the outage or
 * the post-recovery settle. `shortAuthorizationLifetimes` schedules a refresh
 * at most ~31s after any issuance and leaves a context sufficient only until
 * ~46s, so a 36s outage guarantees a refresh opportunity while disconnected and
 * a 55s settle puts the final proof past the cut-time context's validity. This
 * is test-fixture timing, not a production default.
 */
const outageRuntimeOptions = {
  ...runtimeOptions,
  interruptibleNativeProxy: true,
};

/** Hold the participant transport down across the scheduled refresh. */
const OUTAGE_HOLD_MS = 36_000;

/**
 * Settle past the cut-time context's effective validity so a later success
 * cannot be explained by the credential present at the cut still being accepted.
 */
const PRE_EXPIRY_SETTLE_MS = 55_000;

test("a real native NATS outage recovers after authorization refresh and resumes Rust/TS Live", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: `outage-live-rust-${ulid()}`,
      contract: participants.LiveProbeProvider.participant,
    });
    const process = spawnTracked("setsid", rustFixtureArgv("live_probe"), {
      env: {
        ...env,
        TRELLIS_URL: runtime.oauthIssuer,
        TRELLIS_CA_CERTIFICATE: runtime.caCertificate,
        TRELLIS_IDENTITY_SEED: identity.seed,
        // Keep a refresh/reconnect alive across the deliberately unavailable
        // transport instead of cycling through the 5s production default.
        TRELLIS_TIMEOUT_MS: "45000",
        CARGO_TARGET_DIR: fileURLToPath(
          new URL("../../target", import.meta.url),
        ),
      },
    });
    process.stdout.pipe(stdout);
    process.stderr.pipe(stderr);
    let exited = false;
    const processStatus = process.exited.then((status) => {
      exited = true;
      return status;
    });
    const caller = await runtime.connectClient({
      name: `outage-live-caller-${ulid()}`,
      contract: apps.LiveProbeCaller.request,
    });
    const phases: string[] = [];
    const transitions: Array<{ at: number; status: unknown }> = [];
    const unsubscribe = caller.connection.subscribe((status) => {
      phases.push(status.phase);
      transitions.push({ at: Date.now(), status });
    });
    try {
      // Establish real functionality before cutting transport: a successful
      // Inspect proves the Rust process booted, both sides authenticated, the
      // NATS route works, and generated routing works.
      await runtime.waitFor(
        async () => {
          if (exited) {
            throw new Error(
              `Rust liveprobe provider exited: ${JSON.stringify(
                await processStatus,
              )}`,
            );
          }
          return (
            await caller.inspect(
              { runId: "readiness", streamId: "readiness" },
              { timeout: 1_000 },
            )
          ).isOk();
        },
        { timeoutMs: 120_000 },
      );

      runtime.interruptNativeTransport();
      // Anchor both proof windows to the real cut: readiness may have consumed
      // an arbitrary amount of time, and the outage must still span a scheduled
      // refresh and the settle must still cross the cut-time context's validity.
      const outageStartedAt = Date.now();

      // A genuine outage, not a synthesized status event: the caller must
      // publish a real non-connected logical phase.
      await runtime.waitFor(
        () => phases.some((phase) => phase !== "connected"),
        { timeoutMs: 20_000 },
      );
      const downPhase = caller.connection.status.phase;
      assert(
        downPhase !== "connected",
        "the real transport cut must leave the caller logically disconnected",
      );

      // Hold the cut across the scheduled refresh (~31s after issuance) so both
      // endpoints refresh while genuinely disconnected.
      while (Date.now() - outageStartedAt < OUTAGE_HOLD_MS) {
        const phase = caller.connection.status.phase;
        assert(
          phase !== "connected",
          "the outage must keep the logical connection down until restored",
        );
        await new Promise((resolve) => setTimeout(resolve, 500));
      }
      assert(!exited, "the Rust provider must survive the outage");

      runtime.restoreNativeTransport();

      // Ordinary NATS reconnect on the same advertised endpoint: no client or
      // provider recreation and no manual resume. The same logical caller must
      // return to connected.
      await runtime
        .waitFor(() => caller.connection.status.phase === "connected", {
          timeoutMs: 30_000,
        })
        .catch((cause) => {
          throw new Error(
            `Native outage recovery failed: ${JSON.stringify({
              transitions,
              current: caller.connection.status,
              outageStartedAt,
              elapsedMs: Date.now() - outageStartedAt,
            })}`,
            { cause },
          );
        });

      // Recovery immediately after restoration is necessary but not sufficient:
      // settle past the cut-time context's validity window first.
      const remaining = PRE_EXPIRY_SETTLE_MS - (Date.now() - outageStartedAt);
      if (remaining > 0) {
        await new Promise((resolve) => setTimeout(resolve, remaining));
      }
      assert(!exited, "the Rust provider must still be running");

      // Ordinary RPC recovery on the same caller against the same Rust process.
      await caller.inspect({ runId: "after", streamId: "after" }).orThrow();

      // New Live work: the same caller opens a fresh observation against the
      // same Rust provider, releases index 1, and receives the real signed
      // frame on both sides' resumed Live managers.
      const runId = `outage-live-${ulid()}`;
      const streamId = "recovered";
      const feed = await caller.watch({ runId, streamId }).orThrow();
      const frames: Array<{
        runId: string;
        streamId: string;
        index: bigint;
      }> = [];
      let sessionError: unknown;
      const drain = (async () => {
        try {
          for await (const frame of feed) {
            frames.push(frame);
            if (frames.length >= 1) return;
          }
        } catch (error) {
          sessionError = error;
        }
      })();
      try {
        await caller
          .release({
            runId,
            streamId,
            throughIndex: 1n,
            finish: false,
            fail: false,
          })
          .orThrow();
        await runtime.waitFor(() => frames.length >= 1, { timeoutMs: 20_000 });
      } finally {
        await feed.return?.();
        await drain;
      }
      if (sessionError) throw sessionError;
      assert.deepEqual(frames[0]?.runId, runId, "the frame carries the run");
      assert.deepEqual(
        frames[0]?.streamId,
        streamId,
        "the frame carries the stream",
      );
      assert.deepEqual(
        frames[0]?.index,
        1n,
        "the frame carries the released index",
      );
      assert(
        !exited,
        "the Rust provider must have served the recovered Live session",
      );
    } finally {
      unsubscribe();
      await caller.connection.close();
      if (!exited && process.pid !== undefined) kill(-process.pid, "SIGTERM");
      await processStatus;
    }
  }, outageRuntimeOptions);
});
