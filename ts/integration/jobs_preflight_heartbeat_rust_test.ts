/** Receipt maintenance must cover a stalled real-broker lifecycle preflight. */
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { once } from "node:events";
import { jetstreamManager } from "@nats-io/jetstream";
import { connect, credsAuthenticator } from "@nats-io/transport-node";
import {
  apis,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";
import {
  spawnTracked,
  type TrackedProcess,
  type ProcessExit,
} from "./_support/process.ts";

test("Rust maintains the accepted Jobs receipt during stalled lifecycle preflight", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const contract =
        participants.TransportGrowthOperationProvider.participant;
      const deployment = "jobs-preflight-rust";
      await runtime.contracts.apply({ deployment, contract });
      const instance = await runtime.services.createInstance({
        deployment,
        name: deployment,
        contract,
      });
      const nats = await connect({
        servers: runtime.natsUrl,
        authenticator: credsAuthenticator(
          await readFile(
            `${runtime.workdir}/config/trellis/nats/creds/trellis-auth.creds`,
          ),
        ),
      });
      const manager = await jetstreamManager(nats);
      const gate = runtime.nativeTransportGate();
      const historySubject = apis.jobs.API.actions["rpc:History"].subject;
      const decoder = new TextDecoder();
      const requests: { connectionId: number; subject: string }[] = [];
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
          if (op.subject === historySubject) {
            requests.push({ connectionId: id, subject: op.subject });
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
          )
            terminals.push({ connectionId: id, subject: op.subject });
        }
        return clientFrame(id, op, raw);
      };
      const serverFrame = gate.onServerFrame.bind(gate);
      gate.onServerFrame = (id, op, raw) => {
        if (
          op.kind === "msg" &&
          op.subject.startsWith("trellis.work.") &&
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
      const preflight = gate.armResponseHold(historySubject);
      let child: TrackedProcess | undefined;
      let status: Promise<ProcessExit> | undefined;
      let drain: Promise<void> | undefined;
      let exited = false;
      const lines: string[] = [];
      const send = async (command: string) => {
        assert(child?.stdin);
        if (!child.stdin.write(`${command}\n`))
          await once(child.stdin, "drain");
      };
      const waitFor = async (marker: string) => {
        await runtime.waitFor(
          () => {
            const error = lines.find((line) => /_ERROR /.test(line));
            if (error !== undefined) throw new Error(error);
            if (lines.includes(marker)) return true;
            if (exited) throw new Error(`fixture exited: ${lines.join("\n")}`);
            return undefined;
          },
          { timeoutMs: 30_000, intervalMs: 25 },
        );
      };
      try {
        child = spawnTracked(
          "setsid",
          [
            ...rustFixtureArgv("transport_growth_operation"),
            "--jobs-preflight",
          ],
          {
            env: {
              ...process.env,
              TRELLIS_URL: runtime.oauthIssuer,
              TRELLIS_CA_CERTIFICATE: runtime.caCertificate,
              TRELLIS_IDENTITY_SEED: instance.seed,
            },
            stdio: ["pipe", "pipe", "inherit"],
          },
        );
        status = child.exited.then((value) => {
          exited = true;
          return value;
        });
        assert(child.stdout);
        child.stdout.setEncoding("utf8");
        drain = (async () => {
          let pending = "";
          for await (const chunk of child!.stdout!) {
            pending += chunk;
            const complete = pending.split("\n");
            pending = complete.pop() ?? "";
            lines.push(...complete.map((line) => line.trim()));
          }
        })();
        let held: Awaited<typeof preflight.held> | undefined;
        let holdError: unknown;
        preflight.held.then(
          (value) => {
            held = value;
          },
          (cause) => {
            holdError = cause;
          },
        );
        await runtime.waitFor(
          () => {
            if (holdError !== undefined) throw holdError;
            if (exited) throw new Error(lines.join("\n"));
            return held;
          },
          { timeoutMs: 30_000, intervalMs: 25 },
        );
        assert(held !== undefined);
        const heldAt = performance.now();
        assert.deepEqual(
          deliveries.length,
          1,
          "hold must intercept the initial receipt before redelivery",
        );
        const receipt = deliveries[0];
        const job = JSON.parse(receipt.body) as {
          jobId: string;
          payload: { value: string };
        };
        assert.deepEqual(job.payload.value, "initial");
        assert.deepEqual(
          held.connectionId,
          receipt.connectionId,
          "lifecycle read must use the actual receiving connection",
        );
        assert(requests.length > 0, "hold intercepts a real lifecycle lookup");
        assert.deepEqual(requests[0].connectionId, receipt.connectionId);
        assert.deepEqual(Number(receipt.reply.split(".").at(-5)), 1);
        assert(
          !lines.some((line) => line.startsWith("JOB_STARTED ")),
          "the first receipt is stalled before handler entry",
        );
        const queues = [];
        for await (const stream of manager.streams.list()) {
          for await (const consumer of manager.consumers.list(
            stream.config.name,
          )) {
            if (consumer.config.filter_subject === receipt.subject) {
              queues.push({ stream: stream.config.name, consumer });
            }
          }
        }
        assert.deepEqual(queues.length, 1);
        const { stream, consumer } = queues[0];
        const info = () => manager.consumers.info(stream, consumer.name);
        const ackWaitMs = consumer.config.ack_wait! / 1_000_000;
        // Poll actual broker INFO while the one reply is held. Three AckWait
        // windows exceed the required two. The authored queue's short retry
        // interval keeps this inside the unchanged RPC request deadline.
        const during = await runtime.waitFor(
          async () => {
            const snapshot = await info();
            return performance.now() - heldAt > 3 * ackWaitMs
              ? snapshot
              : undefined;
          },
          { timeoutMs: 2_000, intervalMs: 25 },
        );
        const heldMs = performance.now() - heldAt;
        const heldDeliveries = deliveries
          .filter((delivery) => delivery.subject === receipt.subject)
          .slice();
        const progress = acks.filter(
          (ack) =>
            ack.subject === receipt.reply &&
            ack.body === "+WPI" &&
            ack.at >= heldAt,
        );
        const heldStarts = lines
          .filter((line) => line.startsWith("JOB_STARTED "))
          .slice();
        console.log(
          `PREFLIGHT_HELD ${JSON.stringify({
            connectionId: receipt.connectionId,
            query: requests[0].subject,
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
          })}`,
        );
        assert.deepEqual(
          heldDeliveries.length,
          1,
          "stalled lifecycle preflight must not redeliver the accepted job",
        );
        assert.deepEqual(during.delivered.consumer_seq, 1);
        assert.deepEqual(during.num_redelivered, 0);
        assert(
          progress.length >= 2,
          "the initial accepted receipt must emit progress ACKs while its lifecycle reply is withheld",
        );
        assert(
          progress.every((ack) => ack.connectionId === receipt.connectionId),
        );
        assert.deepEqual(
          heldStarts,
          [],
          "no handler may pass the held preflight",
        );
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
        await runtime.waitFor(() => (exited ? true : undefined));
        assert.equal((await status).code, 0, lines.join("\n"));
        console.log(
          `PREFLIGHT_SETTLED ${JSON.stringify({
            handlers: lines.filter((line) => /^JOB_(STARTED|DONE) /.test(line)),
            terminals,
            acks: acks.filter((ack) => ack.body !== "+WPI"),
            broker: {
              delivered: settled.delivered,
              ack_floor: settled.ack_floor,
              num_ack_pending: settled.num_ack_pending,
              num_pending: settled.num_pending,
            },
          })}`,
        );
        assert.deepEqual(
          lines.filter((line) => line.startsWith("JOB_STARTED ")),
          ["JOB_STARTED 1 initial"],
        );
        assert.deepEqual(
          lines.filter((line) => line.startsWith("JOB_DONE ")),
          ["JOB_DONE 1 initial"],
        );
        assert.deepEqual(
          terminals
            .filter((terminal) =>
              terminal.subject.endsWith(`.${job.jobId}.completed`),
            )
            .map((terminal) => terminal.connectionId),
          [receipt.connectionId],
        );
        assert.deepEqual(
          acks
            .filter(
              (ack) =>
                ack.subject === receipt.reply &&
                (ack.body === "" || ack.body === "+ACK"),
            )
            .map((ack) => ack.connectionId),
          [receipt.connectionId],
        );
        assert.deepEqual(settled.delivered.consumer_seq, 1);
        assert.deepEqual(settled.ack_floor.consumer_seq, 1);
        assert.deepEqual(settled.num_ack_pending, 0);
        assert.deepEqual(settled.num_pending, 0);
      } finally {
        try {
          await preflight.release();
        } finally {
          child?.stdin?.end();
          if (child?.pid !== undefined && !exited) {
            try {
              process.kill(-child.pid, "SIGKILL");
            } catch {
              /* already exited */
            }
          }
          await Promise.allSettled([status, drain]);
          gate.onClientFrame = clientFrame;
          gate.onServerFrame = serverFrame;
          await nats.close();
        }
      }
    },
    { interruptibleNativeProxy: true },
  );
});
