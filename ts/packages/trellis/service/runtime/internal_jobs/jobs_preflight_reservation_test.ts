import { AckPolicy, jetstream, jetstreamManager } from "@nats-io/jetstream";
import { deadline } from "@nats-io/nats-core";
import { connect, credsAuthenticator } from "@nats-io/transport-node";
import { assert, assertEquals } from "@std/assert";
import { join } from "@std/path";
import { NativeTransportGate } from "../../../../trellis-testkit/src/native_gate.ts";
import { NatsTestContainer } from "../../../../trellis-testkit/src/nats_container.ts";
import { TcpProxy } from "../../../../trellis-testkit/src/runtime.ts";
import { JobManager } from "./job-manager.ts";
import { startNatsWorkerHostFromBinding } from "./runtime-worker.ts";
import type { JobEvent } from "./types.ts";

Deno.test("Jobs reserves the initial delivery during a slow lifecycle query", async () => {
  const workdir = await Deno.makeTempDir({ prefix: "jobs-preflight-" });
  let nats: NatsTestContainer | undefined;
  let proxy: TcpProxy | undefined;
  let nc: Awaited<ReturnType<typeof connect>> | undefined;
  let host:
    | Awaited<ReturnType<typeof startNatsWorkerHostFromBinding>>
    | undefined;
  let hold: ReturnType<NativeTransportGate["armResponseHold"]> | undefined;
  const handlerRelease = Promise.withResolvers<void>();
  const handlerEntered = Promise.withResolvers<void>();
  let handlers = 0;
  try {
    nats = await NatsTestContainer.start(workdir);
    const gate = new NativeTransportGate();
    proxy = TcpProxy.start(nats.natsUrl, { scheme: "nats", gate });
    nc = await connect({
      servers: proxy.url,
      authenticator: credsAuthenticator(
        await Deno.readFile(
          join(workdir, "nats", nats.manifest.paths.creds.trellisService),
        ),
      ),
    });
    const js = jetstream(nc);
    // Broker observations bypass the held worker inbox, not the worker runtime.
    const jsm = await jetstreamManager(nats.nc);
    const prefix = "trellis.jobs.preflight.work";
    const ackWaitMs = 250;
    const binding = {
      workStream: "JOBS",
      jobs: {
        serviceName: "preflight",
        namespace: "preflight",
        queues: {
          work: {
            queueType: "work",
            publishPrefix: prefix,
            workSubject: `${prefix}.*.created`,
            consumerName: "preflight",
            payload: { schema: "PreflightPayload" },
            maxDeliver: 2,
            backoffMs: [],
            ackWaitMs,
          },
        },
      },
    };
    await jsm.streams.add({
      name: "JOBS",
      subjects: [`${prefix}.>`],
      allow_direct: true,
    });
    await jsm.consumers.add("JOBS", {
      durable_name: "preflight",
      ack_policy: AckPolicy.Explicit,
      ack_wait: ackWaitMs * 1_000_000,
      max_deliver: 2,
      filter_subject: `${prefix}.*.created`,
    });
    const manager = new JobManager({ nc: js, jobs: binding.jobs });
    const job = await manager.create("work", { value: "initial" });
    const created = await jsm.direct.getMessage("JOBS", {
      last_by_subj: `${prefix}.${job.id}.created`,
    });
    assert(created);
    const createdBody = created.string();
    const decode = new TextDecoder();
    const requestSubject = `$JS.API.DIRECT.GET.JOBS.${prefix}.${job.id}.*`;
    const requests: {
      connectionId: number;
      subject: string;
      inbox?: string;
    }[] = [];
    const deliveries: {
      connectionId: number;
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
    const clientFrame = gate.onClientFrame.bind(gate);
    gate.onClientFrame = (connectionId, op, raw) => {
      if (op.kind === "pub") {
        if (op.subject === requestSubject) {
          requests.push({ connectionId, subject: op.subject, inbox: op.reply });
        }
        if (op.subject.startsWith("$JS.ACK.")) {
          const text = decode.decode(raw);
          let body = text.slice(text.indexOf("\r\n") + 2, -2);
          if (op.headers !== undefined) {
            body = body.slice(body.indexOf("\r\n\r\n") + 4);
          }
          acks.push({
            connectionId,
            subject: op.subject,
            body,
            at: performance.now(),
          });
        }
        if (op.subject === `${prefix}.${job.id}.completed`) {
          terminals.push({ connectionId, subject: op.subject });
        }
      }
      return clientFrame(connectionId, op, raw);
    };
    const serverFrame = gate.onServerFrame.bind(gate);
    gate.onServerFrame = (connectionId, op, raw) => {
      if (
        op.kind === "msg" && op.reply?.startsWith("$JS.ACK.JOBS.preflight.")
      ) {
        const body = decode.decode(op.body);
        deliveries.push({
          connectionId,
          reply: op.reply,
          body,
          at: performance.now(),
        });
      }
      return serverFrame(connectionId, op, raw);
    };
    // Select the actual created-event reply, not a fabricated slow callback.
    hold = gate.armResponseHold(
      requestSubject,
      undefined,
      (body) => decode.decode(body) === createdBody,
    );
    host = await startNatsWorkerHostFromBinding(binding, {
      nats: nc,
      manager,
      instanceId: "preflight-worker",
      queueConcurrency: { work: 2 },
      handler: async () => {
        handlers++;
        handlerEntered.resolve();
        await handlerRelease.promise;
        return { reserved: true };
      },
    });
    const held = await deadline(hold.held, 5_000);
    const heldAt = performance.now();
    assertEquals(
      deliveries.length,
      1,
      "intercept initial preflight before redelivery",
    );
    const receipt = deliveries[0];
    assertEquals(held.requestSubject, requestSubject);
    assertEquals(held.connectionId, receipt.connectionId);
    assertEquals(requests.length, 1, "hold the first real lifecycle query");
    assertEquals(requests[0].connectionId, receipt.connectionId);
    assert(requests[0].inbox !== undefined);
    assertEquals(JSON.parse(receipt.body).jobId, job.id);
    assertEquals(Number(receipt.reply.split(".").at(-5)), 1);
    assertEquals(handlers, 0, "initial lifecycle reply precedes handler entry");
    const info = () => jsm.consumers.info("JOBS", "preflight");
    // Poll authoritative broker state across three AckWait periods. This wait
    // measures the withheld-reply interval; it is not a readiness sleep.
    const during = await waitFor(async () => {
      const snapshot = await info();
      return performance.now() - heldAt >= 3 * ackWaitMs ? snapshot : undefined;
    });
    const progress = acks.filter((ack) =>
      ack.subject === receipt.reply && ack.body === "+WPI"
    );
    console.log(
      "PREFLIGHT_HELD",
      JSON.stringify(
        {
          held,
          request: requests[0],
          createdBody,
          heldMs: performance.now() - heldAt,
          ackWaitMs,
          deliveries: deliveries.map(({ connectionId, reply, at }) => ({
            connectionId,
            reply,
            afterInitialReceiptMs: at - receipt.at,
          })),
          progressAcks: progress,
          allAcks: acks,
          handlers,
          broker: {
            delivered: during.delivered,
            num_redelivered: during.num_redelivered,
            num_ack_pending: during.num_ack_pending,
            num_waiting: during.num_waiting,
          },
        },
        null,
        2,
      ),
    );
    assertEquals(
      deliveries.length,
      1,
      "slow initial lifecycle query must not redeliver reserved work into the spare slot",
    );
    assertEquals(during.delivered.consumer_seq, 1);
    assertEquals(during.num_redelivered, 0);
    assertEquals(during.num_ack_pending, 1);
    assertEquals(
      during.num_waiting,
      1,
      "spare persistent slot remains waiting at broker",
    );
    assert(
      progress.length >= 2,
      "initial receipt needs progress ACKs while its real lifecycle reply is held",
    );
    assert(progress.every((ack) => ack.connectionId === receipt.connectionId));
    assertEquals(handlers, 0, "no handler may bypass held initial preflight");
    await hold.release();
    await deadline(handlerEntered.promise, 5_000);
    // Handler return is not the end of receipt ownership: the real final
    // lifecycle publication must be confirmed before the source ACK.
    hold = gate.armResponseHold(
      `${prefix}.${job.id}.completed`,
      receipt.connectionId,
    );
    handlerRelease.resolve();
    await deadline(hold.held, 5_000);
    const finalHeldAt = performance.now();
    const progressBeforeFinalHold = acks.filter((ack) =>
      ack.subject === receipt.reply && ack.body === "+WPI"
    ).length;
    const finalHeld = await waitFor(async () => {
      const snapshot = await info();
      return performance.now() - finalHeldAt >= 3 * ackWaitMs
        ? snapshot
        : undefined;
    });
    assertEquals(
      finalHeld.delivered.consumer_seq,
      1,
      "final publication wait must retain the original receipt",
    );
    assertEquals(finalHeld.num_redelivered, 0);
    assertEquals(finalHeld.num_ack_pending, 1);
    assertEquals(deliveries.length, 1);
    assert(
      acks.filter((ack) => ack.subject === receipt.reply && ack.body === "+WPI")
        .length >= progressBeforeFinalHold + 2,
    );
    assertEquals(
      acks.filter((ack) =>
        ack.subject === receipt.reply &&
        (ack.body === "" || ack.body === "+ACK")
      ),
      [],
      "source must not ACK before final lifecycle publish confirmation",
    );
    await hold.release();
    const settled = await waitFor(async () => {
      const snapshot = await info();
      return snapshot.num_ack_pending === 0 && snapshot.num_pending === 0
        ? snapshot
        : undefined;
    });
    const completed = await jsm.direct.getMessage("JOBS", {
      last_by_subj: `${prefix}.${job.id}.completed`,
    });
    assert(completed);
    const event: JobEvent = JSON.parse(completed.string());
    assertEquals(event.result, { reserved: true });
    assertEquals(handlers, 1);
    assertEquals(terminals.map((terminal) => terminal.connectionId), [
      receipt.connectionId,
    ]);
    assertEquals(
      acks.filter((ack) =>
        ack.subject === receipt.reply &&
        (ack.body === "+ACK" || ack.body === "")
      ).map((ack) => ack.connectionId),
      [receipt.connectionId],
    );
    assertEquals(settled.delivered.consumer_seq, 1);
    assertEquals(settled.ack_floor.consumer_seq, 1);

    await host.stop();
    host = undefined;
    // Stop while the actual next job's first lifecycle reply is held. Keep
    // maintenance alive until the reply settles and shutdown NAKs its receipt.
    const stoppedJob = await manager.create("work", {
      value: "stop-preflight",
    });
    const stoppedCreated = await jsm.direct.getMessage("JOBS", {
      last_by_subj: `${prefix}.${stoppedJob.id}.created`,
    });
    assert(stoppedCreated);
    const stoppedBody = stoppedCreated.string();
    hold = gate.armResponseHold(
      `$JS.API.DIRECT.GET.JOBS.${prefix}.${stoppedJob.id}.*`,
      receipt.connectionId,
      (body) => decode.decode(body) === stoppedBody,
    );
    host = await startNatsWorkerHostFromBinding(binding, {
      nats: nc,
      manager,
      instanceId: "stopping-preflight-worker",
      queueConcurrency: { work: 2 },
      handler: () => {
        handlers++;
        return Promise.resolve({ reserved: true });
      },
    });
    await deadline(hold.held, 5_000);
    const stoppedReceipt = deliveries.find((delivery) =>
      delivery.body === stoppedBody
    );
    assert(stoppedReceipt);
    let stopSettled = false;
    const stop = host.stop().then(() => {
      stopSettled = true;
    });
    const stopHeldAt = performance.now();
    const stopHeld = await waitFor(async () => {
      const snapshot = await info();
      // Account the spare slot's already-issued bounded pull before releasing
      // this receipt; otherwise that stopping slot legitimately receives its NAK.
      return performance.now() - stopHeldAt >= 3 * ackWaitMs &&
          snapshot.num_waiting === 0
        ? snapshot
        : undefined;
    });
    assertEquals(
      stopSettled,
      false,
      "shutdown must await retained preflight and disposition",
    );
    assertEquals(stopHeld.delivered.consumer_seq, 2);
    assertEquals(stopHeld.num_redelivered, 0);
    assertEquals(stopHeld.num_ack_pending, 1);
    assertEquals(
      deliveries.filter((delivery) => delivery.body === stoppedBody).length,
      1,
    );
    assert(
      acks.filter((ack) =>
        ack.subject === stoppedReceipt.reply && ack.body === "+WPI"
      ).length >= 2,
    );
    assertEquals(
      handlers,
      1,
      "shutdown preflight must not enter another handler",
    );
    await hold.release();
    await deadline(stop, 5_000);
    host = undefined;
    await nc.flush();
    assertEquals(
      acks.filter((ack) =>
        ack.subject === stoppedReceipt.reply && ack.body.startsWith("-NAK")
      ).map((ack) => ack.connectionId),
      [stoppedReceipt.connectionId],
    );
    const consumer = await jetstream(nats.nc).consumers.get(
      "JOBS",
      "preflight",
    );
    const redelivery = await consumer.next({ expires: 1_000 });
    assert(
      redelivery,
      "shutdown NAK must leave the unhandled job available at broker",
    );
    assertEquals(JSON.parse(redelivery.string()).jobId, stoppedJob.id);
    assertEquals(redelivery.info.deliveryCount, 2);
    assert(await redelivery.ackAck());
    const stoppedAckCount = acks.filter((ack) =>
      ack.subject === stoppedReceipt.reply
    ).length;
    const stoppedAt = performance.now();
    const stopped = await waitFor(async () => {
      const snapshot = await info();
      return performance.now() - stoppedAt >= 3 * ackWaitMs &&
          snapshot.num_ack_pending === 0 && snapshot.num_waiting === 0
        ? snapshot
        : undefined;
    });
    assertEquals(stopped.num_pending, 0);
    assertEquals(
      acks.filter((ack) => ack.subject === stoppedReceipt.reply).length,
      stoppedAckCount,
      "released receiving session must not retain a progress timer",
    );
    assertEquals(handlers, 1);
    console.log(
      "PREFLIGHT_PUBLICATION_AND_STOP",
      JSON.stringify(
        {
          finalPublication: {
            delivered: finalHeld.delivered,
            num_ack_pending: finalHeld.num_ack_pending,
          },
          shutdown: {
            delivered: stopHeld.delivered,
            num_redelivered: stopHeld.num_redelivered,
            receipt: stoppedReceipt.reply,
            acks: acks.filter((ack) => ack.subject === stoppedReceipt.reply),
            stoppedAckCount,
          },
          settled: {
            num_pending: stopped.num_pending,
            num_ack_pending: stopped.num_ack_pending,
            num_waiting: stopped.num_waiting,
          },
          handlers,
        },
        null,
        2,
      ),
    );
  } finally {
    handlerRelease.resolve();
    await hold?.release();
    await host?.stop();
    await nc?.close();
    proxy?.stop();
    await nats?.stop();
    await Deno.remove(workdir, { recursive: true });
  }
});

async function waitFor<T>(predicate: () => Promise<T | undefined>): Promise<T> {
  const until = performance.now() + 5_000;
  while (performance.now() < until) {
    const result = await predicate();
    if (result !== undefined) return result;
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  throw new Error("preflight broker state did not converge within 5 seconds");
}
