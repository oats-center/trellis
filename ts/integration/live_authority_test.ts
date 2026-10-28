/**
 * Real-boundary live authority acceptance.
 *
 * Exercises continued observed authority against an actual runtime: a quiet
 * active observation must end when its caller authority is revoked, without
 * waiting for another domain frame, and the provider source must settle.
 */

import { TrellisService } from "@oatscenter/trellis/service";
import assert from "node:assert/strict";
import { test } from "node:test";
import { codecs } from "@oatscenter/trellis/generated";
import { ulid } from "ulid";
import { adminParticipant } from "../packages/trellis-testkit/src/admin/methods.ts";

import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { repoTrellisSource, withTrellisRuntime } from "./_support/runtime.ts";

/** Built server binary supplied by the live test harness. */

test("G02/T14 revoking the caller fences a quiet live observation", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const admin = await runtime.connectClient({
        name: "live-authority-admin",
        contract: adminParticipant,
      });
      let starts = 0;
      let cleanups = 0;
      let emitted = 0;
      const identity = await runtime.registerService({
        name: "live-authority",
        contract: participants.Provider.participant,
      });
      const service = await TrellisService.connect({
        trellisUrl: runtime.oauthIssuer,
        participant: participants.Provider.participant,
        name: "live-authority",
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
        contract: apps.Caller.request,
      });
      try {
        const handle = await client.runtimeWatch({}).orThrow();
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
        assert.deepEqual(starts, 1);

        const session = client.ownAuthority.current;
        assert(session?.loginSessionId);
        const revoke = {
          loginSessionId: codecs.ulid.decode(session.loginSessionId),
          idempotencyKey: ulid(),
        };
        await admin.sessionsRevoke(revoke).orThrow();

        // The observation must terminate on its own, without another data frame
        // and without the application asking again.
        const settled = await Promise.race<DrainOutcome | "timeout">([
          drain,
          new Promise<"timeout">((resolve) =>
            setTimeout(() => resolve("timeout"), 20_000),
          ),
        ]);
        assert(
          settled !== "timeout",
          "a revoked observation must fence itself",
        );
        // A drained value followed by a bounded failure, or an immediate
        // failure, are both valid; a silent infinite wait is not.
        assert(settled.kind === "ended" || settled.kind === "threw");
        await runtime.waitFor(() => cleanups === 1, { timeoutMs: 15_000 });
        assert.deepEqual(cleanups, 1);
      } finally {
        await client.connection.close();
        await service.stop();
        assert.deepEqual(await serviceExit, undefined);
      }
    },
    {
      trellis: { source: repoTrellisSource() },
    },
  );
});
