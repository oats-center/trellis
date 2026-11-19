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
import type { TrellisTestRuntime } from "@oatscenter/trellis-testkit";
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
}

test("a rust service takes no job or delivery before it serves", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "early-rust",
      contract: participants.Provider.participant,
    });
    const service = startService(runtime, "rust", {
      TRELLIS_IDENTITY_SEED: identity.seed,
      ROLE: "new",
      HOLD_SERVE: "1",
    });
    const caller = await runtime.connectClient({
      name: "early-caller-rust",
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
