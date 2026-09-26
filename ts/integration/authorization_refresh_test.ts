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
import { withTrellisRuntime } from "./_support/runtime.ts";

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
