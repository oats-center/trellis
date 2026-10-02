/** Receipt maintenance must cover a stalled real-broker lifecycle preflight. */
import { assert, assertEquals } from "@std/assert";
import { jetstreamManager } from "@nats-io/jetstream";
import { connect, credsAuthenticator } from "@nats-io/transport-node";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";

Deno.test("Rust maintains the accepted Jobs receipt during stalled lifecycle preflight", async () => {
  await withTrellisRuntime(async (runtime) => {
    const contract = participants.TransportGrowthOperationProvider.participant;
    const deployment = "jobs-preflight-rust";
    await runtime.contracts.install({ contract });
    const requested = await runtime.contracts.requestApply({
      deployment,
      contract,
    });
    assert(requested.status === "approval_required");
    if (requested.status !== "approval_required") return;
    await runtime.contracts.approveApply(requested.pendingId, {
      excludeCapabilities: ["runtime-trellis.transport_growth@v1::extend"],
    });
    const instance = await runtime.services.createInstance({
      deployment,
      name: deployment,
      contract,
    });
    const nats = await connect({
      servers: runtime.natsUrl,
      authenticator: credsAuthenticator(
        await Deno.readFile(`${runtime.workdir}/nats/creds/trellis-auth.creds`),
      ),
    });
    const manager = await jetstreamManager(nats);
    const gate = runtime.nativeTransportGate();
    const decoder = new TextDecoder();
    const requests: { connectionId: number; body: string }[] = [];
    const deliveries: {
      connectionId: number;
      subject: string;
      reply: string;
      body: string;
      at: number;
    }[] = [];
    const acks: {
      connectionId: number;
      subject: string;
      body: string;
      at: number;
    }[] = [];
    const terminals: { connectionId: number; subject: string }[] = [];
    // Passive observation of existing proxy frames: always invoke the ordinary
    // gate unchanged. No generated traffic, injected replies, or runtime hooks.
    const clientFrame = gate.onClientFrame.bind(gate);
    gate.onClientFrame = (id, op, raw) => {
      if (op.kind === "pub") {
        const text = decoder.decode(raw);
        let body = text.slice(text.indexOf("\r\n") + 2, -2);
        if (op.headers !== undefined) {
          body = body.slice(body.indexOf("\r\n\r\n") + 4);
        }
        if (op.subject === "$JS.API.STREAM.MSG.GET.JOBS") {
          requests.push({ connectionId: id, body });
        }
        if (op.subject.startsWith("$JS.ACK.")) {
          acks.push({
            connectionId: id,
            subject: op.subject,
            body,
            at: performance.now(),
          });
        }
        if (
          op.subject.startsWith("trellis.jobs.") &&
          op.subject.endsWith(".completed")
        ) terminals.push({ connectionId: id, subject: op.subject });
      }
      return clientFrame(id, op, raw);
    };
    const serverFrame = gate.onServerFrame.bind(gate);
    gate.onServerFrame = (id, op, raw) => {
      if (
        op.kind === "msg" && op.subject.startsWith("trellis.work.") &&
        op.reply !== undefined
      ) {
        deliveries.push({
          connectionId: id,
          subject: op.subject,
          reply: op.reply,
          body: decoder.decode(op.body),
          at: performance.now(),
        });
      }
      return serverFrame(id, op, raw);
    };
    // Arm before launch: initial submission precedes ordinary worker intake.
    // Hold an actual receiving-connection lifecycle read, not a fabricated slow
    // callback. Lookup count and order do not affect the receipt obligation.
    const preflight = gate.armResponseHold("$JS.API.STREAM.MSG.GET.JOBS");
    let child: Deno.ChildProcess | undefined;
    let stdin: WritableStreamDefaultWriter<Uint8Array> | undefined;
    let status: Promise<Deno.CommandStatus> | undefined;
    let drain: Promise<void> | undefined;
    let exited = false;
    const lines: string[] = [];
    const send = async (command: string) => {
      assert(stdin !== undefined);
      await stdin.write(new TextEncoder().encode(`${command}\n`));
    };
    const waitFor = async (marker: string) => {
      await runtime.waitFor(() => {
        const error = lines.find((line) => /_ERROR /.test(line));
        if (error !== undefined) throw new Error(error);
        if (lines.includes(marker)) return true;
        if (exited) throw new Error(`fixture exited: ${lines.join("\n")}`);
        return undefined;
      }, { timeoutMs: 30_000, intervalMs: 25 });
    };
    try {
      child = new Deno.Command("setsid", {
        args: rustFixtureArgv("transport_growth_operation"),
        env: {
          TRELLIS_URL: runtime.trellisUrl,
          TRELLIS_IDENTITY_SEED: instance.seed,
          TRELLIS_JOB_CONCURRENCY: "2",
        },
        stdin: "piped",
        stdout: "piped",
        stderr: "inherit",
      }).spawn();
      status = child.status.then((value) => {
        exited = true;
        return value;
      });
      stdin = child.stdin.getWriter();
      drain = (async () => {
        let pending = "";
        for await (
          const chunk of child!.stdout.pipeThrough(new TextDecoderStream())
        ) {
          pending += chunk;
          const complete = pending.split("\n");
          pending = complete.pop() ?? "";
          lines.push(...complete.map((line) => line.trim()));
        }
      })();
      let held: Awaited<typeof preflight.held> | undefined;
      let holdError: unknown;
      preflight.held.then((value) => {
        held = value;
      }, (cause) => {
        holdError = cause;
      });
      await runtime.waitFor(() => {
        if (holdError !== undefined) throw holdError;
        if (exited) throw new Error(lines.join("\n"));
        return held;
      }, { timeoutMs: 30_000, intervalMs: 25 });
      assert(held !== undefined);
      const heldAt = performance.now();
      assertEquals(
        deliveries.length,
        1,
        "hold must intercept the initial receipt before redelivery",
      );
      const receipt = deliveries[0];
      const job = JSON.parse(receipt.body) as {
        jobId: string;
        payload: { value: string };
      };
      assertEquals(job.payload.value, "initial");
      assertEquals(
        held.connectionId,
        receipt.connectionId,
        "lifecycle read must use the actual receiving connection",
      );
      assert(requests.length > 0, "hold intercepts a real lifecycle lookup");
      const query = JSON.parse(requests[0].body) as { last_by_subj: string };
      assert(
        query.last_by_subj.includes(`.held.${job.jobId}.`),
        requests[0].body,
      );
      assertEquals(requests[0].connectionId, receipt.connectionId);
      assertEquals(Number(receipt.reply.split(".").at(-5)), 1);
      assert(
        !lines.some((line) => line.startsWith("JOB_STARTED ")),
        "the first receipt is stalled before handler entry",
      );
      const queues = [];
      for await (const stream of manager.streams.list()) {
        for await (
          const consumer of manager.consumers.list(stream.config.name)
        ) {
          if (consumer.config.filter_subject === receipt.subject) {
            queues.push({ stream: stream.config.name, consumer });
          }
        }
      }
      assertEquals(queues.length, 1);
      const { stream, consumer } = queues[0];
      const info = () => manager.consumers.info(stream, consumer.name);
      assertEquals(
        consumer.config.ack_wait,
        250_000_000,
        "retain the production fixture's 250ms AckWait",
      );
      const ackWaitMs = consumer.config.ack_wait! / 1_000_000;
      // Poll actual broker INFO while the one reply is held. Three AckWait
      // windows exceed the required two without approaching request timeout.
      const during = await runtime.waitFor(async () => {
        const snapshot = await info();
        return performance.now() - heldAt > 3 * ackWaitMs &&
            (snapshot.delivered.consumer_seq > 1 || snapshot.num_waiting > 0)
          ? snapshot
          : undefined;
      }, { timeoutMs: 2_000, intervalMs: 25 });
      const heldMs = performance.now() - heldAt;
      const heldDeliveries = deliveries.filter((delivery) =>
        delivery.subject === receipt.subject
      ).slice();
      const progress = acks.filter((ack) =>
        ack.subject === receipt.reply && ack.body === "+WPI" && ack.at >= heldAt
      );
      const heldStarts = lines.filter((line) => line.startsWith("JOB_STARTED "))
        .slice();
      console.log(`PREFLIGHT_HELD ${
        JSON.stringify({
          connectionId: receipt.connectionId,
          query,
          heldMs,
          ackWaitMs,
          deliveries: heldDeliveries.map(({ connectionId, reply, at }) => ({
            connectionId,
            reply,
            afterReceiptMs: at - receipt.at,
          })),
          receiptProgressAcks: progress.length,
          progressAcks: acks.filter((ack) => ack.body === "+WPI"),
          handlers: heldStarts,
          broker: {
            delivered: during.delivered,
            num_redelivered: during.num_redelivered,
            num_ack_pending: during.num_ack_pending,
            num_waiting: during.num_waiting,
          },
        })
      }`);
      assertEquals(
        heldDeliveries.length,
        1,
        "stalled lifecycle preflight must not consume the spare slot with a redelivery",
      );
      assertEquals(during.delivered.consumer_seq, 1);
      assertEquals(during.num_redelivered, 0);
      assertEquals(
        during.num_waiting,
        1,
        "the spare slot must be waiting at the broker while preflight is held",
      );
      assert(
        progress.length >= 2,
        "the initial accepted receipt must emit progress ACKs while its lifecycle reply is withheld",
      );
      assert(
        progress.every((ack) => ack.connectionId === receipt.connectionId),
      );
      assertEquals(heldStarts, [], "no handler may pass the held preflight");
      await preflight.release();
      await waitFor("JOB_STARTED 1 initial");
      await send("JOB_RELEASE initial");
      await waitFor("JOB_DONE 1 initial");
      const settled = await runtime.waitFor(async () => {
        const snapshot = await info();
        return snapshot.num_ack_pending === 0 && snapshot.num_pending === 0
          ? snapshot
          : undefined;
      });
      await send("JOB_STATUS initial");
      await waitFor("JOB_STATUS initial Completed 1 held-1");
      await send("EXIT");
      await waitFor("OPERATION_PROVIDER_DONE");
      await runtime.waitFor(() => exited ? true : undefined);
      assert((await status).success, lines.join("\n"));
      console.log(`PREFLIGHT_SETTLED ${
        JSON.stringify({
          handlers: lines.filter((line) => /^JOB_(STARTED|DONE) /.test(line)),
          terminals,
          acks: acks.filter((ack) => ack.body !== "+WPI"),
          broker: {
            delivered: settled.delivered,
            ack_floor: settled.ack_floor,
            num_ack_pending: settled.num_ack_pending,
            num_pending: settled.num_pending,
          },
        })
      }`);
      assertEquals(lines.filter((line) => line.startsWith("JOB_STARTED ")), [
        "JOB_STARTED 1 initial",
      ]);
      assertEquals(lines.filter((line) => line.startsWith("JOB_DONE ")), [
        "JOB_DONE 1 initial",
      ]);
      assertEquals(
        terminals.filter((terminal) =>
          terminal.subject.endsWith(`.held.${job.jobId}.completed`)
        ).map((terminal) => terminal.connectionId),
        [receipt.connectionId],
      );
      assertEquals(
        acks.filter((ack) =>
          ack.subject === receipt.reply &&
          (ack.body === "" || ack.body === "+ACK")
        ).map((ack) => ack.connectionId),
        [receipt.connectionId],
      );
      assertEquals(settled.delivered.consumer_seq, 1);
      assertEquals(settled.ack_floor.consumer_seq, 1);
      assertEquals(settled.num_ack_pending, 0);
      assertEquals(settled.num_pending, 0);
    } finally {
      try {
        await preflight.release();
      } finally {
        await stdin?.close().catch(() => undefined);
        if (child !== undefined && !exited) {
          try {
            Deno.kill(-child.pid, "SIGKILL");
          } catch { /* already exited */ }
        }
        await Promise.allSettled([status, drain]);
        gate.onClientFrame = clientFrame;
        gate.onServerFrame = serverFrame;
        await nats.close();
      }
    }
  }, { interruptibleNativeProxy: true });
});
