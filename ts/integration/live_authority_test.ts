/**
 * Real-boundary live authority acceptance.
 *
 * Exercises continued observed authority against an actual runtime: a quiet
 * active observation must end when its caller authority is revoked, without
 * waiting for another domain frame, and the provider source must settle.
 */

import { TrellisService } from "@oatscenter/trellis/service";
import { assert, assertEquals } from "@std/assert";
import { fromFileUrl } from "@std/path";
import { participants as webParticipants } from "trellis-web-generated";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

/** Built server binary supplied by the live test harness. */
function serverBinary(): string {
  return Deno.env.get("TRELLIS_TEST_SERVER_BIN") ??
    fromFileUrl(
      new URL("../../rust/target/debug/trellis-server", import.meta.url),
    );
}

Deno.test("G02/T14 revoking the caller fences a quiet live observation", async () => {
  await withTrellisRuntime(async (runtime) => {
    const admin = await runtime.connectClient({
      name: "live-authority-admin",
      contract: webParticipants.Console.participant,
    });
    let starts = 0;
    let cleanups = 0;
    let emitted = 0;
    const identity = await runtime.registerService({
      name: `live-authority-${crypto.randomUUID()}`,
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Provider.participant,
      name: `live-authority-${crypto.randomUUID()}`,
      seed: identity.seed,
    }).orThrow();
    const serviceExit = service.wait().catch((error: unknown) => error);
    // One value, then quiet: the session is ACTIVE with no further frames, so
    // any end after revocation cannot be attributed to a delivered frame.
    await service.handleWatch(async ({ emit, signal }) => {
      starts += 1;
      await emit({ value: "quiet-session" }).orThrow();
      emitted += 1;
      await new Promise<void>((resolve) => {
        signal.addEventListener("abort", () => resolve(), { once: true });
      });
      cleanups += 1;
    });

    const client = await runtime.connectClient({
      name: "live-authority-caller",
      contract: participants.Caller.participant,
    });
    try {
      const handle = await client.watch({}).orThrow();
      type DrainOutcome = {
        kind: "ended" | "threw";
        count: number;
        message?: string;
      };
      const drain: Promise<DrainOutcome> = (async () => {
        let count = 0;
        try {
          for await (const _value of handle) count += 1;
          return { kind: "ended", count };
        } catch (error) {
          return {
            kind: "threw",
            count,
            message: error instanceof Error ? error.message : String(error),
          };
        }
      })();
      await runtime.waitFor(() => emitted === 1, { timeoutMs: 15_000 });
      assertEquals(starts, 1);

      // The runtime is freshly started for this case, so the only active
      // Caller session is this test's caller.
      const session = (await admin.sessionsList({
        participantId: participants.Caller.participant.id,
        state: "active",
      }).orThrow()).items[0];
      assert(session, "caller login session must be listed before revoke");
      const revoke = {
        sessionId: session.sessionId,
        expectedVersion: session.version,
        idempotencyKey: crypto.randomUUID(),
        reason: "live authority acceptance",
      };
      await admin.sessionsRevoke(revoke).orThrow();

      // The observation must terminate on its own, without another data frame
      // and without the application asking again.
      const settled = await Promise.race<DrainOutcome | "timeout">([
        drain,
        new Promise<"timeout">((resolve) =>
          setTimeout(() => resolve("timeout"), 20_000)
        ),
      ]);
      assert(settled !== "timeout", "a revoked observation must fence itself");
      // A drained value followed by a bounded failure, or an immediate
      // failure, are both valid; a silent infinite wait is not.
      assert(settled.kind === "ended" || settled.kind === "threw");
      await runtime.waitFor(() => cleanups === 1, { timeoutMs: 15_000 });
      assertEquals(cleanups, 1);
    } finally {
      await client.connection.close();
      await service.stop();
      assertEquals(await serviceExit, undefined);
    }
  }, {
    trellis: {
      command: {
        cmd: serverBinary(),
        args: ["--config", "{config}", "all"],
      },
    },
  });
});
