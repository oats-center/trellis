/**
 * Readiness and graceful stop of real TypeScript and Rust service processes:
 * what systemd, a readiness probe, and callers see while one instance starts
 * and another stops.
 */
import assert from "node:assert/strict";
import { existsSync } from "node:fs";
import { createServer } from "node:net";
import { join } from "node:path";
import { test } from "node:test";
import { jetstreamManager } from "@nats-io/jetstream";
import { connect, credsAuthenticator } from "@nats-io/transport-node";
import { Result } from "@oatscenter/trellis";
import { codecs } from "@oatscenter/trellis/generated";
import { TrellisService } from "@oatscenter/trellis/service";
import type { TrellisTestRuntime } from "@oatscenter/trellis-testkit";
import { ulid } from "ulid";
import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { NotifySocket } from "./_support/notify_socket.ts";
import { spawnTracked } from "./_support/process.ts";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";

type Line = { event: string; role: string; at: number; value?: string };

const languages = ["typescript", "rust"] as const;

/** Starts the stop-test Provider program in one language, following its output. */
function startService(
  runtime: TrellisTestRuntime,
  language: (typeof languages)[number],
  env: Record<string, string>,
) {
  const [command, ...args] =
    language === "rust"
      ? rustFixtureArgv("graceful_service")
      : [
          process.execPath,
          "--no-warnings",
          join(import.meta.dirname, "_support/graceful_service.ts"),
        ];
  const child = spawnTracked(command, args, {
    env: {
      ...process.env,
      NODE_EXTRA_CA_CERTS: runtime.caCertificate,
      TRELLIS_CA_CERTIFICATE: runtime.caCertificate,
      TRELLIS_URL: runtime.trellisUrl,
      ...env,
    },
  });
  const lines: Line[] = [];
  let output = "";
  let buffered = "";
  child.stdout.on("data", (chunk: Buffer) => {
    buffered += chunk.toString();
    const parts = buffered.split("\n");
    buffered = parts.pop() ?? "";
    for (const part of parts) {
      output += `${part}\n`;
      try {
        lines.push(JSON.parse(part) as Line);
      } catch {
        // A log line, kept in `output`.
      }
    }
  });
  child.stderr.on("data", (chunk: Buffer) => (output += chunk.toString()));
  const seen = (event: string) => lines.find((line) => line.event === event);
  return { child, lines, seen, output: () => output };
}

async function freePort(): Promise<number> {
  const server = createServer();
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  await new Promise((resolve) => server.close(resolve));
  assert(address && typeof address === "object");
  return address.port;
}

async function probe(port: number, path: string): Promise<number> {
  const response = await fetch(`http://127.0.0.1:${port}${path}`);
  await response.arrayBuffer();
  return response.status;
}

for (const language of languages) {
  test(`a ${language} service tells systemd and its probe when it serves and when it stops`, async () => {
    await withTrellisRuntime(async (runtime) => {
      const identity = await runtime.registerService({
        name: `ready-${language}`,
        contract: participants.Provider.participant,
      });
      const notify = new NotifySocket(`@trellis-notify-${crypto.randomUUID()}`);
      const file = join(runtime.workdir, "ready");
      const port = await freePort();
      const service = startService(runtime, language, {
        TRELLIS_IDENTITY_SEED: identity.seed,
        ROLE: "new",
        NOTIFY_SOCKET: notify.path,
        READINESS_FILE: file,
        READINESS_PORT: String(port),
        HOLD_SERVE: "1",
      });
      try {
        // Connected with every handler registered, but not yet serving.
        await runtime.waitFor(() => service.seen("registered"), {
          timeoutMs: 60_000,
        });
        assert.equal(await probe(port, "/live"), 200);
        assert.equal(await probe(port, "/ready"), 503);
        assert.equal(existsSync(file), false);
        assert.equal(notify.find("READY"), undefined, service.output());

        service.child.stdin.write("serve\n");
        const ready = await runtime.waitFor(() => notify.find("READY", "1"), {
          timeoutMs: 30_000,
        });
        assert.equal(await probe(port, "/ready"), 200);
        assert.equal(existsSync(file), true);
        // READY means a request reaches it at once.
        const caller = await runtime.connectClient({
          name: `ready-caller-${language}`,
          contract: apps.Caller.request,
        });
        try {
          assert.deepEqual(await caller.echo({ value: "x" }).orThrow(), {
            value: "new:x",
          });
        } finally {
          await caller.connection.close();
        }

        service.child.kill("SIGTERM");
        const stopping = await runtime.waitFor(
          () => notify.find("STOPPING", "1"),
          { timeoutMs: 10_000 },
        );
        assert(stopping.at >= ready.at);
        const exit = await service.child.exited;
        assert.deepEqual(exit, { code: 0, signal: null }, service.output());
        assert(service.seen("exited"), service.output());
        assert.equal(existsSync(file), false);
        assert.equal(
          notify.received.filter(
            (notification) => "READY" in notification.fields,
          ).length,
          1,
        );
      } finally {
        service.child.kill("SIGKILL");
        notify.close();
      }
    });
  });

  test(`stopping a ${language} instance under load drops no request and hands its work to the instance that serves`, async () => {
    await withTrellisRuntime(async (runtime) => {
      const contract = participants.Provider.participant;
      const first = await runtime.registerService({
        name: `stop-old-${language}`,
        contract,
      });
      const second = await runtime.registerService({
        name: `stop-new-${language}`,
        contract,
      });
      const oldNotify = new NotifySocket(
        `@trellis-notify-${crypto.randomUUID()}`,
      );
      const newNotify = new NotifySocket(
        `@trellis-notify-${crypto.randomUUID()}`,
      );
      const stopTimeoutMs = 3_000;
      const old = startService(runtime, language, {
        TRELLIS_IDENTITY_SEED: first.seed,
        ROLE: "old",
        NOTIFY_SOCKET: oldNotify.path,
        STOP_TIMEOUT_MS: String(stopTimeoutMs),
      });
      let fresh: ReturnType<typeof startService> | undefined;
      const caller = await runtime.connectClient({
        name: `stop-caller-${language}`,
        contract: apps.Caller.request,
      });
      let loading = true;
      const failures: string[] = [];
      let calls = 0;
      try {
        await runtime.waitFor(() => oldNotify.find("READY", "1"), {
          timeoutMs: 60_000,
        });
        await runtime.waitFor(() => old.seen("job-started"));
        const live = await caller.runtimeWatch({}).orThrow();
        assert.deepEqual((await live.next()).value, { value: "old" });
        const liveEnded = live.closed.then((end) => ({ end, at: Date.now() }));
        await caller.publishChanged({ value: "held" }).orThrow();
        await runtime.waitFor(() => old.seen("delivery-started"), {
          timeoutMs: 30_000,
        });

        fresh = startService(runtime, language, {
          TRELLIS_IDENTITY_SEED: second.seed,
          ROLE: "new",
          NOTIFY_SOCKET: newNotify.path,
        });
        await runtime.waitFor(() => newNotify.find("READY", "1"), {
          timeoutMs: 60_000,
        });
        const load = (async () => {
          while (loading) {
            calls++;
            const result = await caller.echo({ value: "load" });
            if (result.isErr()) failures.push(String(result.error));
            await new Promise((resolve) => setTimeout(resolve, 100));
          }
        })();
        // Queue groups pick at random: send until a slow call lands on the old one.
        let slow: Promise<unknown> | undefined;
        while (!old.seen("slow-started")) {
          const attempt = caller.echo({ value: "slow:2000" }).orThrow();
          slow = attempt;
          await new Promise((resolve) => setTimeout(resolve, 50));
        }

        const stoppedAt = Date.now();
        old.child.kill("SIGTERM");
        await runtime.waitFor(() => oldNotify.find("STOPPING", "1"), {
          timeoutMs: 5_000,
        });
        // The request it held finishes and answers.
        assert.deepEqual(await slow, { value: "old:slow:2000" });
        // Its Live stream ends at once, so its caller can open it again; the
        // next opening reaches the instance that serves.
        const ended = await liveEnded;
        assert(ended.at - stoppedAt < 2_000, `Live ended late`);
        assert.notEqual(ended.end?.reason, "complete");
        const reopened = await caller.runtimeWatch({}).orThrow();
        assert.deepEqual((await reopened.next()).value, { value: "new" });
        await reopened.close().orThrow();
        // The job and the delivery it held past the deadline are handed back at
        // once, not after a lease or acknowledgement deadline.
        const job = await runtime.waitFor(() => fresh?.seen("job-started"), {
          timeoutMs: 20_000,
        });
        const delivery = await runtime.waitFor(
          () => fresh?.seen("delivery-started"),
          { timeoutMs: 20_000 },
        );
        for (const handedBack of [job, delivery])
          assert(
            handedBack.at - stoppedAt < stopTimeoutMs + 2_000,
            `handed back ${handedBack.at - stoppedAt} ms after the stop`,
          );
        const exit = await old.child.exited;
        assert.deepEqual(exit, { code: 0, signal: null }, old.output());
        assert(
          Date.now() - stoppedAt < stopTimeoutMs + 8_000,
          `exited ${Date.now() - stoppedAt} ms after the stop\n${old.output()}`,
        );

        // A second of load after the old instance left.
        await new Promise((resolve) => setTimeout(resolve, 1_000));
        loading = false;
        await load;
        assert.deepEqual(failures, []);
        assert(calls > 20, `only ${calls} calls`);
      } finally {
        loading = false;
        old.child.kill("SIGKILL");
        fresh?.child.kill("SIGKILL");
        oldNotify.close();
        newNotify.close();
        await caller.connection.close();
      }
    });
  });
}

test("a typescript job handler that ignores its cancellation is cut off at the stop timeout, and the process exits", async () => {
  await withTrellisRuntime(async (runtime) => {
    const contract = participants.Provider.participant;
    const first = await runtime.registerService({
      name: "stuck-old",
      contract,
    });
    const second = await runtime.registerService({
      name: "stuck-new",
      contract,
    });
    const oldNotify = new NotifySocket(
      `@trellis-notify-${crypto.randomUUID()}`,
    );
    const newNotify = new NotifySocket(
      `@trellis-notify-${crypto.randomUUID()}`,
    );
    const stopTimeoutMs = 2_000;
    const old = startService(runtime, "typescript", {
      TRELLIS_IDENTITY_SEED: first.seed,
      ROLE: "old",
      NOTIFY_SOCKET: oldNotify.path,
      STOP_TIMEOUT_MS: String(stopTimeoutMs),
      IGNORE_JOB_CANCEL: "1",
    });
    let fresh: ReturnType<typeof startService> | undefined;
    try {
      await runtime.waitFor(() => oldNotify.find("READY", "1"), {
        timeoutMs: 60_000,
      });
      await runtime.waitFor(() => old.seen("job-started"), {
        timeoutMs: 30_000,
      });
      fresh = startService(runtime, "typescript", {
        TRELLIS_IDENTITY_SEED: second.seed,
        ROLE: "new",
        NOTIFY_SOCKET: newNotify.path,
      });
      await runtime.waitFor(() => newNotify.find("READY", "1"), {
        timeoutMs: 60_000,
      });

      const stoppedAt = Date.now();
      old.child.kill("SIGTERM");
      // At the deadline the job goes back at once, though its handler never
      // looks at its cancellation, and the other instance runs it.
      const handedBack = await runtime.waitFor(
        () => fresh?.seen("job-started"),
        { timeoutMs: stopTimeoutMs + 10_000 },
      );
      assert(
        handedBack.at - stoppedAt < stopTimeoutMs + 2_000,
        `handed back ${handedBack.at - stoppedAt} ms after the stop\n${old.output()}`,
      );
      // wait() returns, and the process ends although the stuck handler's
      // timer would hold it open for ten minutes.
      const exit = await Promise.race([
        old.child.exited,
        new Promise<"running">((resolve) =>
          setTimeout(() => resolve("running"), stopTimeoutMs + 10_000),
        ),
      ]);
      assert.deepEqual(exit, { code: 0, signal: null }, old.output());
      assert(old.seen("exited"), old.output());
    } finally {
      old.child.kill("SIGKILL");
      fresh?.child.kill("SIGKILL");
      oldNotify.close();
      newNotify.close();
    }
  });
});

test("a stopping typescript instance takes no request on a generation it adopts while it drains", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const contract =
        participants.TransportGrowthOperationProvider.participant;
      const stopping = await runtime.registerService({
        name: "fenced-stopping",
        contract,
      });
      // The stopping instance gets its optional capability only while it
      // drains, and adopts a new generation for it then.
      const capabilityId = "runtime-trellis.transport_growth.v1:growth.extend";
      const grants = await runtime.callAdminRpc("authCapabilityGrantsList", {
        capabilityId,
      });
      assert.equal(grants.items.length, 1);
      const [grant] = grants.items;
      await runtime.callAdminRpc("authCapabilityGrantsRevoke", {
        grantId: grant.grantId,
        expectedRevision: grant.revision,
        idempotencyKey: codecs.ulid.decode(ulid()),
      });
      const serving = await runtime.registerService({
        name: "fenced-serving",
        contract,
      });
      const release = Promise.withResolvers<void>();
      let stopBegan = false;
      let acceptedWhileStopping = 0;
      const connectService = async (role: string, seed: string) => {
        const service = await TrellisService.connect({
          trellisUrl: runtime.trellisUrl,
          participant: contract,
          seed,
          runtime: { stopTimeoutMs: 120_000 },
        }).orThrow();
        let parks = 0;
        await service.handleExtend(() => Result.ok({}));
        await service.handlePark(async () => {
          if (role === "stopping" && stopBegan) acceptedWhileStopping++;
          if (role === "stopping" && ++parks === 1) await release.promise;
          return Result.ok({ value: role });
        });
        const exit = service.wait().catch((error: unknown) => error);
        await service.ready;
        return { service, exit };
      };
      const caller = await runtime.connectClient({
        name: "fenced-caller",
        contract: apps.TransportGrowthCaller.request,
        timeout: 120_000,
      });
      const old = await connectService("stopping", stopping.seed);
      const services = [old.service];
      let held: Promise<unknown> | undefined;
      try {
        services.push((await connectService("serving", serving.seed)).service);
        // Queue groups pick at random: call until one call is held by the
        // instance about to stop.
        for (let attempt = 0; attempt < 50 && !held; attempt++) {
          const answer = caller.park({}).orThrow();
          const settled = await Promise.race([
            answer.then(() => true),
            new Promise<false>((resolve) =>
              setTimeout(() => resolve(false), 200),
            ),
          ]);
          if (!settled) held = answer;
        }
        assert(held, "no call reached the instance about to stop");

        stopBegan = true;
        const stopped = old.service.stop();
        await runtime.callAdminRpc("authCapabilityGrantsGrant", {
          principalId: grant.principalId,
          capabilityId,
          expectedRevision: 0n,
          idempotencyKey: codecs.ulid.decode(ulid()),
        });
        // Its own call needs the grown authority, which its next renewal brings
        // on a new generation: the call succeeds once it has adopted that.
        await runtime.waitFor(
          async () => (await old.service.extend({})).isOk(),
          { timeoutMs: 90_000 },
        );
        const values = await Promise.all(
          Array.from({ length: 20 }, () => caller.park({}).orThrow()),
        );
        assert.deepEqual(
          values.map((answer) => answer.value),
          Array(20).fill("serving"),
        );
        assert.equal(acceptedWhileStopping, 0);

        release.resolve();
        assert.deepEqual(await held, { value: "stopping" });
        await stopped;
        assert.equal(await old.exit, undefined);
      } finally {
        release.resolve();
        await held?.catch(() => undefined);
        await caller.connection.close();
        for (const service of services) await service.stop();
      }
      // Short authorization lifetimes, so the grant arrives with a renewal soon.
    },
    {
      authorization: { contextLifetimeSeconds: 27, allowedClockSkewSeconds: 2 },
    },
  );
});

test("a typescript service serves only once the broker has its durable consumer", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "consumer-ready",
      contract: participants.Provider.participant,
    });
    const nats = await connect({
      servers: runtime.natsUrl,
      authenticator: credsAuthenticator(
        await runtime.natsCredentials("trellis"),
      ),
    });
    const notify = new NotifySocket(`@trellis-notify-${crypto.randomUUID()}`);
    const port = await freePort();
    let service: ReturnType<typeof startService> | undefined;
    try {
      service = startService(runtime, "typescript", {
        TRELLIS_IDENTITY_SEED: identity.seed,
        ROLE: "new",
        NOTIFY_SOCKET: notify.path,
        READINESS_PORT: String(port),
        HOLD_REGISTER: "1",
      });
      await runtime.waitFor(() => service?.seen("connected"), {
        timeoutMs: 60_000,
      });
      // The `changes` consumer is missing at the broker when the service
      // registers its handler.
      const manager = await jetstreamManager(nats);
      let changes:
        | {
            stream: string;
            config: Parameters<typeof manager.consumers.add>[1];
          }
        | undefined;
      const seen: string[] = [];
      for await (const stream of manager.streams.list())
        for await (const consumer of manager.consumers.list(
          stream.config.name,
        )) {
          const filters = [
            consumer.config.filter_subject,
            ...(consumer.config.filter_subjects ?? []),
          ];
          seen.push(`${stream.config.name}/${consumer.name} ${filters}`);
          if (filters.some((subject) => subject?.includes(".Changed")))
            changes = { stream: stream.config.name, config: consumer.config };
        }
      assert(changes, `no consumer for the changes group in ${seen}`);
      assert(changes.config.durable_name);
      await manager.consumers.delete(
        changes.stream,
        changes.config.durable_name,
      );
      service.child.stdin.write("register\n");
      await runtime.waitFor(() => service?.seen("waiting"), {
        timeoutMs: 60_000,
      });
      // Every handler is registered and wait() runs, but a delivery has
      // nowhere to go yet: not ready.
      await new Promise((resolve) => setTimeout(resolve, 2_000));
      assert.equal(notify.find("READY"), undefined, service.output());
      assert.equal(await probe(port, "/ready"), 503);

      await manager.consumers.add(changes.stream, changes.config);
      await runtime.waitFor(() => notify.find("READY", "1"), {
        timeoutMs: 30_000,
      });
      assert.equal(await probe(port, "/ready"), 200);
      const caller = await runtime.connectClient({
        name: "consumer-ready-caller",
        contract: apps.Caller.request,
      });
      try {
        await caller.publishChanged({ value: "after-ready" }).orThrow();
        await runtime.waitFor(() => service?.seen("delivery-finished"), {
          timeoutMs: 30_000,
        });
      } finally {
        await caller.connection.close();
      }
    } finally {
      service?.child.kill("SIGKILL");
      notify.close();
      await nats.close();
    }
  });
});
