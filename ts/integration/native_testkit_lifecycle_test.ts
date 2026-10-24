import assert from "node:assert/strict";
import { test } from "node:test";
import { join } from "node:path";
import { mkdtemp, readFile, readlink, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { createConnection } from "node:net";
import { once } from "node:events";
import { ulid } from "ulid";
import {
  processIsGone,
  processMatchesIdentity,
  recordProcessIdentity,
} from "../packages/trellis-testkit/src/cleanup.ts";
import { startTrellisRuntime } from "./_support/runtime.ts";
import { startTrellisProcess } from "../packages/trellis-testkit/src/trellis_process.ts";

test("native startup rejects another host's ready listener without disturbing that host", async () => {
  await using runtime = await startTrellisRuntime();
  const logs = await mkdtemp(join(tmpdir(), "duplicate-host-proof-"));
  let duplicate: Awaited<ReturnType<typeof startTrellisProcess>> | undefined;
  try {
    const server = process.env["TRELLIS_TEST_SERVER_BIN"];
    assert(server);
    await assert.rejects(
      async () => {
        duplicate = await startTrellisProcess({
          trellisUrl: runtime.trellisUrl,
          configPath: join(logs, "config.toml"),
          command: {
            cmd: server,
            args: [
              "--isolated-process-group",
              "--config",
              join(runtime.workdir, "config", "trellis", "config.toml"),
              "platform",
            ],
            cwd: runtime.workdir,
            env: { HOME: runtime.workdir, TOKIO_WORKER_THREADS: "2" },
          },
          startupTimeoutMs: 10_000,
          shutdownTimeoutMs: 5_000,
        });
      },
      (error: unknown) =>
        error instanceof Error &&
        error.message.includes("exited before readiness"),
    );
    const { user } = await runtime.callAdminRpc("authUsersCreate", {
      username: "surviving-host",
      displayName: "Surviving host",
      idempotencyKey: ulid(),
    });
    const reset = await runtime.callAdminRpc("authUsersPasswordResetCreate", {
      userId: user.principalId,
      idempotencyKey: ulid(),
    });
    assert(reset.token);
  } finally {
    await duplicate?.stop();
    await rm(logs, { recursive: true, force: true });
  }
});

test("native testkit concurrent hosts isolate mutations, persist through restart, and close owned listeners", async () => {
  const runtimes: Awaited<ReturnType<typeof startTrellisRuntime>>[] = [];
  try {
    const results = await Promise.allSettled(
      Array.from({ length: 4 }, async () => {
        const runtime = await startTrellisRuntime();
        runtimes.push(runtime);
        const { user } = await runtime.callAdminRpc("authUsersCreate", {
          username: "same-user",
          displayName: "Same user",
          idempotencyKey: "01ARZ3NDEKTSV4RRFFQ69G5FAV",
        });
        await runtime.restart();
        const reset = await runtime.callAdminRpc(
          "authUsersPasswordResetCreate",
          {
            userId: user.principalId,
            idempotencyKey: "01ARZ3NDEKTSV4RRFFQ69G5FAW",
          },
        );
        assert(reset.token);
        return user.principalId;
      }),
    );
    const users = results.map((result) => {
      if (result.status === "rejected") throw result.reason;
      return result.value;
    });
    assert.deepEqual(
      new Set(users).size,
      4,
      "same mutations in independent hosts must not share persisted users",
    );
  } finally {
    await Promise.all(runtimes.map((runtime) => runtime.stop()));
  }
  for (const runtime of runtimes) {
    for (const url of [runtime.trellisUrl, runtime.natsUrl]) {
      const endpoint = new URL(url);
      await assert.rejects(
        async () => {
          const connection = createConnection({
            host: endpoint.hostname,
            port: Number(endpoint.port),
          });
          try {
            await once(connection, "connect");
          } finally {
            connection.destroy();
          }
        },
        { code: "ECONNREFUSED" },
      );
    }
  }
});

for (const failure of ["stalled", "crashed"] as const) {
  test(`native testkit cleans up a ${failure} host and its broker without killing another host`, async () => {
    const runtime = await startTrellisRuntime({
      timeouts: { shutdownMs: 1_000 },
    });
    let survivor: Awaited<ReturnType<typeof startTrellisRuntime>> | undefined;
    const brokerPid = Number(
      (
        await readFile(
          join(runtime.workdir, "runtime", "trellis", "nats-server.pid"),
          "utf8",
        )
      ).trim(),
    );
    assert(Number.isSafeInteger(brokerPid) && brokerPid > 0);
    const broker = await recordProcessIdentity(
      brokerPid,
      await readlink(`/proc/${brokerPid}/exe`),
    );
    assert(broker);
    const parent = Number(
      (await readFile(`/proc/${brokerPid}/status`, "utf8")).match(
        /^PPid:\s+(\d+)$/m,
      )?.[1],
    );
    assert(Number.isSafeInteger(parent) && parent > 0);
    const host = await recordProcessIdentity(
      parent,
      await readlink(`/proc/${parent}/exe`),
    );
    assert(host);
    try {
      survivor = await startTrellisRuntime();
      process.kill(host.pid, failure === "stalled" ? "SIGSTOP" : "SIGKILL");
      if (failure === "crashed") {
        await runtime.waitFor(() => processIsGone(host.pid));
      }
      await runtime.stop();
      await runtime.waitFor(
        async () =>
          (await processIsGone(host.pid)) && (await processIsGone(brokerPid)),
      );
      for (const url of [runtime.trellisUrl, runtime.natsUrl]) {
        const endpoint = new URL(url);
        await assert.rejects(
          async () => {
            const connection = createConnection({
              host: endpoint.hostname,
              port: Number(endpoint.port),
            });
            try {
              await once(connection, "connect");
            } finally {
              connection.destroy();
            }
          },
          { code: "ECONNREFUSED" },
        );
      }
      const { user } = await survivor.callAdminRpc("authUsersCreate", {
        username: "after-neighbor-force-close",
        displayName: "Survivor",
        idempotencyKey: ulid(),
      });
      const reset = await survivor.callAdminRpc(
        "authUsersPasswordResetCreate",
        {
          userId: user.principalId,
          idempotencyKey: ulid(),
        },
      );
      assert(reset.token);
    } finally {
      if (await processMatchesIdentity(host)) process.kill(host.pid, "SIGCONT");
      await runtime.stop();
      await survivor?.stop();
      if (await processMatchesIdentity(broker)) {
        process.kill(broker.pid, "SIGKILL");
      }
    }
  });
}
