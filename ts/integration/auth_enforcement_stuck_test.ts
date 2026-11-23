import assert from "node:assert/strict";
import { readdir } from "node:fs/promises";
import { basename, join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { test } from "node:test";
import { codecs } from "@oatscenter/trellis/generated";
import type { TrellisTestRuntime } from "@oatscenter/trellis-testkit";
import { ulid } from "ulid";

import { apps } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { adminParticipant } from "../packages/trellis-testkit/src/admin/methods.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

/** Auth's `enforcement` check in the platform's latest heartbeat, once `matches`. */
function enforcementCheck(
  runtime: TrellisTestRuntime,
  observer: Awaited<ReturnType<typeof connectObserver>>,
  matches: (check: { status: string; summary?: string }) => boolean,
) {
  return runtime.waitFor(
    async () => {
      const { participant, instances } = await observer
        .healthInspect({
          contractId: "trellis.platform",
          participantKind: "service",
        })
        .orThrow();
      for (const instance of instances) {
        const check = instance.latestSample.checks.find(
          (entry) => entry.name === "enforcement",
        );
        if (check && matches(check)) {
          return { check, status: participant.effectiveStatus };
        }
      }
      return undefined;
    },
    { timeoutMs: 120_000 },
  );
}

/** Connects a client that reads health. */
function connectObserver(runtime: TrellisTestRuntime, name: string) {
  return runtime.connectClient({ name, contract: apps.Observer.request });
}

/** Connects an admin client to the runtime. */
function connectAdmin(runtime: TrellisTestRuntime, name: string) {
  return runtime.connectClient({ name, contract: adminParticipant });
}

// The Auth database refuses, for good, to finish one revocation's enforcement
// page (a trigger another process added aborts it). The page is retried every
// minute, since its session still needs enforcing; the error is logged once
// rather than every minute, and Auth's health reports degraded, naming the
// stuck work, until a page succeeds. Once the trigger is gone the session is
// revoked and Auth reports healthy again.
// This catches stuck enforcement that only shows as an error repeated every
// minute in the log, and health that stays degraded after it recovers.
test(
  "a permanently failing enforcement page shows as degraded Auth health and recovers",
  { timeout: 360_000 },
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const client = await connectAdmin(runtime, "stuck-enforcement");
      const observer = await connectObserver(
        runtime,
        "stuck-enforcement-observer",
      );
      const session = (await client.sessionsMe({}).orThrow())
        .authorizationSessionId;
      // Auth starts healthy.
      await enforcementCheck(
        runtime,
        observer,
        (check) => check.status === "ok",
      );

      const files = await readdir(runtime.workdir, { recursive: true });
      const path = files.find((file) => basename(file) === "platform.sqlite");
      assert(path, files.join("\n"));
      const database = new DatabaseSync(join(runtime.workdir, path), {
        timeout: 10_000,
      });
      try {
        database.exec(
          "CREATE TRIGGER refuse_page BEFORE UPDATE OF state ON auth_enforcement_work " +
            `WHEN NEW.scope_json LIKE '%${session}%' ` +
            "BEGIN SELECT RAISE(ABORT, 'page refused'); END",
        );
        await runtime.callAdminRpc("authAuthorizationSessionsRevoke", {
          authorizationSessionId: session,
          idempotencyKey: codecs.ulid.decode(ulid()),
        });
        const work = () =>
          database
            .prepare(
              "SELECT work_id AS id, next_attempt_at AS next FROM auth_enforcement_work " +
                "WHERE scope_json LIKE ?",
            )
            .get(`%${session}%`) as { id: string; next: number } | undefined;

        const stuck = await enforcementCheck(
          runtime,
          observer,
          (check) => check.status === "degraded",
        );
        const first = work();
        assert(first, "the revocation queued enforcement work");
        assert.match(String(stuck.check.summary), new RegExp(first.id));
        assert.match(String(stuck.check.summary), /page refused/);
        assert.equal(stuck.status, "degraded");

        // The page is tried again a minute later, and fails again, without
        // another error in the log.
        await runtime.waitFor(() => (work()?.next ?? 0) > first.next, {
          timeoutMs: 90_000,
        });
        const logged = runtime
          .controlPlaneOutput()
          .split("\n")
          .filter(
            (line) =>
              line.includes(first.id) &&
              line.includes("Auth enforcement page failed"),
          );
        assert.equal(logged.length, 1, logged.join("\n"));
        assert.match(logged[0]!, /"level":"ERROR"/);
      } finally {
        database.exec("DROP TRIGGER IF EXISTS refuse_page");
        database.close();
      }

      // The next attempt succeeds: the session ends and Auth is healthy again.
      await runtime.waitFor(
        async () => {
          const result = await client.sessionsMe({}, { timeout: 1000 });
          return (
            result.isOk() && result.orThrow().authorizationSessionId !== session
          );
        },
        { timeoutMs: 90_000 },
      );
      const healthy = await enforcementCheck(
        runtime,
        observer,
        (check) => check.status === "ok",
      );
      assert.notEqual(healthy.status, "degraded");
      await client.connection.close();
      await observer.connection.close();
    });
  },
);
