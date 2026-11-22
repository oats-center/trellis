import assert from "node:assert/strict";
import { readdir } from "node:fs/promises";
import { basename, join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { test } from "node:test";
import { codecs } from "@oatscenter/trellis/generated";
import { ulid } from "ulid";

import { adminParticipant } from "../packages/trellis-testkit/src/admin/methods.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

const RETRIED = "Auth background work failed; it will run again";

// Another process holds the Auth database's write lock longer than
// trellis-server waits for it, so the Auth background work fails while the lock
// is held. trellis-server must log the failure and try again rather than stop:
// once the lock is released it still answers, and a revocation made afterwards
// is still enforced. This catches a background loop that ends, and takes the
// server down with it, when the database is locked for a few seconds.
test(
  "a write lock held on the Auth database for several seconds does not stop trellis-server",
  { timeout: 180_000 },
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const client = await runtime.connectClient({
        name: "storage-lock-recovery",
        contract: adminParticipant,
      });
      const before = await client.sessionsMe({}).orThrow();

      const files = await readdir(runtime.workdir, { recursive: true });
      const path = files.find((file) => basename(file) === "platform.sqlite");
      assert(path, files.join("\n"));
      const database = new DatabaseSync(join(runtime.workdir, path));
      database.exec("BEGIN IMMEDIATE");
      try {
        // Each attempt waits out the server's busy timeout before it fails,
        // so two failures mean the lock outlasted it and the work was tried
        // again while it was held.
        await runtime.waitFor(
          () => runtime.controlPlaneOutput().split(RETRIED).length > 2,
          { timeoutMs: 60_000 },
        );
      } finally {
        database.exec("ROLLBACK");
        database.close();
      }

      // Still serving, and the background work still enforces a revocation.
      await runtime.callAdminRpc("authAuthorizationSessionsRevoke", {
        authorizationSessionId: before.authorizationSessionId,
        idempotencyKey: codecs.ulid.decode(ulid()),
      });
      await runtime.waitFor(
        async () => {
          const result = await client.sessionsMe({}, { timeout: 1000 });
          return (
            result.isOk() &&
            result.orThrow().authorizationSessionId !==
              before.authorizationSessionId
          );
        },
        { timeoutMs: 30_000 },
      );
      await client.connection.close();
    });
  },
);
