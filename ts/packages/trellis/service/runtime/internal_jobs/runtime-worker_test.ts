import type { NatsConnection } from "@nats-io/nats-core";
import { assertEquals, assertRejects, assertThrows } from "@std/assert";

import { JobManager, JobProcessError } from "./job-manager.ts";
import type { JobKeyCoordinator, JobKeyState } from "./key-coordinator.ts";
import {
  ackActionForOutcome,
  JobsInfrastructureMissingError,
  progressAckIntervalMs,
  startNatsWorkerHostFromBinding,
  startQueueWorkerLoop,
} from "./runtime-worker.ts";
import type { Job, JobContext } from "./types.ts";

const jobContext: JobContext = {
  requestId: "request-1",
  traceId: "0123456789abcdef0123456789abcdef",
  traceparent: "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01",
};

const jobsBinding = {
  serviceName: "svc",
  namespace: "svc",
  queues: {
    refresh: {
      queueType: "refresh",
      publishPrefix: "trellis.jobs.svc.refresh",
      workSubject: "trellis.work.svc.refresh",
      consumerName: "svc-refresh",
      payload: { schema: "RefreshPayload" },
      maxDeliver: 5,
      backoffMs: [1],
      ackWaitMs: 1_000,
    },
  },
};

const keyedJobsBinding = {
  serviceName: "svc",
  namespace: "svc",
  queues: {
    sync: {
      queueType: "sync",
      publishPrefix: "trellis.jobs.svc.sync",
      workSubject: "trellis.work.svc.sync",
      consumerName: "svc-sync",
      payload: { schema: "SyncPayload" },
      maxDeliver: 5,
      backoffMs: [2_500],
      ackWaitMs: 1_000,
      keyConcurrency: {
        key: ["/tenant"],
        maxActive: 1,
        heartbeatIntervalMs: 30_000,
        heartbeatTtlMs: 120_000,
        stalePolicy: "fail-stale" as const,
      },
      queue: { maxQueuedPerKey: 0, whenFull: "reject" as const },
    },
  },
};

Deno.test("startNatsWorkerHostFromBinding uses implementation concurrency", async () => {
  let workers = 0;
  const host = await startNatsWorkerHostFromBinding(
    { jobs: jobsBinding, workStream: "JOBS_WORK" },
    {
      nats: {
        subscribe: () => cancelSubscription(() => {}) as never,
      },
      jsm: {
        consumers: { info: () => Promise.resolve({ config: {} }) },
      },
      js: {
        consumers: {
          getConsumerFromInfo: () => ({
            consume: () => {
              workers += 1;
              return Promise.resolve((async function* () {})());
            },
          }),
        },
      },
      instanceId: "worker-1",
      queueConcurrency: { refresh: 3 },
      manager: new JobManager({ nc: { publish: () => {} }, jobs: jobsBinding }),
      handler: () => Promise.resolve({}),
    },
  );

  assertEquals(workers, 3);
  assertEquals(host.workerCount(), 3);
  await host.stop();
});

Deno.test("startNatsWorkerHostFromBinding rejects invalid implementation concurrency", async () => {
  await assertRejects(
    () =>
      startNatsWorkerHostFromBinding(
        { jobs: jobsBinding, workStream: "JOBS_WORK" },
        {
          nats: {
            subscribe: () => cancelSubscription(() => {}) as never,
          },
          jsm: {
            consumers: { info: () => Promise.resolve({ config: {} }) },
          },
          js: {
            consumers: {
              getConsumerFromInfo: () => {
                throw new Error("consumer should not be built");
              },
            },
          },
          instanceId: "worker-1",
          queueConcurrency: { refresh: 0 },
          manager: new JobManager({
            nc: { publish: () => {} },
            jobs: jobsBinding,
          }),
          handler: () => Promise.resolve({}),
        },
      ),
    Error,
    "expected a positive integer",
  );
});

Deno.test("ackActionForOutcome naks keyed deferred work", () => {
  assertEquals(
    ackActionForOutcome({
      outcome: "deferred",
      tries: 1,
      reason: "active-limit",
    }),
    "nak",
  );
});

Deno.test("progress ACK cadence floors and clamps to whole milliseconds", () => {
  const queue = jobsBinding.queues.refresh;
  assertEquals(progressAckIntervalMs({ ...queue, backoffMs: [1] }), 1);
  assertEquals(progressAckIntervalMs({ ...queue, backoffMs: [2] }), 1);
  assertEquals(progressAckIntervalMs({ ...queue, backoffMs: [5] }), 1);
  assertEquals(progressAckIntervalMs({ ...queue, backoffMs: [3_000] }), 1_000);
});

Deno.test("progress ACK cadence rejects sub-millisecond policies", () => {
  const queue = jobsBinding.queues.refresh;
  for (const wait of [0, 0.5]) {
    assertThrows(
      () => progressAckIntervalMs({ ...queue, backoffMs: [wait] }),
      Error,
      "positive whole millisecond",
    );
  }
});

function cancelSubscription(unsubscribe: () => void): {
  unsubscribe(): void;
  [Symbol.asyncIterator](): AsyncIterator<
    { subject: string; data: Uint8Array }
  >;
} {
  let unsubscribed = false;
  return {
    unsubscribe: () => {
      unsubscribed = true;
      unsubscribe();
    },
    [Symbol.asyncIterator]() {
      return {
        async next() {
          while (!unsubscribed) {
            await new Promise((resolve) => setTimeout(resolve, 1));
          }
          return { done: true, value: undefined };
        },
      };
    },
  };
}

Deno.test("startQueueWorkerLoop skips terminal projected jobs before processing", async () => {
  let acked = 0;
  let handled = 0;
  const job: Job = {
    id: "job-1",
    service: "svc",
    type: "refresh",
    state: "pending",
    context: jobContext,
    payload: { siteId: "site-1" },
    createdAt: "2024-01-01T00:00:00.000Z",
    updatedAt: "2024-01-01T00:00:00.000Z",
    tries: 0,
    maxTries: 5,
  };
  const event = {
    jobId: job.id,
    service: job.service,
    jobType: job.type,
    eventType: "created",
    state: "pending",
    context: jobContext,
    tries: 0,
    maxTries: 5,
    payload: job.payload,
    timestamp: job.createdAt,
  };

  const loop = await startQueueWorkerLoop({
    manager: new JobManager({ nc: { publish: () => {} }, jobs: jobsBinding }),
    consumer: {
      consume() {
        return Promise.resolve((async function* () {
          yield {
            data: new TextEncoder().encode(JSON.stringify(event)),
            subject: "trellis.work.svc.refresh",
            ack: () => {
              acked += 1;
            },
            nak: () => {},
            inProgress: () => {},
          };
        })());
      },
    },
    cancelSubscription: cancelSubscription(() => {}),
    getProjectedJob: () => Promise.resolve({ ...job, state: "cancelled" }),
    handler: () => {
      handled += 1;
      return Promise.resolve({});
    },
  });

  await new Promise((resolve) => setTimeout(resolve, 5));
  await loop.stop();

  assertEquals(acked, 1);
  assertEquals(handled, 0);
});

Deno.test("startQueueWorkerLoop maintains progress while unkeyed work runs", async () => {
  let progressAcks = 0;
  let acked = 0;
  const job: Job = {
    id: "job-long",
    service: "svc",
    type: "refresh",
    state: "pending",
    context: jobContext,
    payload: {},
    createdAt: "2024-01-01T00:00:00.000Z",
    updatedAt: "2024-01-01T00:00:00.000Z",
    tries: 0,
    maxTries: 5,
  };
  const loop = await startQueueWorkerLoop({
    manager: new JobManager({ nc: { publish: () => {} }, jobs: jobsBinding }),
    consumer: {
      consume: () =>
        Promise.resolve((async function* () {
          yield {
            data: new TextEncoder().encode(JSON.stringify(createdEvent(job))),
            subject: "trellis.work.svc.refresh",
            ack: () => {
              acked += 1;
            },
            nak: () => {},
            inProgress: () => {
              progressAcks += 1;
            },
          };
        })()),
    },
    cancelSubscription: cancelSubscription(() => {}),
    progressAckIntervalMs: 1,
    handler: async () => {
      await new Promise((resolve) => setTimeout(resolve, 5));
      return {};
    },
  });

  await new Promise((resolve) => setTimeout(resolve, 10));
  await loop.stop();
  assertEquals(acked, 1);
  assertEquals(progressAcks > 0, true);
});

Deno.test("startQueueWorkerLoop naks unexpected failures and continues", async () => {
  let acked = 0;
  let nacked = 0;
  let projectedCalls = 0;
  let handled = 0;
  const first: Job = {
    id: "job-fails",
    service: "svc",
    type: "refresh",
    state: "pending",
    context: jobContext,
    payload: { siteId: "site-1" },
    createdAt: "2024-01-01T00:00:00.000Z",
    updatedAt: "2024-01-01T00:00:00.000Z",
    tries: 0,
    maxTries: 5,
  };
  const second = { ...first, id: "job-continues" };

  const loop = await startQueueWorkerLoop({
    manager: new JobManager({ nc: { publish: () => {} }, jobs: jobsBinding }),
    consumer: {
      consume() {
        return Promise.resolve((async function* () {
          for (const job of [first, second]) {
            yield {
              data: new TextEncoder().encode(JSON.stringify(createdEvent(job))),
              subject: "trellis.work.svc.refresh",
              ack: () => {
                acked += 1;
              },
              nak: () => {
                nacked += 1;
              },
              inProgress: () => {},
            };
          }
        })());
      },
    },
    cancelSubscription: cancelSubscription(() => {}),
    getProjectedJob: () => {
      projectedCalls += 1;
      if (projectedCalls === 1) throw new Error("projection unavailable");
      return Promise.resolve(undefined);
    },
    handler: () => {
      handled += 1;
      return Promise.resolve({});
    },
  });

  await new Promise((resolve) => setTimeout(resolve, 5));
  await loop.stop();

  assertEquals(nacked, 1);
  assertEquals(acked, 1);
  assertEquals(handled, 1);
});

Deno.test("startQueueWorkerLoop prefers latest lifecycle event over stale projection", async () => {
  let acked = 0;
  let handled = 0;
  const job: Job = {
    id: "job-2",
    service: "svc",
    type: "refresh",
    state: "pending",
    context: jobContext,
    payload: { siteId: "site-2" },
    createdAt: "2024-01-01T00:00:00.000Z",
    updatedAt: "2024-01-01T00:00:00.000Z",
    tries: 0,
    maxTries: 5,
  };
  const event = {
    jobId: job.id,
    service: job.service,
    jobType: job.type,
    eventType: "created",
    state: "pending",
    context: jobContext,
    tries: 0,
    maxTries: 5,
    payload: job.payload,
    timestamp: job.createdAt,
  };

  const loop = await startQueueWorkerLoop({
    manager: new JobManager({ nc: { publish: () => {} }, jobs: jobsBinding }),
    consumer: {
      consume() {
        return Promise.resolve((async function* () {
          yield {
            data: new TextEncoder().encode(JSON.stringify(event)),
            subject: "trellis.work.svc.refresh",
            ack: () => {
              acked += 1;
            },
            nak: () => {},
            inProgress: () => {},
          };
        })());
      },
    },
    cancelSubscription: cancelSubscription(() => {}),
    getLatestLifecycleEvent: () =>
      Promise.resolve({
        ...event,
        eventType: "started",
        state: "active",
      }),
    getProjectedJob: () => Promise.resolve({ ...job, state: "cancelled" }),
    handler: () => {
      handled += 1;
      return Promise.resolve({});
    },
  });

  await new Promise((resolve) => setTimeout(resolve, 5));
  await loop.stop();

  assertEquals(acked, 1);
  assertEquals(handled, 1);
});

Deno.test("final retry awaits the max-delivery advisory", async () => {
  let acked = 0;
  let nacked = 0;
  const published: unknown[] = [];
  const job: Job = {
    id: "job-redelivery",
    service: "svc",
    type: "refresh",
    state: "pending",
    context: jobContext,
    payload: { siteId: "site-redelivery" },
    createdAt: "2024-01-01T00:00:00.000Z",
    updatedAt: "2024-01-01T00:00:00.000Z",
    tries: 0,
    maxTries: 2,
  };
  const event = createdEvent(job);

  const loop = await startQueueWorkerLoop({
    manager: new JobManager({
      nc: {
        publish(_subject, payload) {
          published.push(JSON.parse(new TextDecoder().decode(payload)));
        },
      },
      jobs: jobsBinding,
    }),
    consumer: {
      consume() {
        return Promise.resolve((async function* () {
          yield {
            data: new TextEncoder().encode(JSON.stringify(event)),
            subject: "trellis.work.svc.refresh",
            info: { redeliveryCount: 1 },
            ack: () => {
              acked += 1;
            },
            nak: () => {
              nacked += 1;
            },
            inProgress: () => {},
          };
        })());
      },
    },
    cancelSubscription: cancelSubscription(() => {}),
    getLatestLifecycleEvent: () =>
      Promise.resolve({
        ...event,
        eventType: "retry",
        state: "retry",
        tries: 1,
      }),
    handler: () => {
      throw JobProcessError.retryable("retry again");
    },
  });

  await new Promise((resolve) => setTimeout(resolve, 5));
  await loop.stop();

  assertEquals(acked, 0);
  assertEquals(nacked, 0);
  const terminal = published.at(-1) as { eventType?: string; tries?: number };
  assertEquals(terminal.eventType, "retry");
  assertEquals(terminal.tries, 2);
});

Deno.test("startQueueWorkerLoop cleans queued key state for terminal lifecycle before ack", async () => {
  let acked = 0;
  let handled = 0;
  let removed = 0;
  const job: Job = {
    id: "job-skipped",
    service: "svc",
    type: "sync",
    state: "pending",
    context: jobContext,
    payload: { tenant: "a" },
    createdAt: "2024-01-01T00:00:00.000Z",
    updatedAt: "2024-01-01T00:00:00.000Z",
    tries: 0,
    maxTries: 5,
  };
  const event = createdEvent(job);
  const coordinator: JobKeyCoordinator = {
    admitCreate: () => Promise.reject(new Error("unexpected admit")),
    restoreReplacedQueuedJob: () =>
      Promise.reject(new Error("unexpected restore")),
    removeQueuedJob: () => {
      removed += 1;
      return Promise.resolve({
        kind: "removed",
        state: {
          version: 1,
          service: "svc",
          jobType: "sync",
          key: "a",
          keyHash: "hash",
          maxActive: 1,
          active: [],
          queued: [],
          staleTakeoverCount: 0,
          updatedAt: "2024-01-01T00:00:00.000Z",
        },
      });
    },
    acquireActiveSlot: () => Promise.reject(new Error("unexpected acquire")),
    renewHeartbeat: () => Promise.reject(new Error("unexpected renew")),
    releaseActiveSlot: () => Promise.reject(new Error("unexpected release")),
  };

  const loop = await startQueueWorkerLoop({
    manager: new JobManager({
      nc: { publish: () => {} },
      jobs: keyedJobsBinding,
      keyCoordinator: coordinator,
    }),
    consumer: {
      consume() {
        return Promise.resolve((async function* () {
          yield {
            data: new TextEncoder().encode(JSON.stringify(event)),
            subject: "trellis.work.svc.sync",
            ack: () => {
              acked += 1;
            },
            nak: () => {},
            inProgress: () => {},
          };
        })());
      },
    },
    cancelSubscription: cancelSubscription(() => {}),
    getLatestLifecycleEvent: () =>
      Promise.resolve({
        ...event,
        eventType: "skipped",
        state: "skipped",
      }),
    handler: () => {
      handled += 1;
      return Promise.resolve({});
    },
  });

  await new Promise((resolve) => setTimeout(resolve, 5));
  await loop.stop();

  assertEquals(acked, 1);
  assertEquals(handled, 0);
  assertEquals(removed, 1);
});

Deno.test("startQueueWorkerLoop acks terminal work when keyed cleanup fails", async () => {
  let acked = 0;
  let nacked = 0;
  let fallbackCleanup = 0;
  const job: Job = {
    id: "job-old-payload",
    service: "svc",
    type: "sync",
    state: "pending",
    context: jobContext,
    payload: { input: { tenant: "a" } },
    createdAt: "2024-01-01T00:00:00.000Z",
    updatedAt: "2024-01-01T00:00:00.000Z",
    tries: 0,
    maxTries: 5,
  };
  const event = createdEvent(job);
  const coordinator: JobKeyCoordinator = {
    admitCreate: () => Promise.reject(new Error("unexpected admit")),
    restoreReplacedQueuedJob: () =>
      Promise.reject(new Error("unexpected restore")),
    removeQueuedJob: () => Promise.reject(new Error("old payload schema")),
    removeQueuedJobById: () => {
      fallbackCleanup += 1;
      return Promise.reject(new Error("coordinator unavailable"));
    },
    acquireActiveSlot: () => Promise.reject(new Error("unexpected acquire")),
    renewHeartbeat: () => Promise.reject(new Error("unexpected renew")),
    releaseActiveSlot: () => Promise.reject(new Error("unexpected release")),
  };

  const loop = await startQueueWorkerLoop({
    manager: new JobManager({
      nc: { publish: () => {} },
      jobs: keyedJobsBinding,
      keyCoordinator: coordinator,
    }),
    consumer: {
      consume() {
        return Promise.resolve((async function* () {
          yield {
            data: new TextEncoder().encode(JSON.stringify(event)),
            subject: "trellis.work.svc.sync",
            ack: () => {
              acked += 1;
            },
            nak: () => {
              nacked += 1;
            },
            inProgress: () => {},
          };
        })());
      },
    },
    cancelSubscription: cancelSubscription(() => {}),
    getLatestLifecycleEvent: () =>
      Promise.resolve({
        ...event,
        eventType: "completed",
        state: "completed",
      }),
    handler: () => Promise.reject(new Error("terminal work was handled")),
  });

  await new Promise((resolve) => setTimeout(resolve, 5));
  await loop.stop();

  assertEquals(fallbackCleanup, 1);
  assertEquals(acked, 1);
  assertEquals(nacked, 0);
});

Deno.test("startQueueWorkerLoop holds keyed active-limit deferrals with progress", async () => {
  const nakDelays: Array<number | undefined> = [];
  let progressAcks = 0;
  let acquireCalls = 0;
  let handled = 0;
  const job: Job = {
    id: "job-deferred",
    service: "svc",
    type: "sync",
    state: "pending",
    context: jobContext,
    payload: { tenant: "a" },
    createdAt: "2024-01-01T00:00:00.000Z",
    updatedAt: "2024-01-01T00:00:00.000Z",
    tries: 0,
    maxTries: 5,
  };
  const event = createdEvent(job);
  const coordinator: JobKeyCoordinator = {
    admitCreate: () => Promise.reject(new Error("unexpected admit")),
    restoreReplacedQueuedJob: () =>
      Promise.reject(new Error("unexpected restore")),
    removeQueuedJob: () => Promise.reject(new Error("unexpected remove")),
    acquireActiveSlot: () => {
      acquireCalls += 1;
      return Promise.resolve(
        acquireCalls === 1
          ? {
            kind: "blocked",
            key: "a",
            reason: "active-limit",
            active: 1,
            queued: 1,
            limit: 1,
          }
          : {
            kind: "acquired",
            key: "a",
            keyHash: "hash",
            slotToken: "slot-1",
            stale: [],
            state: {
              version: 1,
              service: "svc",
              jobType: "sync",
              key: "a",
              keyHash: "hash",
              maxActive: 1,
              active: [],
              queued: [],
              staleTakeoverCount: 0,
              updatedAt: "2024-01-01T00:00:00.000Z",
            },
          },
      );
    },
    renewHeartbeat: () => Promise.reject(new Error("unexpected renew")),
    releaseActiveSlot: () => Promise.resolve({ kind: "staleCompletion" }),
  };

  const loop = await startQueueWorkerLoop({
    manager: new JobManager({
      nc: { publish: () => {} },
      jobs: keyedJobsBinding,
      keyCoordinator: coordinator,
    }),
    consumer: {
      consume() {
        return Promise.resolve((async function* () {
          yield {
            data: new TextEncoder().encode(JSON.stringify(event)),
            subject: "trellis.work.svc.sync",
            ack: () => {},
            nak: (delay?: number) => {
              nakDelays.push(delay);
            },
            inProgress: () => {
              progressAcks += 1;
            },
          };
        })());
      },
    },
    cancelSubscription: cancelSubscription(() => {}),
    progressAckIntervalMs: 1,
    handler: () => {
      handled += 1;
      return Promise.resolve({});
    },
  });

  await new Promise((resolve) => setTimeout(resolve, 5));
  await loop.stop();

  assertEquals(handled, 1);
  assertEquals(progressAcks > 0, true);
  assertEquals(nakDelays, []);
});

Deno.test("startQueueWorkerLoop processes keyed manual retried work", async () => {
  let handled = 0;
  let acked = 0;
  let nacked = 0;
  const workEventTypes: string[] = [];
  const job: Job = {
    id: "job-retried",
    service: "svc",
    type: "sync",
    state: "pending",
    context: jobContext,
    payload: { tenant: "a" },
    createdAt: "2024-01-01T00:00:00.000Z",
    updatedAt: "2024-01-01T00:00:00.000Z",
    tries: 0,
    maxTries: 5,
  };
  const event = {
    ...createdEvent(job),
    eventType: "retried" as const,
    previousState: "failed" as const,
  };
  const keyState: JobKeyState = {
    version: 1,
    service: "svc",
    jobType: "sync",
    key: "a",
    keyHash: "hash",
    maxActive: 1,
    active: [],
    queued: [],
    staleTakeoverCount: 0,
    updatedAt: "2024-01-01T00:00:00.000Z",
  };
  const coordinator: JobKeyCoordinator = {
    admitCreate: () => Promise.reject(new Error("unexpected admit")),
    restoreReplacedQueuedJob: () =>
      Promise.reject(new Error("unexpected restore")),
    removeQueuedJob: () => Promise.reject(new Error("unexpected remove")),
    acquireActiveSlot: (request) => {
      workEventTypes.push(request.workEventType ?? "missing");
      return Promise.resolve({
        kind: "acquired",
        key: "a",
        keyHash: "hash",
        slotToken: "slot-1",
        stale: [],
        state: keyState,
      });
    },
    renewHeartbeat: () => Promise.reject(new Error("unexpected renew")),
    releaseActiveSlot: () =>
      Promise.resolve({ kind: "released", state: keyState }),
  };

  const loop = await startQueueWorkerLoop({
    manager: new JobManager({
      nc: { publish: () => {} },
      jobs: keyedJobsBinding,
      keyCoordinator: coordinator,
    }),
    consumer: {
      consume() {
        return Promise.resolve((async function* () {
          yield {
            data: new TextEncoder().encode(JSON.stringify(event)),
            subject: "trellis.work.svc.sync",
            ack: () => {
              acked += 1;
            },
            nak: () => {
              nacked += 1;
            },
            inProgress: () => {},
          };
        })());
      },
    },
    cancelSubscription: cancelSubscription(() => {}),
    getLatestLifecycleEvent: () => Promise.resolve(event),
    handler: () => {
      handled += 1;
      return Promise.resolve({});
    },
  });

  await new Promise((resolve) => setTimeout(resolve, 5));
  await loop.stop();

  assertEquals(handled, 1);
  assertEquals(acked, 1);
  assertEquals(nacked, 0);
  assertEquals(workEventTypes, ["retried"]);
});

Deno.test("startNatsWorkerHostFromBinding reads approved existing consumer only", async () => {
  const requestedConsumers: Array<{ stream: string; consumerName: string }> =
    [];

  const worker = await startNatsWorkerHostFromBinding({
    jobs: jobsBinding,
    workStream: "JOBS_WORK",
  }, {
    nats: {
      subscribe(): ReturnType<NatsConnection["subscribe"]> {
        return cancelSubscription(() => {}) as ReturnType<
          NatsConnection["subscribe"]
        >;
      },
    },
    jsm: {
      consumers: {
        info(stream, consumerName) {
          requestedConsumers.push({ stream, consumerName });
          return Promise.resolve({ config: {} });
        },
      },
    },
    js: {
      consumers: {
        getConsumerFromInfo() {
          return {
            consume() {
              return Promise.resolve((async function* () {})());
            },
          };
        },
      },
    },
    manager: new JobManager({ nc: { publish: () => {} }, jobs: jobsBinding }),
    instanceId: "worker-1",
    queueTypes: ["refresh"],
    handler: () => Promise.resolve({}),
  });

  await worker.stop();

  assertEquals(requestedConsumers, [{
    stream: "JOBS_WORK",
    consumerName: "svc-refresh",
  }]);
});

function createdEvent(job: Job) {
  return {
    jobId: job.id,
    service: job.service,
    jobType: job.type,
    eventType: "created" as const,
    state: "pending" as const,
    context: job.context,
    tries: 0,
    maxTries: job.maxTries,
    payload: job.payload,
    timestamp: job.createdAt,
  };
}

Deno.test("startNatsWorkerHostFromBinding fails closed when approved consumer is missing", async () => {
  await assertRejects(
    () =>
      startNatsWorkerHostFromBinding({
        jobs: jobsBinding,
        workStream: "JOBS_WORK",
      }, {
        nats: {
          subscribe(): ReturnType<NatsConnection["subscribe"]> {
            return cancelSubscription(() => {}) as ReturnType<
              NatsConnection["subscribe"]
            >;
          },
        },
        jsm: {
          consumers: {
            info() {
              const error = new Error("consumer not found");
              error.name = "ConsumerNotFoundError";
              return Promise.reject(error);
            },
          },
        },
        js: {
          consumers: {
            getConsumerFromInfo() {
              throw new Error("consumer should not be built");
            },
          },
        },
        manager: new JobManager({
          nc: { publish: () => {} },
          jobs: jobsBinding,
        }),
        instanceId: "worker-1",
        queueTypes: ["refresh"],
        handler: () => Promise.resolve({}),
      }),
    JobsInfrastructureMissingError,
    "Jobs work stream 'JOBS_WORK' was not found while starting queue 'refresh'",
  );
});
