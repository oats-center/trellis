import assert from "node:assert/strict";
import { readdir } from "node:fs/promises";
import { basename, join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { test } from "node:test";
import { setTimeout as sleep } from "node:timers/promises";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import type { TrellisTestRuntime } from "@oatscenter/trellis-testkit";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { adminParticipant } from "../packages/trellis-testkit/src/admin/methods.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

/** Lines trellis-server logs each time startup `work` fails on its database. */
function failures(runtime: TrellisTestRuntime, work: string): number {
  return runtime
    .controlPlaneOutput()
    .split("\n")
    .filter(
      (line) =>
        line.includes("startup storage work failed; it will run again") &&
        line.includes(`"work":"${work}"`),
    ).length;
}

/** Opens one of the runtime's databases from this process. */
async function database(
  runtime: TrellisTestRuntime,
  file: string,
): Promise<DatabaseSync> {
  const files = await readdir(runtime.workdir, { recursive: true });
  const path = files.find((name) => basename(name) === file);
  assert(path, files.join("\n"));
  // The running server writes it too, so taking the lock can wait for it.
  return new DatabaseSync(join(runtime.workdir, path), { timeout: 10_000 });
}

/**
 * Restarts trellis-server while another process holds `file`'s write lock for
 * 25 seconds, waits for the server to finish starting, and checks that startup
 * `work` reported waiting for the database.
 */
async function restartWhileLocked(
  runtime: TrellisTestRuntime,
  file: string,
  work: string,
): Promise<void> {
  const locked = await database(runtime, file);
  locked.exec("BEGIN IMMEDIATE");
  let restarted: Promise<void>;
  try {
    restarted = runtime.restart();
    // Holding the lock is the condition under test. It outlasts the old
    // server's bounded shutdown and then several of the new server's
    // five-second waits for it, and ends before the restart's readiness wait.
    await sleep(25_000);
  } finally {
    locked.exec("ROLLBACK");
    locked.close();
  }
  await restarted;
  assert(
    failures(runtime, work) > 0,
    `the restarted server reported ${work} waiting for the locked database`,
  );
}

/** Connects the runtime fixture's provider service. */
async function connectProvider(runtime: TrellisTestRuntime, name: string) {
  const key = await runtime.registerService({
    name,
    contract: participants.Provider.participant,
  });
  const service = await TrellisService.connect({
    trellisUrl: runtime.trellisUrl,
    participant: participants.Provider.participant,
    name,
    seed: key.seed,
  }).orThrow();
  service.jobs.work.handle(({ job }) =>
    Promise.resolve(Result.ok(job.payload)),
  );
  return service;
}

/** Connects a client that reads Jobs, Events and Health. */
function connectObserver(runtime: TrellisTestRuntime, name: string) {
  return runtime.connectClient({ name, contract: adminParticipant });
}

// trellis-server starts while another process holds a subsystem's database
// write lock for longer than a connection waits for it. Startup must wait for
// the database, logging why, and finish once the lock is released, and the
// subsystem must then record new work. Each catches a subsystem whose startup
// writes its database once and stops the server when that write fails.

test(
  "trellis-server started while the Jobs database is locked starts and projects jobs",
  { timeout: 240_000 },
  async () => {
    await withTrellisRuntime(async (runtime) => {
      await restartWhileLocked(runtime, "jobs.sqlite", "Jobs startup");
      const service = await connectProvider(runtime, "jobs-after-lock");
      const observer = await connectObserver(runtime, "jobs-after-lock");
      try {
        const job = await service.jobs.work
          .create({ value: "after-lock" })
          .orThrow();
        await runtime.waitFor(
          async () =>
            (await observer.jobsQuery({}).orThrow()).items.some(
              (item) => item.id === job.id,
            ),
          { timeoutMs: 30_000 },
        );
      } finally {
        await observer.connection.close();
        await service.stop();
      }
    });
  },
);

test(
  "trellis-server started while the Events database is locked starts and records events",
  { timeout: 240_000 },
  async () => {
    await withTrellisRuntime(async (runtime) => {
      await restartWhileLocked(runtime, "events.sqlite", "Events startup");
      const service = await connectProvider(runtime, "events-after-lock");
      const observer = await connectObserver(runtime, "events-after-lock");
      try {
        // Nothing published Changed before the restart.
        await service.publishChanged({ value: "after-lock" }).orThrow();
        await runtime.waitFor(
          async () =>
            (
              await observer
                .eventsQuery({ ownerEventName: "Changed" })
                .orThrow()
            ).items.length > 0,
          { timeoutMs: 30_000 },
        );
      } finally {
        await observer.connection.close();
        await service.stop();
      }
    });
  },
);

test(
  "trellis-server started while the Health database is locked starts and projects heartbeats",
  { timeout: 240_000 },
  async () => {
    await withTrellisRuntime(async (runtime) => {
      await restartWhileLocked(runtime, "health.sqlite", "Health startup");
      const service = await connectProvider(runtime, "health-after-lock");
      const observer = await connectObserver(runtime, "health-after-lock");
      try {
        await runtime.waitFor(
          async () =>
            (await observer.healthQuery({}).orThrow()).items.some(
              (item) =>
                item.contractId === participants.Provider.participant.id &&
                item.effectiveStatus === "healthy",
            ),
          { timeoutMs: 60_000 },
        );
      } finally {
        await observer.connection.close();
        await service.stop();
      }
    });
  },
);
