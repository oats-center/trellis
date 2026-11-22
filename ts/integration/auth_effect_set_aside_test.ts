import assert from "node:assert/strict";
import { readdir } from "node:fs/promises";
import { basename, join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { test } from "node:test";
import { codecs } from "@oatscenter/trellis/generated";
import { ulid } from "ulid";

import { adminParticipant } from "../packages/trellis-testkit/src/admin/methods.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

// The Auth database refuses, for good, to finish one revocation's publication
// effect (a trigger another process added aborts its removal). That effect
// must be set aside, logged once at error with its ID, and must not hold back
// the work after it: the kicks queued behind it still run, and another
// client's revocation is enforced too.
// This catches an effect that fails the same way on every attempt being tried
// forever, with the kicks that wait for it never sent.
test(
  "an Auth effect that cannot finish is set aside and the effects after it run",
  { timeout: 180_000 },
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const poisoned = await runtime.connectClient({
        name: "set-aside-poisoned",
        contract: adminParticipant,
      });
      const other = await runtime.connectClient({
        name: "set-aside-other",
        contract: adminParticipant,
      });
      const poisonedSession = (await poisoned.sessionsMe({}).orThrow())
        .authorizationSessionId;
      const otherSession = (await other.sessionsMe({}).orThrow())
        .authorizationSessionId;

      const files = await readdir(runtime.workdir, { recursive: true });
      const path = files.find((file) => basename(file) === "platform.sqlite");
      assert(path, files.join("\n"));
      const database = new DatabaseSync(join(runtime.workdir, path), {
        timeout: 10_000,
      });
      try {
        database.exec(
          "CREATE TRIGGER refuse_effect BEFORE DELETE ON auth_post_commit_actions " +
            "WHEN OLD.kind='session_revoke' AND " +
            `json_extract(OLD.payload_json,'$.authorizationSessionId')='${poisonedSession}' ` +
            "BEGIN SELECT RAISE(ABORT, 'effect refused'); END",
        );
        for (const session of [poisonedSession, otherSession]) {
          await runtime.callAdminRpc("authAuthorizationSessionsRevoke", {
            authorizationSessionId: session,
            idempotencyKey: codecs.ulid.decode(ulid()),
          });
        }

        // Both revoked sessions end.
        for (const [client, session] of [
          [poisoned, poisonedSession],
          [other, otherSession],
        ] as const) {
          await runtime.waitFor(
            async () => {
              const result = await client.sessionsMe({}, { timeout: 1000 });
              return (
                result.isOk() &&
                result.orThrow().authorizationSessionId !== session
              );
            },
            { timeoutMs: 30_000 },
          );
        }

        const effect = database
          .prepare(
            "SELECT action_id AS id, failed_at AS failed, last_error AS reason " +
              "FROM auth_post_commit_actions WHERE kind='session_revoke' AND " +
              "json_extract(payload_json,'$.authorizationSessionId')=?",
          )
          .get(poisonedSession) as
          | { id: string; failed: number | null; reason: string | null }
          | undefined;
        assert(effect, "the refused effect is kept for inspection");
        assert.notEqual(effect.failed, null, "the refused effect is set aside");
        assert.match(String(effect.reason), /effect refused/);
        const logged = runtime
          .controlPlaneOutput()
          .split("\n")
          .filter(
            (line) =>
              line.includes(effect.id) &&
              line.includes("Auth effect set aside"),
          );
        assert.equal(logged.length, 1, logged.join("\n"));
        assert.match(logged[0]!, /"level":"ERROR"/);

        // The kicks queued behind the set-aside effect ran and finished.
        await runtime.waitFor(
          () =>
            (
              database
                .prepare(
                  "SELECT count(*) AS pending FROM auth_post_commit_actions " +
                    "WHERE kind='kick' AND " +
                    "json_extract(payload_json,'$.authorizationSessionId')=?",
                )
                .get(poisonedSession) as { pending: number }
            ).pending === 0,
          { timeoutMs: 30_000 },
        );
      } finally {
        database.exec("DROP TRIGGER IF EXISTS refuse_effect");
        database.close();
      }
      await poisoned.connection.close();
      await other.connection.close();
    });
  },
);
