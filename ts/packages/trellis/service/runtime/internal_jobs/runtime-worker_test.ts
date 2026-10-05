import { AckPolicy, jetstream, jetstreamManager } from "@nats-io/jetstream";
import { Kvm } from "@nats-io/kv";
import { deadline } from "@nats-io/nats-core";
import { connect, credsAuthenticator } from "@nats-io/transport-node";
import { join } from "@std/path";
import { assert, assertEquals, assertRejects, assertThrows } from "@std/assert";

import { startTrellisRuntime } from "../../../../../integration/_support/runtime.ts";
import type { TrellisTestRuntime } from "@oatscenter/trellis-testkit";
import { ActiveJobCancellationRegistry } from "./cancellation-registry.ts";
import {
  JobManager,
  JobProcessError,
  prepareJobSubmission,
} from "./job-manager.ts";
import {
  createNatsJobKeyCoordinator,
  deriveJobKey,
  type JobKeyState,
} from "./key-coordinator.ts";
import {
  ackActionForOutcome,
  getLatestLifecycleEvent,
  JobsConsumerMissingError,
  progressAckIntervalMs,
  startNatsWorkerHostFromBinding,
  startQueueWorkerLoop,
  toWorkerConsumer,
} from "./runtime-worker.ts";
import type { Job, JobEvent } from "./types.ts";

Deno.test("republishing a completed prepared submission after broker deduplication does not execute it again", async () => {
  await withBroker(async (fixture) => {
    const { manager, jsm, host, settled, completed, nc, encode } = fixture;
    await jsm.streams.update("ADAPTER_JOBS", { duplicate_window: 100_000_000 });
    const submission = prepareJobSubmission({
      submissionId: crypto.randomUUID(),
      mode: "create",
      service: "svc",
      queue: "refresh",
      jobId: crypto.randomUUID(),
      payload: { tenant: "duplicate" },
      createdAt: new Date().toISOString(),
    });
    let executions = 0;
    await host(async () => ({ execution: ++executions }));
    const job = await manager.createPrepared(submission);
    await settled();
    const original = await completed(job);
    for (let index = 0; index < 50_000; index++) {
      const eventType = index % 2 === 0 ? "progress" : "logged";
      nc.publish(
        `adapter.jobs.svc.refresh.${job.id}.${eventType}`,
        encode({
          ...original,
          eventType,
          state: "active",
          previousState: "active",
        }),
      );
    }
    await nc.flush();
    await waitFor(async () =>
      (await jsm.streams.info("ADAPTER_JOBS")).state.messages >= 50_003
    );
    let recoveryReads = 0;
    const scanSubjects = new Set<string>();
    const reads = [
      "$JS.API.DIRECT.GET.ADAPTER_JOBS",
      "$JS.API.DIRECT.GET.ADAPTER_JOBS.>",
    ].map((
      subject,
    ) =>
      nc.subscribe(subject, {
        callback: (_error, message) => {
          recoveryReads++;
          if (message.subject === "$JS.API.DIRECT.GET.ADAPTER_JOBS") {
            const query = message.json<{ next_by_subj?: string }>();
            if (query.next_by_subj) scanSubjects.add(query.next_by_subj);
          }
        },
      })
    );
    // This wait exercises the broker's actual finite deduplication contract.
    await new Promise((resolve) => setTimeout(resolve, 150));
    await manager.createPrepared(submission);
    await settled();
    await nc.flush();
    for (const subscription of reads) subscription.unsubscribe();
    assertEquals(executions, 1);
    assert(
      scanSubjects.size > 0,
      "the probe must observe historical scan requests",
    );
    assert(
      [...scanSubjects].every((subject) =>
        !subject.includes("*") && !subject.includes(">")
      ),
      `recovery must not scan observation wildcards: ${
        JSON.stringify([...scanSubjects])
      }`,
    );
    assert(
      recoveryReads > 0 && recoveryReads < 32,
      `terminal recovery must exclude 50,000 observations, got ${recoveryReads} direct reads`,
    );
    assertEquals((await completed(job)).result, original.result);
    assertEquals(
      (await jsm.consumers.info("ADAPTER_JOBS", "worker")).ack_floor
        .consumer_seq,
      2,
    );
  });
});

Deno.test("deadline recovery excludes retained observation traffic before invoking cleanup", async () => {
  await withBroker(async (f) => {
    const manager = new JobManager({
      nc: f.js,
      jobs: {
        ...f.binding.jobs,
        queues: {
          refresh: { ...f.binding.jobs.queues.refresh, defaultDeadlineMs: 1 },
        },
      },
    });
    const job = await manager.create("refresh", {});
    const prefix = f.binding.jobs.queues.refresh.publishPrefix;
    const message = await f.jsm.direct.getMessage("ADAPTER_JOBS", {
      last_by_subj: `${prefix}.${job.id}.created`,
    });
    assert(message);
    const started: JobEvent = {
      ...JSON.parse(message.string()),
      eventType: "started",
      state: "active",
      previousState: "pending",
      tries: 1,
      startedAt: new Date().toISOString(),
      timestamp: new Date().toISOString(),
    };
    await f.js.publish(`${prefix}.${job.id}.started`, f.encode(started));
    for (let index = 0; index < 50_000; index++) {
      const eventType = index % 2 === 0 ? "progress" : "logged";
      f.nc.publish(
        `${prefix}.${job.id}.${eventType}`,
        f.encode({ ...started, eventType }),
      );
    }
    await f.nc.flush();
    await waitFor(async () =>
      (await f.jsm.streams.info("ADAPTER_JOBS")).state.messages >= 50_002
    );
    let recoveryReads = 0;
    const scanSubjects = new Set<string>();
    const reads = [
      "$JS.API.DIRECT.GET.ADAPTER_JOBS",
      "$JS.API.DIRECT.GET.ADAPTER_JOBS.>",
    ].map((
      subject,
    ) =>
      f.nc.subscribe(subject, {
        callback: (_error, message) => {
          recoveryReads++;
          if (message.subject === "$JS.API.DIRECT.GET.ADAPTER_JOBS") {
            const query = message.json<{ next_by_subj?: string }>();
            if (query.next_by_subj) scanSubjects.add(query.next_by_subj);
          }
        },
      })
    );
    let cleanups = 0;
    await f.host(async (active) => {
      assertEquals(active.cancellationToken().reason(), "deadline-exceeded");
      cleanups++;
      return { reconciled: true };
    });
    await f.settled();
    await f.nc.flush();
    for (const subscription of reads) subscription.unsubscribe();
    assertEquals(cleanups, 1);
    assert(
      scanSubjects.size > 0,
      "the probe must observe historical scan requests",
    );
    assert(
      [...scanSubjects].every((subject) =>
        !subject.includes("*") && !subject.includes(">")
      ),
      `recovery must not scan observation wildcards: ${
        JSON.stringify([...scanSubjects])
      }`,
    );
    assert(
      recoveryReads > 0 && recoveryReads < 32,
      `deadline cleanup must exclude observations, got ${recoveryReads} direct reads`,
    );
    assertEquals(
      (await getLatestLifecycleEvent(f.jsm.direct, "ADAPTER_JOBS", prefix, job))
        ?.state,
      "expired",
    );
  });
});

// Production worker kernel and adapters, with case-owned authenticated NATS.
// Full resource provisioning and generation handoff belong to live integration.
Deno.test("a duplicate prepared publication is retired without repeating an active handler", async () => {
  await withBroker(async ({ manager, jsm, host, settled }) => {
    await jsm.streams.update("ADAPTER_JOBS", { duplicate_window: 100_000_000 });
    const entered = Promise.withResolvers<void>();
    const release = Promise.withResolvers<void>();
    const submission = prepareJobSubmission({
      submissionId: crypto.randomUUID(),
      mode: "create",
      service: "svc",
      queue: "refresh",
      jobId: crypto.randomUUID(),
      payload: { tenant: "active-duplicate" },
      createdAt: new Date().toISOString(),
    });
    let executions = 0;
    await host(async () => {
      const execution = ++executions;
      if (execution === 1) {
        entered.resolve();
        await release.promise;
      }
      return { execution };
    }, 2);
    try {
      await manager.createPrepared(submission);
      await deadline(entered.promise, 5_000);
      await new Promise((resolve) => setTimeout(resolve, 150));
      await manager.createPrepared(submission);
      await waitFor(async () => {
        const info = await jsm.consumers.info("ADAPTER_JOBS", "worker");
        return info.delivered.consumer_seq >= 2 && info.num_pending === 0 &&
          info.num_ack_pending === 1;
      });
      assertEquals(executions, 1);
    } finally {
      release.resolve();
    }
    await settled();
  });
});

async function withBroker(
  run: (fixture: Awaited<ReturnType<typeof brokerFixture>>) => Promise<void>,
  keyed = false,
) {
  const workdir = await Deno.makeTempDir({ prefix: "jobs-worker-" });
  let nats: TrellisTestRuntime | undefined;
  let fixture: Awaited<ReturnType<typeof brokerFixture>> | undefined;
  try {
    nats = await startTrellisRuntime();
    fixture = await brokerFixture(nats, keyed, workdir);
    await run(fixture);
  } finally {
    try {
      await Promise.all(fixture?.workers.map((worker) => worker.stop()) ?? []);
    } finally {
      await nats?.stop();
      await Deno.remove(workdir, { recursive: true });
    }
  }
}

async function brokerFixture(
  nats: TrellisTestRuntime,
  keyed: boolean,
  workdir: string,
) {
  const nc = await nats.connectNats();
  const js = jetstream(nc);
  const jsm = await jetstreamManager(nc);
  const prefix = "adapter.jobs.svc.refresh";
  const queue = {
    queueType: "refresh",
    publishPrefix: prefix,
    workSubject: `${prefix}.*.created`,
    consumerName: "worker",
    payload: { schema: "RefreshPayload" },
    maxDeliver: 2,
    backoffMs: [100],
    ackWaitMs: 100,
    ...(keyed
      ? {
        keyConcurrency: {
          key: ["/tenant"],
          maxActive: 1,
          heartbeatIntervalMs: 30_000,
          heartbeatTtlMs: 120_000,
          stalePolicy: "fail-stale" as const,
        },
        queue: { maxQueuedPerKey: 2, whenFull: "reject" as const },
      }
      : {}),
  };
  const binding = {
    workStream: "ADAPTER_JOBS",
    jobs: {
      serviceName: "svc",
      namespace: "svc",
      queues: { refresh: queue },
    },
  };
  await jsm.streams.add({
    name: "ADAPTER_JOBS",
    subjects: [`${prefix}.>`],
    allow_direct: true,
  });
  await jsm.consumers.add("ADAPTER_JOBS", {
    durable_name: "worker",
    ack_policy: AckPolicy.Explicit,
    ack_wait: 100_000_000,
    max_deliver: 2,
    filter_subjects: [`${prefix}.*.created`, `${prefix}.*.retried`],
  });
  const kvm = new Kvm(nc);
  const projection = await kvm.create("WORKER_PROJECTION");
  const keys = keyed ? await kvm.create("JOBS_KEYS_svc") : undefined;
  const manager = new JobManager({
    nc: js,
    jobs: binding.jobs,
    keyCoordinator: keyed ? createNatsJobKeyCoordinator(nc) : undefined,
  });
  const workers: Array<{ stop(): Promise<void> }> = [];
  const encode = (value: unknown) =>
    new TextEncoder().encode(JSON.stringify(value));
  const lifecycle = (job: Job) =>
    getLatestLifecycleEvent(jsm.direct, "ADAPTER_JOBS", prefix, job);
  const projected = async (job: Job): Promise<Job | undefined> => {
    const entry = await projection.get(job.id);
    return entry ? JSON.parse(entry.string()) : undefined;
  };
  const event = async (
    job: Job,
    eventType: JobEvent["eventType"],
    state: JobEvent["state"],
  ) => {
    await js.publish(
      `${prefix}.${job.id}.${eventType}`,
      encode(
        {
          jobId: job.id,
          service: job.service,
          jobType: job.type,
          context: job.context,
          payload: job.payload,
          maxTries: job.maxTries,
          tries: 0,
          timestamp: new Date().toISOString(),
          eventType,
          state,
          ...(eventType === "retried" ? { previousState: "failed" } : {}),
          ...(eventType === "skipped" ? { previousState: "pending" } : {}),
        } satisfies JobEvent,
      ),
    );
  };
  const keyState = async (payload: unknown) => {
    assert(keys);
    const derived = await deriveJobKey({
      service: "svc",
      jobType: "refresh",
      payload,
      template: ["/tenant"],
    });
    const entry = await keys.get(derived.kvKey);
    assert(entry);
    return JSON.parse(entry.string()) as JobKeyState;
  };
  const completed = async (job: Job) => {
    const msg = await jsm.direct.getMessage("ADAPTER_JOBS", {
      last_by_subj: `${prefix}.${job.id}.completed`,
    });
    assert(msg);
    return JSON.parse(msg.string()) as JobEvent;
  };
  const settled = () =>
    waitFor(async () => {
      const info = await jsm.consumers.info("ADAPTER_JOBS", "worker");
      return info.num_pending === 0 && info.num_ack_pending === 0;
    });
  const host = async (
    handler: Parameters<typeof startNatsWorkerHostFromBinding>[1]["handler"],
    concurrency = 1,
  ) => {
    const worker = await startNatsWorkerHostFromBinding(binding, {
      nats: nc,
      manager,
      instanceId: "worker",
      handler,
      queueConcurrency: { refresh: concurrency },
      getProjectedJob: projected,
    });
    workers.push(worker);
    return worker;
  };
  const slot = async (
    handler: Parameters<typeof startQueueWorkerLoop>[0]["handler"],
    readLifecycle = true,
    readProjection = projected,
  ) => {
    const consumer = toWorkerConsumer(
      await js.consumers.get("ADAPTER_JOBS", "worker"),
    );
    const worker = await startQueueWorkerLoop({
      cancellationRegistry: new ActiveJobCancellationRegistry(),
      acquireSession: () =>
        Promise.resolve({
          manager,
          consumer,
          getProjectedJob: readProjection,
          getLatestLifecycleEvent: readLifecycle ? lifecycle : undefined,
          release() {},
        }),
      handler,
      backoffMs: [100],
      deferralBackoffMs: 1_000,
      progressAckIntervalMs: progressAckIntervalMs(queue),
    });
    workers.push(worker);
    return worker;
  };
  return {
    nats,
    workdir,
    nc,
    js,
    jsm,
    binding,
    manager,
    projection,
    keys,
    encode,
    event,
    keyState,
    completed,
    settled,
    host,
    slot,
    workers,
  };
}

async function waitFor(predicate: () => Promise<boolean>): Promise<void> {
  const until = performance.now() + 5_000;
  while (performance.now() < until) {
    if (await predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  throw new Error("worker broker state did not converge within 5 seconds");
}

Deno.test("terminal projected work is acknowledged without handler execution", () =>
  withBroker(async (f) => {
    const job = await f.manager.create("refresh", {});
    await f.projection.put(job.id, f.encode({ ...job, state: "cancelled" }));
    await f.slot(
      () => Promise.reject(new Error("terminal work executed")),
      false,
    );
    await f.settled();
    const info = await f.jsm.streams.info("ADAPTER_JOBS", {
      subjects_filter:
        `${f.binding.jobs.queues.refresh.publishPrefix}.${job.id}.started`,
    });
    assertEquals(
      info.state.messages,
      1,
      "only the original creation is retained",
    );
  }));

Deno.test("latest durable lifecycle wins over a stale terminal projection", () =>
  withBroker(async (f) => {
    const job = await f.manager.create("refresh", {});
    await f.projection.put(job.id, f.encode({ ...job, state: "cancelled" }));
    await f.event(job, "started", "active");
    await f.host(() => Promise.resolve({ refreshed: true }));
    await f.settled();
    assertEquals((await f.completed(job)).result, { refreshed: true });
  }));

Deno.test("progress acknowledgements prevent redelivery during a held handler", () =>
  withBroker(async (f) => {
    const job = await f.manager.create("refresh", {});
    const entered = Promise.withResolvers<void>();
    const release = Promise.withResolvers<void>();
    try {
      await f.host(async () => {
        entered.resolve();
        await release.promise;
        return { held: true };
      });
      await deadline(entered.promise, 5_000);
      const competitor = await f.js.consumers.get("ADAPTER_JOBS", "worker");
      const receive = competitor.next({ expires: 1_000 });
      const since = performance.now();
      await waitFor(() => Promise.resolve(performance.now() - since > 350));
      assertEquals(
        (await f.jsm.consumers.info("ADAPTER_JOBS", "worker")).delivered
          .consumer_seq,
        1,
        "held job must not be redelivered across multiple ack waits",
      );
      release.resolve();
      assertEquals(
        await receive,
        null,
        "competing pull must not receive the held job",
      );
      await f.settled();
      assertEquals((await f.completed(job)).tries, 1);
    } finally {
      release.resolve();
    }
  }));

Deno.test("a real projection outage NAKs work and the slot processes later deliveries", () =>
  withBroker(async (f) => {
    const job = await f.manager.create("refresh", {});
    const readerConnection = await f.nats.connectNats();
    let projection = await new Kvm(readerConnection).open("WORKER_PROJECTION");
    await readerConnection.close();
    await assertRejects(() => projection.get(job.id));
    await f.slot(
      () => Promise.resolve({ recovered: true }),
      false,
      async (work) => {
        const entry = await projection.get(work.id);
        return entry ? JSON.parse(entry.string()) : undefined;
      },
    );
    await waitFor(async () =>
      (await f.jsm.consumers.info("ADAPTER_JOBS", "worker")).delivered
        .consumer_seq >= 1
    );
    await waitFor(async () =>
      (await f.jsm.consumers.info("ADAPTER_JOBS", "worker")).num_waiting === 1
    );
    const failedAt = performance.now();
    await waitFor(() => Promise.resolve(performance.now() - failedAt > 350));
    assertEquals(
      (await f.jsm.consumers.info("ADAPTER_JOBS", "worker")).delivered
        .consumer_seq,
      1,
      "the delayed NAK must defer redelivery beyond several ordinary ack waits",
    );
    projection = f.projection;
    const later = await f.manager.create("refresh", {});
    await f.settled();
    assertEquals(
      (await f.completed(job)).result,
      { recovered: true },
      "real projection failure must leave the source job available",
    );
    assertEquals((await f.completed(later)).result, { recovered: true });
    assertEquals(
      (await f.jsm.consumers.info("ADAPTER_JOBS", "worker")).delivered
        .consumer_seq,
      3,
      "one failed read, a later job, and redelivery must all be accounted",
    );
  }));

Deno.test("final ordinary retry redelivers for execution-owned reconciliation", () =>
  withBroker(async (f) => {
    await f.jsm.consumers.update("ADAPTER_JOBS", "worker", { max_deliver: -1 });
    let handled = 0;
    let reconciled = 0;
    const job = await f.manager.create("refresh", {});
    await f.host((activeJob) => {
      if (activeJob.isCancelled()) {
        assertEquals(activeJob.cancellationToken().reason(), "retry-exhausted");
        reconciled++;
        return Promise.resolve({});
      }
      handled++;
      return Promise.reject(JobProcessError.retryable("retry again"));
    });
    await f.settled();
    const retry = await f.jsm.direct.getMessage("ADAPTER_JOBS", {
      last_by_subj:
        `${f.binding.jobs.queues.refresh.publishPrefix}.${job.id}.retry`,
    });
    assert(retry);
    assertEquals(JSON.parse(retry.string()).tries, 2);
    assertEquals(handled, 2);
    assertEquals(reconciled, 1);
    assertEquals(
      (await getLatestLifecycleEvent(
        f.jsm.direct,
        "ADAPTER_JOBS",
        f.binding.jobs.queues.refresh.publishPrefix,
        job,
      ))?.eventType,
      "dead",
    );
    const info = await f.jsm.consumers.info("ADAPTER_JOBS", "worker");
    assertEquals(info.delivered.consumer_seq, 3);
    assertEquals(
      info.ack_floor.stream_seq,
      1,
      "the final redelivery must reconcile execution exhaustion and ACK the source",
    );
  }));

Deno.test("terminal lifecycle is ACKed and removes its queued KV reservation", () =>
  withBroker(async (f) => {
    const payload = { tenant: "a" };
    const job = await f.manager.create("refresh", payload);
    assertEquals(
      (await f.keyState(payload)).queued.map((entry) => entry.jobId),
      [job.id],
    );
    await f.event(job, "skipped", "skipped");
    await f.host(() => Promise.resolve({ cleaned: true }));
    await f.settled();
    const terminal = await getLatestLifecycleEvent(
      f.jsm.direct,
      "ADAPTER_JOBS",
      f.binding.jobs.queues.refresh.publishPrefix,
      job,
    );
    assertEquals(terminal?.state, "skipped", "terminal work must not execute");
    assertEquals((await f.keyState(payload)).queued, []);
    const next = await f.manager.create("refresh", payload);
    await f.settled();
    assertEquals((await f.completed(next)).state, "completed");
  }, true));

Deno.test("terminal work redelivers until its real key bucket recovers and cleanup completes", () =>
  withBroker(async (f) => {
    const payload = { tenant: "a" };
    const job = await f.manager.create("refresh", payload);
    const persisted = await f.keyState(payload);
    const key = await deriveJobKey({
      service: "svc",
      jobType: "refresh",
      payload,
      template: ["/tenant"],
    });
    await f.jsm.consumers.update("ADAPTER_JOBS", "worker", { max_deliver: -1 });
    await f.event(job, "skipped", "skipped");
    await f.jsm.streams.delete("KV_JOBS_KEYS_svc");
    const worker = await f.host(() =>
      Promise.reject(new Error("terminal work executed"))
    );
    await waitFor(async () =>
      (await f.jsm.consumers.info("ADAPTER_JOBS", "worker")).delivered
        .consumer_seq >= 2
    );
    const info = await f.jsm.consumers.info("ADAPTER_JOBS", "worker");
    assertEquals(
      info.ack_floor.stream_seq,
      0,
      "cleanup outage must leave the source unacknowledged",
    );
    await worker.stop();
    const restored = await new Kvm(f.nc).create("JOBS_KEYS_svc");
    await restored.put(key.kvKey, f.encode(persisted));
    await f.host(() => Promise.reject(new Error("terminal work executed")));
    await f.settled();
    assertEquals((await f.keyState(payload)).queued, []);
    assertEquals(
      (await f.jsm.consumers.info("ADAPTER_JOBS", "worker")).ack_floor
        .stream_seq,
      1,
      "successful cleanup must acknowledge the source",
    );
    assertEquals(
      (await getLatestLifecycleEvent(
        f.jsm.direct,
        "ADAPTER_JOBS",
        f.binding.jobs.queues.refresh.publishPrefix,
        job,
      ))?.state,
      "skipped",
      "recovery must not execute terminal business work",
    );
  }, true));

Deno.test("keyed capacity deferral retains the delivery until a real KV slot releases", () =>
  withBroker(async (f) => {
    const payload = { tenant: "a" };
    const first = await f.manager.create("refresh", payload);
    const entered = Promise.withResolvers<void>();
    const release = Promise.withResolvers<void>();
    try {
      await f.host(async (active) => {
        if (active.job().id === first.id) {
          entered.resolve();
          await release.promise;
        }
        return { id: active.job().id };
      }, 2);
      await deadline(entered.promise, 5_000);
      // Key concurrency does not promise FIFO acquisition. Establish the held
      // active slot before submitting the job whose deferral is being tested.
      const second = await f.manager.create("refresh", payload);
      await waitFor(async () =>
        (await f.jsm.consumers.info("ADAPTER_JOBS", "worker"))
          .num_ack_pending === 2
      );
      const competitor = await f.js.consumers.get("ADAPTER_JOBS", "worker");
      const receive = competitor.next({ expires: 1_000 });
      const since = performance.now();
      await waitFor(() => Promise.resolve(performance.now() - since > 350));
      const state = await f.keyState(payload);
      assertEquals(state.active.map((entry) => entry.jobId), [first.id]);
      assertEquals(state.queued.map((entry) => entry.jobId), [second.id]);
      assertEquals(
        (await f.jsm.consumers.info("ADAPTER_JOBS", "worker")).delivered
          .consumer_seq,
        2,
      );
      release.resolve();
      assertEquals(await receive, null);
      await f.settled();
      assertEquals((await f.completed(second)).tries, 1);
      assertEquals((await f.keyState(payload)).active, []);
    } finally {
      release.resolve();
    }
  }, true));

Deno.test("manual retry of failed keyed work reacquires a real KV slot without create admission", () =>
  withBroker(async (f) => {
    const payload = { tenant: "a" };
    const job = await f.manager.create("refresh", payload);
    const failedHost = await f.host(() =>
      Promise.reject(JobProcessError.failed("invalid input"))
    );
    await f.settled();
    await failedHost.stop();
    const latest = await getLatestLifecycleEvent(
      f.jsm.direct,
      "ADAPTER_JOBS",
      f.binding.jobs.queues.refresh.publishPrefix,
      job,
    );
    assertEquals(latest?.state, "failed");
    assertEquals((await f.keyState(payload)).active, []);
    assertEquals((await f.keyState(payload)).queued, []);
    // The ordinary admin retry envelope does not perform another create admission.
    await f.event(job, "retried", "pending");
    await f.host(() => Promise.resolve({ recovered: true }));
    await f.settled();
    assertEquals((await f.completed(job)).result, { recovered: true });
    assertEquals((await f.keyState(payload)).active, []);
  }, true));

Deno.test("missing approved consumer fails closed against actual JetStream", () =>
  withBroker(async (f) => {
    await f.jsm.consumers.delete("ADAPTER_JOBS", "worker");
    await assertRejects(
      () => f.host(() => Promise.resolve({})),
      JobsConsumerMissingError,
    );
    await f.manager.create("refresh", {});
    const info = await f.jsm.streams.info("ADAPTER_JOBS");
    assertEquals(
      info.state.messages,
      1,
      "queued work remains retained without a worker",
    );
    assertEquals(
      info.state.consumer_count,
      0,
      "host must not silently recreate missing approved consumer",
    );
  }));

Deno.test("invalid implementation concurrency rejects against real broker", () =>
  withBroker(async (f) => {
    await assertRejects(
      () => f.host(() => Promise.resolve({}), 0),
      Error,
      "expected a positive integer",
    );
    const job = await f.manager.create("refresh", {});
    await f.host(() => Promise.resolve({ valid: true }));
    await f.settled();
    assertEquals((await f.completed(job)).result, { valid: true });
  }));

Deno.test("ackActionForOutcome maps dispositions including final exhaustion", () => {
  assertEquals(
    ackActionForOutcome({
      outcome: "deferred",
      tries: 1,
      reason: "active-limit",
    }),
    "nak",
  );
  assertEquals(
    ackActionForOutcome({ outcome: "retry", tries: 1, error: "retry" }),
    "nak",
  );
  assertEquals(
    ackActionForOutcome({ outcome: "retry", tries: 2, error: "retry" }),
    "nak",
  );
  assertEquals(
    ackActionForOutcome({ outcome: "completed", tries: 1, result: {} }),
    "ack",
  );
});

Deno.test("progress ACK cadence floors and clamps to whole milliseconds", () => {
  const queue = {
    queueType: "refresh",
    ackWaitMs: 1_000,
    backoffMs: [],
    publishPrefix: "adapter.jobs.svc.refresh",
    workSubject: "trellis.work.svc.refresh",
    consumerName: "worker",
    maxDeliver: 2,
    payload: { schema: "RefreshPayload" },
  };
  for (const wait of [1, 2, 5]) {
    assertEquals(progressAckIntervalMs({ ...queue, backoffMs: [wait] }), 1);
  }
  assertEquals(progressAckIntervalMs({ ...queue, backoffMs: [3_000] }), 1_000);
  assertEquals(progressAckIntervalMs(queue), 333);
});

Deno.test("progress ACK cadence rejects sub-millisecond policies", () => {
  for (const wait of [0, 0.5]) {
    assertThrows(
      () =>
        progressAckIntervalMs({
          queueType: "refresh",
          ackWaitMs: 1_000,
          backoffMs: [wait],
          publishPrefix: "adapter.jobs.svc.refresh",
          workSubject: "trellis.work.svc.refresh",
          consumerName: "worker",
          maxDeliver: 2,
          payload: { schema: "RefreshPayload" },
        }),
      Error,
      "positive whole millisecond",
    );
  }
});
