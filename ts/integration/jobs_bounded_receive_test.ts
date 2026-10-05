import { AckPolicy, jetstream, jetstreamManager } from "@nats-io/jetstream";
import { assert, assertEquals, assertRejects } from "@std/assert";
import { connect, credsAuthenticator } from "@nats-io/transport-node";
import type { NatsConnection } from "@nats-io/nats-core";
import { join } from "@std/path";
import { startTrellisRuntime } from "./_support/runtime.ts";
import type { TrellisTestRuntime } from "@oatscenter/trellis-testkit";
import { JobManager } from "../packages/trellis/service/runtime/internal_jobs/job-manager.ts";
import { ActiveJobCancellationRegistry } from "../packages/trellis/service/runtime/internal_jobs/cancellation-registry.ts";
import {
  type JobReceivingSession,
  startNatsWorkerHostFromBinding,
  startQueueWorkerLoop,
  toWorkerConsumer,
} from "../packages/trellis/service/runtime/internal_jobs/runtime-worker.ts";

// Real component boundary: case-owned authenticated JetStream, the production
// native consumer conversion, JobManager publication and handler lifecycle.
// This does not claim generation migration or Trellis resource provisioning.
Deno.test("Jobs slots bound broker deliveries and account outstanding pulls on stop", async () => {
  const workdir = await Deno.makeTempDir({ prefix: "jobs-bounded-receive-" });
  let nats: TrellisTestRuntime | undefined;
  let replacement: NatsConnection | undefined;
  let receivingConnection: NatsConnection | undefined;
  let slot: Awaited<ReturnType<typeof startQueueWorkerLoop>> | undefined;
  const releaseSessionHandler = Promise.withResolvers<void>();
  let host:
    | Awaited<ReturnType<typeof startNatsWorkerHostFromBinding>>
    | undefined;
  const release = Promise.withResolvers<void>();
  try {
    nats = await startTrellisRuntime();
    const nc = await nats.connectNats();
    const js = jetstream(nc);
    const jsm = await jetstreamManager(nc);
    const prefix = "adapter.jobs.bounded.refresh";
    const binding = {
      workStream: "ADAPTER_JOBS",
      jobs: {
        serviceName: "bounded",
        namespace: "bounded",
        queues: {
          refresh: {
            queueType: "refresh",
            publishPrefix: prefix,
            workSubject: `${prefix}.*.created`,
            consumerName: "bounded-refresh",
            payload: { schema: "RefreshPayload" },
            maxDeliver: 5,
            backoffMs: [],
            ackWaitMs: 30_000,
          },
        },
      },
    };
    await jsm.streams.add({
      name: "ADAPTER_JOBS",
      subjects: [`${prefix}.>`],
      allow_direct: true,
    });
    await jsm.consumers.add("ADAPTER_JOBS", {
      durable_name: "bounded-refresh",
      ack_policy: AckPolicy.Explicit,
      ack_wait: 30_000_000_000,
      max_ack_pending: 10,
      filter_subject: `${prefix}.*.created`,
    });
    const manager = new JobManager({ nc: js, jobs: binding.jobs });
    const jobs = await Promise.all(
      [1, 2, 3].map((index) => manager.create("refresh", { index })),
    );
    let handled = 0;
    host = await startNatsWorkerHostFromBinding(binding, {
      nats: nc,
      manager,
      instanceId: "bounded-worker",
      queueConcurrency: { refresh: 1 },
      handler: async () => {
        handled += 1;
        if (handled === 1) {
          await release.promise;
        }
        return {};
      },
    });
    await waitFor(() => Promise.resolve(handled === 1));
    await nc.flush();
    const held = await jsm.consumers.info("ADAPTER_JOBS", "bounded-refresh");
    assertEquals(
      held.num_ack_pending,
      1,
      "one slot must not buffer other jobs",
    );
    assertEquals(held.num_pending, 2, "two jobs must remain at the broker");
    console.log(
      `held handler: ack_pending=${held.num_ack_pending}, queued=${held.num_pending}`,
    );
    release.resolve();
    await waitFor(async () => {
      const info = await jsm.consumers.info("ADAPTER_JOBS", "bounded-refresh");
      return info.num_ack_pending === 0 && info.num_pending === 0;
    });
    const completed = await jsm.direct.getMessage("ADAPTER_JOBS", {
      last_by_subj: `${prefix}.${jobs[0]!.id}.completed`,
    });
    assert(completed);
    assertEquals(JSON.parse(completed.string()).state, "completed");
    await host.stop();
    host = undefined;
    assertEquals(handled, 3);

    // The broker, not a local timer or fake consumer, establishes that a pull
    // is outstanding before stop is requested. Late delivery must be NAKed.
    let lateHandled = 0;
    host = await startNatsWorkerHostFromBinding(binding, {
      nats: nc,
      manager,
      instanceId: "late-worker",
      handler: () => {
        lateHandled += 1;
        return Promise.resolve({});
      },
    });
    await waitFor(async () =>
      (await jsm.consumers.info("ADAPTER_JOBS", "bounded-refresh"))
        .num_waiting === 1
    );
    let stopped = false;
    const stop = host.stop().then(() => {
      stopped = true;
    });
    await nc.flush();
    assertEquals(stopped, false, "stop must retain the issued broker pull");
    const late = await manager.create("refresh", { index: 4 });
    await stop;
    host = undefined;
    assertEquals(lateHandled, 0, "shutdown must not start the late job");
    const consumer = await js.consumers.get("ADAPTER_JOBS", "bounded-refresh");
    const redelivery = await consumer.next({ expires: 1_000 });
    assert(
      redelivery,
      "late delivery must remain available after shutdown NAK",
    );
    assertEquals(JSON.parse(redelivery.string()).jobId, late.id);
    assertEquals(
      redelivery.info.deliveryCount,
      2,
      "host must account the original delivery with NAK",
    );
    assert(await redelivery.ackAck());
    await waitFor(async () => {
      const info = await jsm.consumers.info("ADAPTER_JOBS", "bounded-refresh");
      return info.num_ack_pending === 0 && info.num_waiting === 0;
    });
    console.log(
      "late shutdown delivery: NAKed, redelivered and acknowledged; ack_pending=0",
    );

    // An empty pull also settles normally at the broker's bounded expiry.
    host = await startNatsWorkerHostFromBinding(binding, {
      nats: nc,
      manager,
      instanceId: "idle-worker",
      handler: () => Promise.resolve({}),
    });
    await waitFor(async () =>
      (await jsm.consumers.info("ADAPTER_JOBS", "bounded-refresh"))
        .num_waiting === 1
    );
    stopped = false;
    const idleStop = host.stop().then(() => {
      stopped = true;
    });
    await nc.flush();
    assertEquals(stopped, false, "an idle slot must await server expiry");
    await idleStop;
    host = undefined;
    const idle = await jsm.consumers.info("ADAPTER_JOBS", "bounded-refresh");
    assertEquals(idle.num_waiting, 0);
    assertEquals(idle.num_ack_pending, 0);
    console.log(
      "idle shutdown: broker expiry completed; waiting=0, ack_pending=0",
    );

    replacement = await nats.connectNats();
    const rebound = manager.withTransport(jetstream(replacement), undefined);
    await nc.close();
    await assertRejects(() => manager.create("refresh", { index: 5 }));
    const reboundJob = await rebound.create("refresh", { index: 6 });
    const reboundJsm = await jetstreamManager(replacement);
    host = await startNatsWorkerHostFromBinding(binding, {
      nats: replacement,
      manager: rebound,
      instanceId: "rebound-worker",
      handler: () => Promise.resolve({ rebound: true }),
    });
    await waitFor(async () => {
      const info = await reboundJsm.consumers.info(
        "ADAPTER_JOBS",
        "bounded-refresh",
      );
      return info.num_pending === 0 && info.num_ack_pending === 0;
    });
    const reboundCompleted = await reboundJsm.direct.getMessage(
      "ADAPTER_JOBS",
      {
        last_by_subj: `${prefix}.${reboundJob.id}.completed`,
      },
    );
    assert(reboundCompleted);
    assertEquals(JSON.parse(reboundCompleted.string()).result, {
      rebound: true,
    });
    await host.stop();
    host = undefined;
    console.log(
      "immutable transport rebinding: old manager rejects; new manager completes on independent connection",
    );

    // One host-owned subscription must cancel both fixed slots, not only the
    // slot that happened to receive the cancellation event.
    const cancellingJobs = await Promise.all(
      [7, 8].map((index) => rebound.create("refresh", { index })),
    );
    let active = 0;
    let cancelled = 0;
    host = await startNatsWorkerHostFromBinding(binding, {
      nats: replacement,
      manager: rebound,
      instanceId: "cancelling-worker",
      queueConcurrency: { refresh: 2 },
      handler: async (job) => {
        active += 1;
        await waitFor(() => Promise.resolve(job.isCancelled()));
        cancelled += 1;
        return {};
      },
    });
    await waitFor(() => Promise.resolve(active === 2));
    for (const job of cancellingJobs) {
      await jetstream(replacement).publish(
        `${prefix}.${job.id}.cancelled`,
        new TextEncoder().encode(JSON.stringify({
          jobId: job.id,
          service: job.service,
          jobType: job.type,
          eventType: "cancelled",
          state: "cancelled",
          context: job.context,
          tries: 1,
          timestamp: new Date().toISOString(),
        })),
      );
    }
    await waitFor(() => Promise.resolve(cancelled === 2));
    await waitFor(async () =>
      (await reboundJsm.consumers.info("ADAPTER_JOBS", "bounded-refresh"))
        .num_ack_pending === 0
    );
    await host.stop();
    host = undefined;
    const terminalSubjects = await reboundJsm.streams.info("ADAPTER_JOBS", {
      subjects_filter: `${prefix}.*.completed`,
    });
    assertEquals(
      Object.keys(terminalSubjects.state.subjects ?? {}).length,
      4,
      "cancelled handlers must not publish completion",
    );
    console.log(
      "host cancellation: both active slots cancelled and acknowledged without completed lifecycle",
    );

    receivingConnection = await nats.connectNats();
    const physicalLoss = new AbortController();
    void receivingConnection.closed().then(() => physicalLoss.abort());
    const lostConsumer = toWorkerConsumer(
      await jetstream(receivingConnection).consumers.get(
        "ADAPTER_JOBS",
        "bounded-refresh",
      ),
    );
    const lostManager = rebound.withTransport(
      jetstream(receivingConnection),
      undefined,
    );
    const sessionConsumer = toWorkerConsumer(
      await jetstream(replacement).consumers.get(
        "ADAPTER_JOBS",
        "bounded-refresh",
      ),
    );
    let acquired = false;
    let oldAcquired = false;
    let oldReleased = false;
    let sessionHandled = false;
    let sessionReleased = false;
    slot = await startQueueWorkerLoop({
      cancellationRegistry: new ActiveJobCancellationRegistry(),
      acquireSession: (signal) => {
        if (!oldAcquired) {
          oldAcquired = true;
          return Promise.resolve({
            consumer: lostConsumer,
            manager: lostManager,
            physicalLoss: physicalLoss.signal,
            release() {
              oldReleased = true;
            },
          });
        }
        if (acquired) {
          return new Promise<JobReceivingSession<unknown>>(
            (_resolve, reject) => {
              if (signal.aborted) reject(signal.reason);
              else {signal.addEventListener("abort", () =>
                  reject(signal.reason), { once: true });}
            },
          );
        }
        acquired = true;
        return Promise.resolve({
          consumer: sessionConsumer,
          manager: rebound,
          async release() {
            if (sessionReleased) return;
            sessionReleased = true;
            await replacement!.flush();
          },
        });
      },
      handler: async () => {
        sessionHandled = true;
        await releaseSessionHandler.promise;
        return { session: true };
      },
    });
    await waitFor(async () =>
      (await reboundJsm.consumers.info("ADAPTER_JOBS", "bounded-refresh"))
        .num_waiting === 1
    );
    await receivingConnection.close();
    await waitFor(() => Promise.resolve(oldReleased && acquired));
    const sessionJob = await rebound.create("refresh", { index: 9 });
    await waitFor(() => Promise.resolve(sessionHandled));
    assertEquals(
      sessionReleased,
      false,
      "receiving lease must remain owned while the handler runs",
    );
    releaseSessionHandler.resolve();
    await waitFor(() => Promise.resolve(sessionReleased));
    await slot.stop();
    slot = undefined;
    assertEquals(
      (await reboundJsm.consumers.info("ADAPTER_JOBS", "bounded-refresh"))
        .num_ack_pending,
      0,
      "session release must follow the source message disposition",
    );
    const sessionCompleted = await reboundJsm.direct.getMessage(
      "ADAPTER_JOBS",
      {
        last_by_subj: `${prefix}.${sessionJob.id}.completed`,
      },
    );
    assert(sessionCompleted);
    assertEquals(JSON.parse(sessionCompleted.string()).result, {
      session: true,
    });
    console.log(
      "receiving-session ownership: closed source released and replaced; live source held through handler and completion ACK; shutdown aborts next acquisition",
    );
  } finally {
    release.resolve();
    releaseSessionHandler.resolve();
    try {
      await slot?.stop();
      await host?.stop();
    } finally {
      try {
        await replacement?.close();
        await receivingConnection?.close();
        await nats?.stop();
      } finally {
        await Deno.remove(workdir, { recursive: true });
      }
    }
  }
});

async function waitFor(predicate: () => Promise<boolean>): Promise<void> {
  const deadline = performance.now() + 5_000;
  while (performance.now() < deadline) {
    if (await predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  throw new Error("broker state did not converge within 5 seconds");
}
