import { assert, assertEquals, assertRejects } from "@std/assert";
import { join } from "@std/path";
import {
  processIsGone,
  processMatchesIdentity,
  recordProcessIdentity,
} from "../packages/trellis-testkit/src/cleanup.ts";
import { startTrellisRuntime } from "./_support/runtime.ts";
import { startTrellisProcess } from "../packages/trellis-testkit/src/trellis_process.ts";

Deno.test("native startup rejects another host's ready listener without disturbing that host", async () => {
  await using runtime = await startTrellisRuntime();
  const logs = await Deno.makeTempDir({ prefix: "duplicate-host-proof-" });
  let duplicate: Awaited<ReturnType<typeof startTrellisProcess>> | undefined;
  try {
    const server = Deno.env.get("TRELLIS_TEST_SERVER_BIN");
    assert(server);
    await assertRejects(
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
      Error,
      "exited before readiness",
    );
    const { user } = await runtime.callAdminRpc("authUsersCreate", {
      username: "surviving-host",
      name: "Surviving host",
      email: null,
      image: null,
      idempotencyKey: crypto.randomUUID(),
    });
    const { flow } = await runtime.callAdminRpc(
      "authUsersPasswordResetCreate",
      {
        userId: user.userId,
        returnTarget: null,
        idempotencyKey: crypto.randomUUID(),
      },
    );
    assertEquals(flow.targetPrincipalId, user.principalId);
  } finally {
    await duplicate?.stop();
    await Deno.remove(logs, { recursive: true });
  }
});

Deno.test("native testkit concurrent hosts isolate mutations, persist through restart, and close owned listeners", async () => {
  const runtimes: Awaited<ReturnType<typeof startTrellisRuntime>>[] = [];
  try {
    const results = await Promise.allSettled(
      Array.from({ length: 4 }, async () => {
        const runtime = await startTrellisRuntime();
        runtimes.push(runtime);
        const { user } = await runtime.callAdminRpc("authUsersCreate", {
          username: "same-user",
          name: "Same user",
          email: null,
          image: null,
          idempotencyKey: "same-mutation",
        });
        await runtime.restart();
        const { flow } = await runtime.callAdminRpc(
          "authUsersPasswordResetCreate",
          {
            userId: user.userId,
            returnTarget: null,
            idempotencyKey: "same-reset",
          },
        );
        assertEquals(flow.targetPrincipalId, user.principalId);
        return user.userId;
      }),
    );
    const users = results.map((result) => {
      if (result.status === "rejected") throw result.reason;
      return result.value;
    });
    assertEquals(
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
      await assertRejects(async () => {
        const connection = await Deno.connect({
          hostname: endpoint.hostname,
          port: Number(endpoint.port),
        });
        connection.close();
      }, Deno.errors.ConnectionRefused);
    }
  }
});

for (const failure of ["stalled", "crashed"] as const) {
  Deno.test(`native testkit cleans up a ${failure} host and its broker without killing another host`, async () => {
    const runtime = await startTrellisRuntime({
      timeouts: { shutdownMs: 1_000 },
    });
    let survivor: Awaited<ReturnType<typeof startTrellisRuntime>> | undefined;
    const brokerPid = Number(
      (await Deno.readTextFile(
        join(runtime.workdir, "runtime", "trellis", "nats-server.pid"),
      )).trim(),
    );
    assert(Number.isSafeInteger(brokerPid) && brokerPid > 0);
    const broker = await recordProcessIdentity(
      brokerPid,
      await Deno.readLink(`/proc/${brokerPid}/exe`),
    );
    assert(broker);
    const parent = Number(
      (await Deno.readTextFile(`/proc/${brokerPid}/status`)).match(
        /^PPid:\s+(\d+)$/m,
      )?.[1],
    );
    assert(Number.isSafeInteger(parent) && parent > 0);
    const host = await recordProcessIdentity(
      parent,
      await Deno.readLink(`/proc/${parent}/exe`),
    );
    assert(host);
    try {
      survivor = await startTrellisRuntime();
      Deno.kill(host.pid, failure === "stalled" ? "SIGSTOP" : "SIGKILL");
      if (failure === "crashed") {
        await runtime.waitFor(() => processIsGone(host.pid));
      }
      await runtime.stop();
      await runtime.waitFor(async () =>
        await processIsGone(host.pid) && await processIsGone(brokerPid)
      );
      for (const url of [runtime.trellisUrl, runtime.natsUrl]) {
        const endpoint = new URL(url);
        await assertRejects(async () => {
          const connection = await Deno.connect({
            hostname: endpoint.hostname,
            port: Number(endpoint.port),
          });
          connection.close();
        }, Deno.errors.ConnectionRefused);
      }
      const { user } = await survivor.callAdminRpc("authUsersCreate", {
        username: "after-neighbor-force-close",
        name: "Survivor",
        email: null,
        image: null,
        idempotencyKey: "survivor-user",
      });
      const { flow } = await survivor.callAdminRpc(
        "authUsersPasswordResetCreate",
        {
          userId: user.userId,
          returnTarget: null,
          idempotencyKey: "survivor-reset",
        },
      );
      assertEquals(flow.targetPrincipalId, user.principalId);
    } finally {
      if (await processMatchesIdentity(host)) Deno.kill(host.pid, "SIGCONT");
      await runtime.stop();
      await survivor?.stop();
      if (await processMatchesIdentity(broker)) {
        Deno.kill(broker.pid, "SIGKILL");
      }
    }
  });
}
