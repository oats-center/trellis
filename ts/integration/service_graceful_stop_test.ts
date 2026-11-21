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
import type { NatsConnection } from "@nats-io/nats-core";
import { connect, credsAuthenticator } from "@nats-io/transport-node";
import { Result } from "@oatscenter/trellis";
import { codecs } from "@oatscenter/trellis/generated";
import { TrellisService } from "@oatscenter/trellis/service";
import type { TrellisTestRuntime } from "@oatscenter/trellis-testkit";
import { ulid } from "ulid";
import {
  apis,
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { readRuntimeServiceAttachments } from "./_support/broker_inventory.ts";
import { NotifySocket } from "./_support/notify_socket.ts";
import { spawnTracked } from "./_support/process.ts";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";

type Line = {
  event: string;
  role: string;
  at: number;
  value?: string;
  error?: string;
};

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

/**
 * Deletes the first broker consumer whose filter subject contains `fragment`
 * and returns a function that adds it back.
 */
async function deleteConsumer(
  nats: NatsConnection,
  fragment: string,
): Promise<() => Promise<unknown>> {
  const manager = await jetstreamManager(nats);
  const seen: string[] = [];
  for await (const stream of manager.streams.list())
    for await (const consumer of manager.consumers.list(stream.config.name)) {
      const filters = [
        consumer.config.filter_subject,
        ...(consumer.config.filter_subjects ?? []),
      ];
      seen.push(`${stream.config.name}/${consumer.name} ${filters}`);
      if (!filters.some((subject) => subject?.includes(fragment))) continue;
      const { config } = consumer;
      assert(config.durable_name);
      await manager.consumers.delete(stream.config.name, config.durable_name);
      return () => manager.consumers.add(stream.config.name, config);
    }
  assert.fail(`no consumer for ${fragment} in ${seen}`);
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
        // Queue groups pick at random: send one slow call at a time until one
        // lands on the old instance. The new one answers at once, so a call it
        // took ends before the next is sent, and the call the old one holds is
        // the only one outstanding when it reports holding it.
        let slow: Promise<unknown> | undefined;
        while (!slow) {
          const attempt = caller.echo({ value: "slow:2000" }).orThrow();
          let answered = false;
          attempt.then(
            () => (answered = true),
            () => (answered = true),
          );
          while (!answered && !old.seen("slow-started")) {
            await new Promise((resolve) => setTimeout(resolve, 10));
          }
          if (old.seen("slow-started")) slow = attempt;
          else assert.deepEqual(await attempt, { value: "new:slow:2000" });
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

  test(`a stopping ${language} instance hands back at once a job it pulls as it stops`, async () => {
    await withTrellisRuntime(async (runtime) => {
      const contract = participants.Provider.participant;
      const first = await runtime.registerService({
        name: `late-old-${language}`,
        contract,
      });
      const second = await runtime.registerService({
        name: `late-new-${language}`,
        contract,
      });
      const notify = new NotifySocket(`@trellis-notify-${crypto.randomUUID()}`);
      const stopping = startService(runtime, language, {
        TRELLIS_IDENTITY_SEED: first.seed,
        ROLE: "new",
        NOTIFY_SOCKET: notify.path,
        STOP_TIMEOUT_MS: "5000",
      });
      let next: ReturnType<typeof startService> | undefined;
      const caller = await runtime.connectClient({
        name: `late-caller-${language}`,
        contract: apps.Caller.request,
      });
      try {
        await runtime.waitFor(() => notify.find("READY", "1"), {
          timeoutMs: 60_000,
        });
        // A request it holds keeps it draining while its idle job worker
        // still waits on a pull.
        const held = caller.echo({ value: "hold:2000" }).orThrow();
        await runtime.waitFor(() => stopping.seen("hold-started"));
        stopping.child.kill("SIGTERM");
        await runtime.waitFor(() => notify.find("STOPPING", "1"));
        stopping.child.stdin.write("create:late\n");
        await runtime.waitFor(() => stopping.seen("created"));
        assert.deepEqual(await held, { value: "new:hold:2000" });
        assert.deepEqual(await stopping.child.exited, {
          code: 0,
          signal: null,
        });
        // The stopping instance gave the job back rather than run it, and the
        // next instance runs it.
        assert.equal(
          stopping.seen("job-started"),
          undefined,
          stopping.output(),
        );
        next = startService(runtime, language, {
          TRELLIS_IDENTITY_SEED: second.seed,
          ROLE: "new",
        });
        const job = await runtime.waitFor(() => next?.seen("job-started"), {
          timeoutMs: 60_000,
        });
        assert.equal(job.value, "late");
      } finally {
        stopping.child.kill("SIGKILL");
        next?.child.kill("SIGKILL");
        notify.close();
        await caller.connection.close();
      }
    });
  });

  test(`a ${language} service takes no job or delivery before it serves`, async () => {
    await withTrellisRuntime(async (runtime) => {
      const identity = await runtime.registerService({
        name: `early-${language}`,
        contract: participants.Provider.participant,
      });
      const service = startService(runtime, language, {
        TRELLIS_IDENTITY_SEED: identity.seed,
        ROLE: "new",
        HOLD_SERVE: "1",
      });
      const caller = await runtime.connectClient({
        name: `early-caller-${language}`,
        contract: apps.Caller.request,
      });
      try {
        // Registered, so its worker and consumer exist, but not yet serving.
        await runtime.waitFor(() => service.seen("registered"), {
          timeoutMs: 60_000,
        });
        service.child.stdin.write("create:early\n");
        await runtime.waitFor(() => service.seen("created"));
        await caller.publishChanged({ value: "early" }).orThrow();
        // Both wait for it. One that takes work early starts it well within this.
        await new Promise((resolve) => setTimeout(resolve, 1_500));
        assert.equal(service.seen("job-started"), undefined, service.output());
        assert.equal(service.seen("delivery-started"), undefined);

        const servedAt = Date.now();
        service.child.stdin.write("serve\n");
        const job = await runtime.waitFor(() => service.seen("job-started"), {
          timeoutMs: 30_000,
        });
        const delivery = await runtime.waitFor(
          () => service.seen("delivery-started"),
          { timeoutMs: 30_000 },
        );
        assert.equal(job.value, "early");
        assert.equal(delivery.value, "early");
        assert(job.at >= servedAt && delivery.at >= servedAt);
      } finally {
        service.child.kill("SIGKILL");
        await caller.connection.close();
      }
    });
  });

  test(`a ${language} service serves only once the broker has its durable consumer`, async () => {
    await withTrellisRuntime(async (runtime) => {
      const identity = await runtime.registerService({
        name: `consumer-ready-${language}`,
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
        service = startService(runtime, language, {
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
        const restore = await deleteConsumer(nats, ".Changed");
        service.child.stdin.write("register\n");
        await runtime.waitFor(() => service?.seen("waiting"), {
          timeoutMs: 60_000,
        });
        // Every handler is registered and wait() runs, but a delivery has
        // nowhere to go yet: not ready.
        await new Promise((resolve) => setTimeout(resolve, 2_000));
        assert.equal(notify.find("READY"), undefined, service.output());
        assert.equal(await probe(port, "/ready"), 503);

        await restore();
        await runtime.waitFor(() => notify.find("READY", "1"), {
          timeoutMs: 30_000,
        });
        assert.equal(await probe(port, "/ready"), 200);
        const caller = await runtime.connectClient({
          name: `consumer-ready-caller-${language}`,
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

  test(`a ${language} job handler that ignores its cancellation is cut off at the stop timeout, and the process exits`, async () => {
    await withTrellisRuntime(async (runtime) => {
      const contract = participants.Provider.participant;
      const first = await runtime.registerService({
        name: `stuck-old-${language}`,
        contract,
      });
      const second = await runtime.registerService({
        name: `stuck-new-${language}`,
        contract,
      });
      const oldNotify = new NotifySocket(
        `@trellis-notify-${crypto.randomUUID()}`,
      );
      const newNotify = new NotifySocket(
        `@trellis-notify-${crypto.randomUUID()}`,
      );
      const stopTimeoutMs = 2_000;
      const old = startService(runtime, language, {
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
        fresh = startService(runtime, language, {
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

  test(`a ${language} job whose lease is lost while its handler ignores cancellation does not hold up the stop`, async () => {
    await withTrellisRuntime(async (runtime) => {
      const contract = participants.Provider.participant;
      const identity = await runtime.registerService({
        name: `lost-${language}`,
        contract,
      });
      const notify = new NotifySocket(`@trellis-notify-${crypto.randomUUID()}`);
      const stopTimeoutMs = 2_000;
      const service = startService(runtime, language, {
        TRELLIS_IDENTITY_SEED: identity.seed,
        ROLE: "old",
        NOTIFY_SOCKET: notify.path,
        STOP_TIMEOUT_MS: String(stopTimeoutMs),
        IGNORE_JOB_CANCEL: "1",
      });
      try {
        await runtime.waitFor(() => notify.find("READY", "1"), {
          timeoutMs: 60_000,
        });
        await runtime.waitFor(() => service.seen("job-started"), {
          timeoutMs: 30_000,
        });
        // Dropping the connection the job arrived on loses its lease; the
        // handler never notices and keeps running on a new connection.
        const [lost] = await readRuntimeServiceAttachments(
          runtime,
          contract.id,
        );
        assert(lost);
        await runtime.callAdminRpc("authConnectionsKick", {
          attachmentId: lost.attachmentId,
          idempotencyKey: codecs.ulid.decode(ulid()),
        });
        await runtime.waitFor(
          async () =>
            (await readRuntimeServiceAttachments(runtime, contract.id)).some(
              (attachment) => attachment.attachmentId !== lost.attachmentId,
            ),
          { timeoutMs: 30_000 },
        );

        const stoppedAt = Date.now();
        service.child.kill("SIGTERM");
        // The stop ends at its deadline, without waiting on the stuck handler,
        // and the process exits.
        const exit = await Promise.race([
          service.child.exited,
          new Promise<"running">((resolve) =>
            setTimeout(() => resolve("running"), stopTimeoutMs + 15_000),
          ),
        ]);
        assert.deepEqual(exit, { code: 0, signal: null }, service.output());
        const exited = service.seen("exited");
        assert(exited, service.output());
        assert(
          exited.at - stoppedAt < stopTimeoutMs + 3_000,
          `stopped ${exited.at - stoppedAt} ms after the signal`,
        );
      } finally {
        service.child.kill("SIGKILL");
        notify.close();
      }
    });
  });
}

test("a typescript service whose connection closes before its durable consumer attaches stops with an error and never serves", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "closed-early",
      contract: participants.Provider.participant,
    });
    const nats = await connect({
      servers: runtime.natsUrl,
      authenticator: credsAuthenticator(
        await runtime.natsCredentials("trellis"),
      ),
    });
    const notify = new NotifySocket(`@trellis-notify-${crypto.randomUUID()}`);
    let service: ReturnType<typeof startService> | undefined;
    try {
      service = startService(runtime, "typescript", {
        TRELLIS_IDENTITY_SEED: identity.seed,
        ROLE: "new",
        NOTIFY_SOCKET: notify.path,
        HOLD_REGISTER: "1",
      });
      await runtime.waitFor(() => service?.seen("connected"), {
        timeoutMs: 60_000,
      });
      // wait() waits for a consumer the broker does not have when its
      // connection closes.
      await deleteConsumer(nats, ".Changed");
      service.child.stdin.write("register\n");
      await runtime.waitFor(() => service?.seen("waiting"), {
        timeoutMs: 60_000,
      });
      service.child.stdin.write("close\n");
      const failed = await runtime.waitFor(() => service?.seen("failed"), {
        timeoutMs: 15_000,
      });
      assert.match(failed.error ?? "", /closed/, service.output());
      assert.deepEqual(await service.child.exited, { code: 0, signal: null });
      assert.equal(notify.find("READY"), undefined, service.output());
    } finally {
      service?.child.kill("SIGKILL");
      notify.close();
      await nats.close();
    }
  });
});

test("a typescript service whose stop fails after a signal reports it through wait() and exits on its own", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "failing-stop",
      contract: participants.Provider.participant,
    });
    const nats = await connect({
      servers: runtime.natsUrl,
      authenticator: credsAuthenticator(
        await runtime.natsCredentials("trellis"),
      ),
    });
    let service: ReturnType<typeof startService> | undefined;
    try {
      service = startService(runtime, "typescript", {
        ROLE: "new",
        TRELLIS_IDENTITY_SEED: identity.seed,
      });
      await runtime.waitFor(() => service?.seen("ready"), {
        timeoutMs: 60_000,
      });
      // Its job queue loses its broker consumer while it serves, so stopping
      // its job workers fails.
      await deleteConsumer(nats, "handoffWork");
      service.child.kill("SIGTERM");
      const failed = await runtime.waitFor(() => service?.seen("failed"), {
        timeoutMs: 30_000,
      });
      assert.ok(failed.error, service.output());
      // The process ends on its own once wait() has reported the error: no
      // unhandled rejection ends it first.
      assert.deepEqual(
        await service.child.exited,
        { code: 0, signal: null },
        service.output(),
      );
      assert.match(service.output(), /Stopping after the signal failed/);
    } finally {
      service?.child.kill("SIGKILL");
      await nats.close();
    }
  });
});

// The stopping instance gets its optional capability around the stop and
// adopts a new generation for it: either after the stop began, or while that
// generation's intake is still being installed as the stop begins.
for (const adoption of ["after", "during"] as const) {
  const when =
    adoption === "after"
      ? "adopts while it drains"
      : "was still installing when the stop began";
  test(`a stopping typescript instance takes no request or Live session on a generation it ${when}`, async () => {
    await withTrellisRuntime(
      async (runtime) => {
        const contract =
          participants.TransportGrowthOperationProvider.participant;
        const stopping = await runtime.registerService({
          name: "fenced-stopping",
          contract,
        });
        const capabilityId =
          "localhost/runtime-trellis.transport_growth.v1:growth.extend";
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
          await service.handleProgress(async ({ emit }) => {
            if (role === "stopping" && stopBegan) acceptedWhileStopping++;
            await emit({ value: role });
          });
          const exit = service.wait().catch((error: unknown) => error);
          await service.ready;
          return { service, exit };
        };
        const caller = await runtime.connectClient({
          name: "fenced-caller",
          contract: apps.RpcDistributionCaller.request,
          timeout: 120_000,
        });
        const gate = runtime.nativeTransportGate();
        const old = await connectService("stopping", stopping.seed);
        const services = [old.service];
        let held: Promise<unknown> | undefined;
        try {
          services.push(
            (await connectService("serving", serving.seed)).service,
          );
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

          const grow = () =>
            runtime.callAdminRpc("authCapabilityGrantsGrant", {
              principalId: grant.principalId,
              capabilityId,
              expectedRevision: 0n,
              idempotencyKey: codecs.ulid.decode(ulid()),
            });
          let stopped: Promise<void>;
          if (adoption === "after") {
            stopBegan = true;
            stopped = old.service.stop();
            await grow();
            // Its own call needs the grown authority, which its next renewal
            // brings on a new generation: the call succeeds once it has
            // adopted that.
            await runtime.waitFor(
              async () => (await old.service.extend({})).isOk(),
              { timeoutMs: 90_000 },
            );
          } else {
            // The new generation's routes reach the broker, its Live open
            // route too, but the flush that ends its Live install is held: the
            // stop begins mid-install.
            gate.arm(
              apis.transport_growth.API.actions["live:Progress"].subject,
            );
            let installing: number | undefined;
            void gate.barrierHeld().then((id) => (installing = id));
            await grow();
            await runtime.waitFor(() => installing, { timeoutMs: 90_000 });
            stopBegan = true;
            stopped = old.service.stop();
          }
          const values = await Promise.all(
            Array.from({ length: 20 }, () => caller.park({}).orThrow()),
          );
          assert.deepEqual(
            values.map((answer) => answer.value),
            Array(20).fill("serving"),
          );
          // One at a time: a provider admits only a few concurrent sessions
          // per caller.
          const observed: string[][] = [];
          for (let open = 0; open < 20; open++) {
            const observation = await caller.progress({}).orThrow();
            const frames: string[] = [];
            try {
              for await (const frame of observation) frames.push(frame.value);
            } finally {
              await observation.close().orThrow();
            }
            observed.push(frames);
          }
          assert.deepEqual(observed, Array(20).fill(["serving"]));
          assert.equal(acceptedWhileStopping, 0);

          await gate.release();
          release.resolve();
          assert.deepEqual(await held, { value: "stopping" });
          await stopped;
          assert.equal(await old.exit, undefined);
        } finally {
          await gate.release();
          release.resolve();
          await held?.catch(() => undefined);
          await caller.connection.close();
          for (const service of services) await service.stop();
        }
      },
      {
        interruptibleNativeProxy: true,
        // Short authorization lifetimes, so the grant arrives with a renewal
        // soon.
        authorization: {
          contextLifetimeSeconds: 27,
          allowedClockSkewSeconds: 2,
        },
      },
    );
  });
}

test("a stopping typescript instance ends at once a Live session it accepted on a generation still installing", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const contract =
        participants.TransportGrowthOperationProvider.participant;
      const identity = await runtime.registerService({
        name: "installing-live",
        contract,
      });
      const capabilityId =
        "localhost/runtime-trellis.transport_growth.v1:growth.extend";
      const grants = await runtime.callAdminRpc("authCapabilityGrantsList", {
        capabilityId,
      });
      const [grant] = grants.items;
      assert(grant);
      await runtime.callAdminRpc("authCapabilityGrantsRevoke", {
        grantId: grant.grantId,
        expectedRevision: grant.revision,
        idempotencyKey: codecs.ulid.decode(ulid()),
      });
      const service = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: contract,
        seed: identity.seed,
        runtime: { stopTimeoutMs: 120_000 },
      }).orThrow();
      const sessionsEnded = Promise.withResolvers<void>();
      let stopBegan = false;
      let handled = 0;
      await service.handleExtend(() => Result.ok({}));
      await service.handlePark(() => Result.ok({ value: "park" }));
      // Each session stays open until the provider ends it.
      await service.handleProgress(async ({ emit, signal }) => {
        if (stopBegan) handled++;
        await emit({ value: "open" });
        await Promise.race([
          new Promise((resolve) => signal.addEventListener("abort", resolve)),
          sessionsEnded.promise,
        ]);
      });
      const exit = service.wait().catch((error: unknown) => error);
      await service.ready;
      const caller = await runtime.connectClient({
        name: "installing-live-caller",
        contract: apps.RpcDistributionCaller.request,
        timeout: 120_000,
      });
      const gate = runtime.nativeTransportGate();
      const subject =
        apis.transport_growth.API.actions["live:Progress"].subject;
      try {
        // The grant brings a new generation whose Live open route reaches the
        // broker while the flush that ends its install is held.
        gate.arm(subject);
        let installing: number | undefined;
        void gate.barrierHeld().then((id) => (installing = id));
        await runtime.callAdminRpc("authCapabilityGrantsGrant", {
          principalId: grant.principalId,
          capabilityId,
          expectedRevision: 0n,
          idempotencyKey: codecs.ulid.decode(ulid()),
        });
        const connection = await runtime.waitFor(() => installing, {
          timeoutMs: 90_000,
        });
        const deliveredToInstalling = () =>
          gate
            .connection(connection)
            ?.deliveries.filter((delivery) => delivery.subject === subject)
            .length ?? 0;
        // Openings go to either generation: open one at a time until the
        // installing generation accepts one.
        type Observation = Awaited<
          ReturnType<ReturnType<typeof caller.progress>["orThrow"]>
        >;
        let accepted: Observation | undefined;
        for (let attempt = 0; attempt < 40 && !accepted; attempt++) {
          const before = deliveredToInstalling();
          const observation = await caller.progress({}).orThrow();
          if (deliveredToInstalling() > before) accepted = observation;
          else {
            assert.deepEqual((await observation.next()).value, {
              value: "open",
            });
            await observation.close().orThrow();
          }
        }
        assert(accepted, "no Live session reached the installing generation");
        // Its caller asks it to activate, which that generation can do only
        // once its install ends. The request reaching it shows the caller
        // already listens for the session's frames. Every generation of the
        // instance hears each session's controls, so the request is the one
        // naming the session whose frames the caller now subscribes to.
        const dataSubscriptions = () =>
          gate
            .connections()
            .flatMap((observed) => observed.subs)
            .filter((sub) => sub.subject.startsWith("live.v1.data."));
        const earlier = dataSubscriptions().length;
        const first = accepted.next().then(
          (next) => next,
          (error: unknown) => error,
        );
        await runtime.waitFor(
          () => {
            const session = dataSubscriptions()
              .slice(earlier)
              .at(0)
              ?.subject.split(".")
              .at(-1);
            return (
              session !== undefined &&
              gate
                .connection(connection)
                ?.deliveries.some(
                  (delivery) =>
                    delivery.subject.startsWith(`${subject}.observe.`) &&
                    delivery.subject.endsWith(`.${session}`),
                )
            );
          },
          { timeoutMs: 10_000 },
        );

        const stoppedAt = Date.now();
        stopBegan = true;
        const stopped = service.stop();
        // The session ends as the stop begins, while that generation's
        // install, and so its connection, is still held.
        const end = await Promise.race([
          accepted.closed,
          new Promise<"open">((resolve) =>
            setTimeout(() => resolve("open"), 10_000),
          ),
        ]);
        assert(end !== "open", "the Live session outlived the stop");
        assert.equal(end.reason, "local_shutdown");
        assert(
          Date.now() - stoppedAt < 5_000,
          `ended ${Date.now() - stoppedAt} ms after the stop`,
        );
        assert.deepEqual(await first, { done: true, value: undefined });
        assert.equal(gate.isClosed(connection), false);
        assert.equal(handled, 0);

        await gate.release();
        await stopped;
        assert.equal(await exit, undefined);
      } finally {
        sessionsEnded.resolve();
        await gate.release();
        await caller.connection.close();
        await service.stop();
      }
    },
    {
      interruptibleNativeProxy: true,
      // Short authorization lifetimes, so the grant arrives with a renewal
      // soon.
      authorization: {
        contextLifetimeSeconds: 27,
        allowedClockSkewSeconds: 2,
      },
    },
  );
});
