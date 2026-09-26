/**
 * Real-boundary authorization-refresh continuity acceptance.
 *
 * A planned authorization-credential rotation may replace the physical NATS
 * attachment, but it is maintenance of the existing logical Trellis connection:
 * ordinary RPCs continue to succeed, the public connection status never becomes
 * disconnected/reconnecting, and retained Live sessions keep their handler and
 * protocol state instead of terminating and reopening.
 *
 * Each case runs one process-local target past the signed context's full
 * lifetime. A connection that failed to refresh would lose its authority and
 * fail the in-flight requests, so a successful run proves the refresh actually
 * happened rather than passing vacuously. The short lifetime is a test fixture;
 * it is not a production default and does not lengthen any lease.
 */

import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import { assert, assertEquals } from "@std/assert";
import { fromFileUrl } from "@std/path";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";

/** Built server binary supplied by the live test harness. */
function serverBinary(): string {
  return Deno.env.get("TRELLIS_TEST_SERVER_BIN") ??
    fromFileUrl(
      new URL("../../target/debug/trellis-server", import.meta.url),
    );
}

/**
 * A short but sane window. `notBefore` precedes issuance by the 30s allowed
 * clock skew, so the post-issuance validity is `lifetime - skew` (46s) and the
 * proactive refresh fires at `lifetime - refreshLead - skew` (31s) after
 * issuance. Every process-local connection refreshes on that cadence, so a
 * case that runs 60s crosses a real rotation and passes the point where a
 * missed refresh would have fully expired the signed context. This is a test
 * fixture, not a production default.
 */
const shortAuthorizationLifetimes = {
  contextLifetimeSeconds: 76,
  refreshLeadSeconds: 15,
  refreshJitterSeconds: 0,
  minimumContextLifetimeSeconds: 46,
};

const runtimeOptions = {
  authorization: shortAuthorizationLifetimes,
  trellis: {
    command: {
      cmd: serverBinary(),
      args: ["--config", "{config}", "all"],
    },
  },
};

/** Past the 31s refresh and the 46s expiry, so a missed refresh is observable. */
const OBSERVATION_MS = 60_000;

/** Connects one provider service whose Watch source emits continuously. */
async function startContinuousProvider(
  runtime: Parameters<Parameters<typeof withTrellisRuntime>[0]>[0],
  onStart: () => void,
  onCleanup: () => void,
) {
  const identity = await runtime.registerService({
    name: `refresh-live-${crypto.randomUUID()}`,
    contract: participants.Provider.participant,
  });
  const service = await TrellisService.connect({
    trellisUrl: runtime.trellisUrl,
    participant: participants.Provider.participant,
    name: `refresh-live-${crypto.randomUUID()}`,
    seed: identity.seed,
  }).orThrow();
  const exit = service.wait().catch((error: unknown) => error);
  await service.handleWatch(async ({ emit, signal }) => {
    onStart();
    while (!signal.aborted) {
      await emit({ value: `frame-${Date.now()}` }).orThrow();
      await new Promise((resolve) => setTimeout(resolve, 200));
    }
    onCleanup();
  });
  return { service, exit };
}

Deno.test("authorization refresh keeps a live RPC client connected and unsuspended", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: `refresh-rpc-${crypto.randomUUID()}`,
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Provider.participant,
      name: `refresh-rpc-${crypto.randomUUID()}`,
      seed: identity.seed,
    }).orThrow();
    const serviceExit = service.wait().catch((error: unknown) => error);
    await service.handleEcho(({ input }) => Result.ok(input));
    const client = await runtime.connectClient({
      name: "refresh-rpc-caller",
      contract: participants.Caller.participant,
    });
    const phases: string[] = [];
    const unsubscribe = client.connection.subscribe((status) =>
      phases.push(status.phase)
    );
    try {
      assertEquals(
        (await client.echo({ value: "before" }).orThrow()).value,
        "before",
      );

      // Issue real RPCs across every scheduled refresh in the window. A
      // suspended installation or a logical reconnect would surface as a
      // failed request or a public phase transition.
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
      assertEquals(failures, []);

      assertEquals(
        (await client.echo({ value: "after" }).orThrow()).value,
        "after",
      );
      assertEquals(client.connection.status.phase, "connected");
      for (const phase of phases) {
        assertEquals(
          phase,
          "connected",
          "a planned credential rotation must not publish a logical connection transition",
        );
      }
    } finally {
      unsubscribe();
      await client.connection.close();
      await service.stop();
      assertEquals(await serviceExit, undefined);
    }
  }, runtimeOptions);
});

Deno.test("an active live observation continues across consumer and provider authorization refresh", async () => {
  await withTrellisRuntime(async (runtime) => {
    let starts = 0;
    let cleanups = 0;
    // The consumer and the provider share the same proactive refresh schedule,
    // so this window rotates both endpoints of the session. There is no
    // independent public trigger for one endpoint alone.
    const { service, exit } = await startContinuousProvider(
      runtime,
      () => starts += 1,
      () => cleanups += 1,
    );
    const client = await runtime.connectClient({
      name: "refresh-live-caller",
      contract: participants.Caller.participant,
    });
    try {
      const handle = await client.watch({}).orThrow();
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

      // Continue past a full context lifetime. A session that lost authority or
      // was fenced by a physical rotation would stop delivering frames.
      await new Promise((resolve) => setTimeout(resolve, OBSERVATION_MS));
      if (sessionError) throw sessionError;
      assert(
        frames.length > established,
        "a retained live session must continue across credential rotation",
      );
      assertEquals(
        starts,
        1,
        "the provider handler must not be restarted",
      );
      assertEquals(ended, false, "the session must not report terminal");
      assertEquals(client.connection.status.phase, "connected");

      await handle.return?.();
      await drain;
      await runtime.waitFor(() => cleanups === 1, { timeoutMs: 15_000 });
    } finally {
      await client.connection.close();
      await service.stop();
      // A normally closed session settles its provider source; the clean stop
      // is the expected terminal, not a lost-authority failure.
      assertEquals(await exit, undefined);
    }
  }, runtimeOptions);
});

/**
 * Issue the caller context later than the provider's so the provider's own
 * scheduled rotation lands while the retained caller digest is unchanged.
 */
const CALLER_CONTEXT_OFFSET_MS = 20_000;

/**
 * Observe across the provider's scheduled rotation (31s after issuance) and
 * past its original context expiry (46s after issuance), while stopping before
 * the offset caller context would refresh (51s after issuance). Crossing the
 * provider expiry is what proves the rotation happened: a provider that failed
 * to refresh would lose its own guard and terminate the session there.
 */
const RUST_ROTATION_OBSERVATION_MS = 26_000;

Deno.test("a Rust provider live session rebinds an unchanged caller context across its own credential rotation", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: `refresh-live-rust-${crypto.randomUUID()}`,
      contract: participants.LiveProbeProvider.participant,
    });
    const process = new Deno.Command("setsid", {
      args: rustFixtureArgv("live_probe"),
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
    const processStatus = process.status.then((status) => {
      exited = true;
      return status;
    });
    const installer = await runtime.connectClient({
      name: `refresh-live-rust-installer-${crypto.randomUUID()}`,
      contract: participants.LiveProbeCaller.participant,
    });
    try {
      // The Rust provider serves no RPC until it is connected, so a successful
      // Inspect is the readiness signal the rotation window is measured from.
      await runtime.waitFor(async () => {
        if (exited) {
          throw new Error(
            `Rust liveprobe provider exited: ${
              JSON.stringify(
                await processStatus,
              )
            }`,
          );
        }
        return (await installer.inspect(
          { runId: "readiness", streamId: "readiness" },
          { timeout: 1_000 },
        )).isOk();
      }, { timeoutMs: 120_000 });

      await new Promise((resolve) =>
        setTimeout(resolve, CALLER_CONTEXT_OFFSET_MS)
      );

      const caller = await runtime.connectClient({
        name: `refresh-live-rust-caller-${crypto.randomUUID()}`,
        contract: participants.LiveProbeCaller.participant,
      });
      try {
        const runId = `refresh-live-rust-${crypto.randomUUID()}`;
        const feed = await caller.watch({ runId, streamId: "quiet" }).orThrow();
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

        // Drive one released frame at a time across the provider's rotation. A
        // guard that cannot rebind the unchanged caller context on the new
        // epoch stops delivering and terminates the session in this window.
        let next = 1n;
        const startedAt = Date.now();
        let loopError: unknown;
        try {
          while (Date.now() - startedAt < RUST_ROTATION_OBSERVATION_MS) {
            await caller.release({
              runId,
              streamId: "quiet",
              throughIndex: next,
              finish: false,
              fail: false,
            }).orThrow();
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
          "the Rust provider session must deliver frames across its rotation",
        );
        assertEquals(ended, false, "the session must not report terminal");

        const status = await installer.inspect({ runId, streamId: "quiet" })
          .orThrow();
        assertEquals(
          status.starts,
          1n,
          "the provider handler must not be restarted",
        );
        assertEquals(
          status.active,
          1n,
          "the provider source must stay active",
        );

        await feed.return?.();
        await drain;
      } finally {
        await caller.connection.close();
      }
    } finally {
      await installer.connection.close();
      if (!exited) Deno.kill(-process.pid, "SIGTERM");
      await processStatus;
    }
  }, runtimeOptions);
});

/**
 * A real native-path outage case: the participant's actual TCP path to NATS is
 * cut while the proactive refresh fires, then restored on the same endpoint.
 * `shortAuthorizationLifetimes` schedules the refresh ~31s after issuance and
 * leaves the predecessor sufficient only until ~46s, so holding the outage for
 * ~36s puts the refresh inside the outage and a ~54s settle puts the final
 * checks past the predecessor window. This is test-fixture timing, not a
 * production default.
 */
const outageRuntimeOptions = {
  ...runtimeOptions,
  interruptibleNativeProxy: true,
};

/** Hold the participant transport down across the scheduled refresh. */
const OUTAGE_HOLD_MS = 36_000;

/**
 * Settle past the original context's effective validity so a later success
 * cannot be explained by the predecessor credential still being accepted.
 */
const PRE_EXPIRY_SETTLE_MS = 54_000;

Deno.test("a real native NATS outage recovers after authorization refresh and resumes Rust/TS Live", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: `outage-live-rust-${crypto.randomUUID()}`,
      contract: participants.LiveProbeProvider.participant,
    });
    const process = new Deno.Command("setsid", {
      args: rustFixtureArgv("live_probe"),
      env: {
        TRELLIS_URL: runtime.trellisUrl,
        TRELLIS_IDENTITY_SEED: identity.seed,
        // Keep a refresh/reconnect alive across the deliberately unavailable
        // transport instead of cycling through the 5s production default.
        TRELLIS_TIMEOUT_MS: "45000",
        CARGO_TARGET_DIR: fromFileUrl(
          new URL("../../target", import.meta.url),
        ),
      },
      stdout: "inherit",
      stderr: "inherit",
    }).spawn();
    let exited = false;
    const processStatus = process.status.then((status) => {
      exited = true;
      return status;
    });
    const caller = await runtime.connectClient({
      name: `outage-live-caller-${crypto.randomUUID()}`,
      contract: participants.LiveProbeCaller.participant,
    });
    // Scheduled refresh and predecessor expiry are measured from the caller's
    // context issuance, so the fixture window is anchored at connection time.
    const callerConnectedAt = Date.now();
    const phases: string[] = [];
    const unsubscribe = caller.connection.subscribe((status) =>
      phases.push(status.phase)
    );
    try {
      // Establish real functionality before cutting transport: a successful
      // Inspect proves the Rust process booted, both sides authenticated, the
      // NATS route works, and generated routing works.
      await runtime.waitFor(async () => {
        if (exited) {
          throw new Error(
            `Rust liveprobe provider exited: ${
              JSON.stringify(
                await processStatus,
              )
            }`,
          );
        }
        return (await caller.inspect(
          { runId: "readiness", streamId: "readiness" },
          { timeout: 1_000 },
        )).isOk();
      }, { timeoutMs: 120_000 });

      runtime.interruptNativeTransport();

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
      while (Date.now() - callerConnectedAt < OUTAGE_HOLD_MS) {
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
      await runtime.waitFor(
        () => caller.connection.status.phase === "connected",
        { timeoutMs: 30_000 },
      );

      // Recovery immediately after restoration is necessary but not sufficient:
      // settle past the predecessor's effective validity window first.
      const remaining = PRE_EXPIRY_SETTLE_MS - (Date.now() - callerConnectedAt);
      if (remaining > 0) {
        await new Promise((resolve) => setTimeout(resolve, remaining));
      }
      assert(!exited, "the Rust provider must still be running");

      // Ordinary RPC recovery on the same caller against the same Rust process.
      await caller.inspect({ runId: "after", streamId: "after" }).orThrow();

      // New Live work: the same caller opens a fresh observation against the
      // same Rust provider, releases index 1, and receives the real signed
      // frame on both sides' resumed Live managers.
      const runId = `outage-live-${crypto.randomUUID()}`;
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
        await caller.release({
          runId,
          streamId,
          throughIndex: 1n,
          finish: false,
          fail: false,
        }).orThrow();
        await runtime.waitFor(() => frames.length >= 1, { timeoutMs: 20_000 });
      } finally {
        await feed.return?.();
        await drain;
      }
      if (sessionError) throw sessionError;
      assertEquals(frames[0]?.runId, runId, "the frame carries the run");
      assertEquals(
        frames[0]?.streamId,
        streamId,
        "the frame carries the stream",
      );
      assertEquals(
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
      if (!exited) Deno.kill(-process.pid, "SIGTERM");
      await processStatus;
    }
  }, outageRuntimeOptions);
});
