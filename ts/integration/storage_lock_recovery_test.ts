import assert from "node:assert/strict";
import { appendFile, readFile, readdir } from "node:fs/promises";
import { basename, join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { test } from "node:test";
import { setTimeout as sleep } from "node:timers/promises";
import { codecs } from "@oatscenter/trellis/generated";
import type { TrellisTestRuntime } from "@oatscenter/trellis-testkit";
import { ulid } from "ulid";

import { adminParticipant } from "../packages/trellis-testkit/src/admin/methods.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

/**
 * Lines trellis-server logs each time a pass of Auth `work` fails, in the
 * background or, with `message`, at startup.
 */
function failures(
  runtime: TrellisTestRuntime,
  work: string,
  message = "Auth work failed; it will run again",
): number {
  return runtime
    .controlPlaneOutput()
    .split("\n")
    .filter(
      (line) => line.includes(message) && line.includes(`"work":"${work}"`),
    ).length;
}

/** Opens the runtime's Auth database from this process. */
async function authDatabase(
  runtime: TrellisTestRuntime,
): Promise<DatabaseSync> {
  const files = await readdir(runtime.workdir, { recursive: true });
  const path = files.find((file) => basename(file) === "platform.sqlite");
  assert(path, files.join("\n"));
  // The running server writes it too, so taking the lock can wait for it.
  return new DatabaseSync(join(runtime.workdir, path), { timeout: 10_000 });
}

/** Connects an admin client to the runtime. */
function connectAdmin(runtime: TrellisTestRuntime, name: string) {
  return runtime.connectClient({ name, contract: adminParticipant });
}

/** Revokes the client's authorization session and waits for it to be enforced. */
async function revocationEnforced(
  runtime: TrellisTestRuntime,
  client: Awaited<ReturnType<typeof connectAdmin>>,
): Promise<void> {
  const before = await client.sessionsMe({}).orThrow();
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
}

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
      const client = await connectAdmin(runtime, "storage-lock-recovery");
      const database = await authDatabase(runtime);
      database.exec("BEGIN IMMEDIATE");
      try {
        // Each attempt waits out the server's busy timeout before it fails,
        // so two failures mean the lock outlasted it and the work was tried
        // again while it was held.
        await runtime.waitFor(
          () => failures(runtime, "Auth background work") >= 2,
          { timeoutMs: 60_000 },
        );
      } finally {
        database.exec("ROLLBACK");
        database.close();
      }
      await revocationEnforced(runtime, client);
      await client.connection.close();
    });
  },
);

// trellis-server starts while another process holds the Auth database's
// write lock for well beyond the time a connection waits for it. Startup must
// wait for the database, logging why, and finish once the lock is released.
// This catches a startup step that gives up on a locked database and stops the
// server.
test(
  "trellis-server started while the Auth database is locked starts once the lock is released",
  { timeout: 240_000 },
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const database = await authDatabase(runtime);
      database.exec("BEGIN IMMEDIATE");
      let restarted: Promise<void>;
      try {
        restarted = runtime.restart();
        // Holding the lock is the condition under test. It outlasts the old
        // server's bounded shutdown and then several of the new server's
        // five-second waits for it.
        await sleep(30_000);
      } finally {
        database.exec("ROLLBACK");
        database.close();
      }
      await restarted;
      assert(
        failures(
          runtime,
          "Auth startup",
          "startup storage work failed; it will run again",
        ) > 0,
        "the restarted server reported waiting for the locked database",
      );
      const client = await connectAdmin(runtime, "started-after-lock");
      await revocationEnforced(runtime, client);
      await client.connection.close();
    });
  },
);

// trellis-server is stopped while another process holds the Auth database's
// write lock. Background store work it started before the stop is still
// waiting for the lock, each call for the whole 60-second busy timeout set
// here. The server must stop without finishing that work, exit without
// waiting for it, and report it. This catches a server that outlives its
// shutdown by the busy timeout of every store call queued behind the lock,
// until its supervisor kills it.
test(
  "trellis-server stopped while the Auth database is locked exits without waiting for the lock",
  { timeout: 240_000 },
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const config = join(runtime.workdir, "config", "trellis", "config.toml");
      await appendFile(
        config,
        '\n[platform.storage]\nkind = "sqlite"\nbusy_timeout_ms = 60000\n',
      );
      await runtime.restart();
      const database = await authDatabase(runtime);
      database.exec("BEGIN IMMEDIATE");
      try {
        // Background store work runs every second, so some of it is waiting
        // for the lock by now.
        await sleep(3_000);
        const stopping = Date.now();
        await runtime.stop();
        // The runtime's shutdown bound is 10 seconds, after which it kills
        // the server.
        const stopped = Date.now() - stopping;
        assert(stopped < 8_000, `stopping took ${stopped}ms`);
        assert.match(
          runtime.controlPlaneOutput(),
          /exiting with storage calls still waiting for their database/,
        );
      } finally {
        database.exec("ROLLBACK");
        database.close();
      }
    });
  },
);

// An installation that sets a longer busy timeout for its Auth database
// waits that long for a lock. A lock held for eight seconds, longer than the
// five-second default but shorter than the 30 seconds configured here, must be
// waited out: the background work fails no pass while it is held. This catches
// a connection that ignores `busy_timeout_ms`.
test(
  "the Auth database waits for a lock as long as its configured busy timeout",
  { timeout: 240_000 },
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const config = join(runtime.workdir, "config", "trellis", "config.toml");
      const written = await readFile(config, "utf8");
      assert(!/^\[platform/m.test(written), written);
      await appendFile(
        config,
        '\n[platform.storage]\nkind = "sqlite"\nbusy_timeout_ms = 30000\n',
      );
      await runtime.restart();
      const client = await connectAdmin(runtime, "configured-busy-timeout");

      const database = await authDatabase(runtime);
      database.exec("BEGIN IMMEDIATE");
      try {
        // The lock's length is the condition under test.
        await sleep(8_000);
      } finally {
        database.exec("ROLLBACK");
        database.close();
      }
      await revocationEnforced(runtime, client);
      assert.equal(
        failures(runtime, "Auth background work"),
        0,
        runtime.controlPlaneOutput(),
      );
      await client.connection.close();
    });
  },
);
