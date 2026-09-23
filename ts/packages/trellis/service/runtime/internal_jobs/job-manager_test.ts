import type { MsgHdrs } from "@nats-io/nats-core";
import {
  assertEquals,
  assertExists,
  assertInstanceOf,
  assertMatch,
  assertRejects,
} from "@std/assert";
import { AsyncResult } from "@oatscenter/result";
import { context, metrics, propagation, trace } from "@opentelemetry/api";
import { AsyncLocalStorageContextManager } from "@opentelemetry/context-async-hooks";
import { W3CTraceContextPropagator } from "@opentelemetry/core";
import {
  AggregationTemporality,
  InMemoryMetricExporter,
  MeterProvider,
  PeriodicExportingMetricReader,
} from "npm:@opentelemetry/sdk-metrics@^2.7.0";
import {
  BasicTracerProvider,
  InMemorySpanExporter,
  SimpleSpanProcessor,
} from "@opentelemetry/sdk-trace-base";

import {
  JobNotEnqueuedError,
  JobRef,
  runWithActiveJobContext,
} from "../../../jobs.ts";
import type { JobsBinding } from "./bindings.ts";
import {
  JobCancellationToken,
  JobManager,
  JobProcessError,
} from "./job-manager.ts";
import type { JobKeyCoordinator } from "./key-coordinator.ts";
import type { Job, JobContext } from "./types.ts";

type PublishedMessage = {
  subject: string;
  payload: Uint8Array;
  headers?: MsgHdrs;
};

const TRACEPARENT_PATTERN =
  /^[0-9a-f]{2}-[0-9a-f]{32}-[0-9a-f]{16}-[0-9a-f]{2}$/;

const jobContext: JobContext = {
  requestId: "request-1",
  traceId: "0123456789abcdef0123456789abcdef",
  traceparent: "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01",
};

function unsupportedCoordinator(): JobKeyCoordinator {
  return {
    admitCreate: () => Promise.reject(new Error("unexpected admit")),
    restoreReplacedQueuedJob: () =>
      Promise.reject(new Error("unexpected restore")),
    removeQueuedJob: () => Promise.reject(new Error("unexpected remove")),
    acquireActiveSlot: () => Promise.reject(new Error("unexpected acquire")),
    renewHeartbeat: () => Promise.reject(new Error("unexpected renew")),
    releaseActiveSlot: () => Promise.reject(new Error("unexpected release")),
  };
}

Deno.test("JobManager creates and publishes job context", async () => {
  const metricExporter = new InMemoryMetricExporter(
    AggregationTemporality.CUMULATIVE,
  );
  const reader = new PeriodicExportingMetricReader({
    exporter: metricExporter,
    exportIntervalMillis: 60_000,
  });
  metrics.setGlobalMeterProvider(new MeterProvider({ readers: [reader] }));
  const spanExporter = new InMemorySpanExporter();
  trace.setGlobalTracerProvider(
    new BasicTracerProvider({
      spanProcessors: [new SimpleSpanProcessor(spanExporter)],
    }),
  );
  propagation.setGlobalPropagator(new W3CTraceContextPropagator());
  context.setGlobalContextManager(new AsyncLocalStorageContextManager());
  const published: PublishedMessage[] = [];
  const manager = new JobManager<{ siteId: string }, { ok: boolean }>({
    nc: {
      publish(subject, payload, opts) {
        published.push({ subject, payload, headers: opts?.headers });
      },
    },
    jobs: {
      serviceName: "trellis/svc",
      namespace: "svc",
      queues: {
        refresh: {
          queueType: "refresh",
          publishPrefix: "trellis.jobs.svc.refresh",
          workSubject: "trellis.work.svc.refresh",
          consumerName: "svc-refresh",
          payload: { schema: "RefreshPayload" },
          result: { schema: "RefreshResult" },
          maxDeliver: 3,
          backoffMs: [],
          ackWaitMs: 1_000,
        },
      },
    },
    meta: {
      nextJobId: () => "job-1",
      nowIso: () => "2024-01-01T00:00:00.000Z",
    },
  });

  const job = await manager.create("refresh", { siteId: "site-1" });
  assertEquals(job.service, "trellis/svc");
  assertEquals(job.context.requestId.length > 0, true);
  assertMatch(job.context.traceparent, TRACEPARENT_PATTERN);
  assertEquals(job.context.traceId, job.context.traceparent.slice(3, 35));

  assertEquals(published.length, 1);
  const created = JSON.parse(
    new TextDecoder().decode(published[0].payload),
  ) as {
    context?: typeof job.context;
    service?: string;
  };
  assertEquals(created.context, job.context);
  assertEquals(created.service, "trellis/svc");
  assertEquals(published[0].subject, "trellis.jobs.svc.refresh.job-1.created");
  assertEquals(published[0].headers?.get("request-id"), job.context.requestId);
  assertEquals(
    published[0].headers?.get("traceparent"),
    job.context.traceparent,
  );

  let release!: () => void;
  const waiting = new Promise<void>((resolve) => {
    release = resolve;
  });
  let entered!: () => void;
  const running = new Promise<void>((resolve) => {
    entered = resolve;
  });
  const processing = manager.processWithHeartbeat(
    job,
    new JobCancellationToken(),
    async () => {},
    async (activeJob) => {
      entered();
      await waiting;
      assertEquals(activeJob.context(), job.context);
      const child = trace.getTracer("test").startSpan("job-child");
      child.end();
      return { ok: true };
    },
  );
  await running;
  const [earlyStart] = spanExporter.getFinishedSpans().filter((span) =>
    span.name === "trellis.job.attempt.start"
  );
  assertExists(earlyStart);
  release();
  const outcome = await processing;

  assertEquals(outcome.outcome, "completed");
  await reader.forceFlush();
  const attempts = metricExporter.getMetrics().flatMap((resource) =>
    resource.scopeMetrics.flatMap((scope) => scope.metrics)
  ).filter((metric) =>
    metric.descriptor.name === "trellis.job.attempt.duration"
  );
  assertEquals(attempts.length, 1);
  assertEquals(attempts[0].dataPoints[0].attributes, {
    "trellis.route": "refresh",
    "trellis.outcome": "completed",
  });
  const [start] = spanExporter.getFinishedSpans().filter((span) =>
    span.name === "trellis.job.attempt.start"
  );
  const [finish] = spanExporter.getFinishedSpans().filter((span) =>
    span.name === "trellis.job.attempt.finish"
  );
  assertExists(start);
  assertExists(finish);
  assertEquals(start.parentSpanContext, undefined);
  assertEquals(start.links[0]?.context.traceId, job.context.traceId);
  assertEquals(finish.parentSpanContext?.spanId, start.spanContext().spanId);
  assertEquals(finish.links[0]?.context.traceId, job.context.traceId);
  const [child] = spanExporter.getFinishedSpans().filter((span) =>
    span.name === "job-child"
  );
  assertEquals(child?.parentSpanContext?.spanId, start.spanContext().spanId);
  metrics.disable();
  trace.disable();
  propagation.disable();
  context.disable();
  assertEquals(published.length, 3);
  for (const message of published) {
    const event = JSON.parse(new TextDecoder().decode(message.payload)) as {
      context?: typeof job.context;
    };
    assertEquals(event.context, job.context);
    assertExists(message.headers);
    assertEquals(message.headers.get("request-id"), job.context.requestId);
    assertEquals(message.headers.get("traceparent"), job.context.traceparent);
  }
});

Deno.test("JobManager preserves structured failure error string", async () => {
  const published: PublishedMessage[] = [];
  const manager = new JobManager<{ siteId: string }, { ok: boolean }>({
    nc: {
      publish(subject, payload, opts) {
        published.push({ subject, payload, headers: opts?.headers });
      },
    },
    jobs: {
      serviceName: "svc",
      namespace: "svc",
      queues: {
        refresh: {
          queueType: "refresh",
          publishPrefix: "trellis.jobs.svc.refresh",
          workSubject: "trellis.work.svc.refresh",
          consumerName: "svc-refresh",
          payload: { schema: "RefreshPayload" },
          result: { schema: "RefreshResult" },
          maxDeliver: 1,
          backoffMs: [],
          ackWaitMs: 1_000,
        },
      },
    },
    meta: {
      nextJobId: () => "job-structured-failure",
      nowIso: () => "2024-01-01T00:00:00.000Z",
    },
  });
  const job = await manager.create("refresh", { siteId: "site-1" });
  const serializedError = JSON.stringify({
    id: "err-1",
    type: "AuthError",
    message: "Auth failed: forbidden",
    context: {
      jobType: "refresh",
      service: "svc",
      contractId: "jobs.test@v1",
      contractDigest: "digest-1",
      requestId: job.context.requestId,
    },
    traceId: job.context.traceId,
    reason: "forbidden",
  });

  const outcome = await manager.processWithHeartbeat(
    job,
    new JobCancellationToken(),
    async () => {},
    async () => {
      throw JobProcessError.failed(serializedError);
    },
  );

  assertEquals(outcome.outcome, "failed");
  if (outcome.outcome !== "failed") return;
  assertEquals(JSON.parse(outcome.error), JSON.parse(serializedError));

  const failedEvent = JSON.parse(
    new TextDecoder().decode(published[published.length - 1]!.payload),
  ) as { error?: string };
  assertEquals(
    JSON.parse(failedEvent.error ?? ""),
    JSON.parse(serializedError),
  );
});

Deno.test("JobManager child jobs inherit active job lineage", async () => {
  const published: PublishedMessage[] = [];
  let childId = 0;
  const manager = new JobManager<{ siteId: string }, { ok: boolean }>({
    nc: {
      publish(subject, payload, opts) {
        published.push({ subject, payload, headers: opts?.headers });
      },
    },
    jobs: {
      serviceName: "svc",
      namespace: "svc",
      queues: {
        refresh: {
          queueType: "refresh",
          publishPrefix: "trellis.jobs.svc.refresh",
          workSubject: "trellis.work.svc.refresh",
          consumerName: "svc-refresh",
          payload: { schema: "RefreshPayload" },
          result: { schema: "RefreshResult" },
          maxDeliver: 3,
          backoffMs: [],
          ackWaitMs: 1_000,
        },
      },
    },
    meta: {
      nextJobId: () => `child-${++childId}`,
      nowIso: () => "2024-01-01T00:00:00.000Z",
    },
  });
  const parent: Job<{ siteId: string }, { ok: boolean }> = {
    id: "parent-1",
    service: "svc",
    type: "refresh",
    state: "pending",
    context: jobContext,
    payload: { siteId: "parent" },
    createdAt: "2024-01-01T00:00:00.000Z",
    updatedAt: "2024-01-01T00:00:00.000Z",
    tries: 0,
    maxTries: 3,
    lineage: { rootJobId: "root-1", operationId: "operation-1" },
  };

  const outcome = await manager.processWithHeartbeat(
    parent,
    new JobCancellationToken(),
    async () => {},
    async () => {
      await manager.create("refresh", { siteId: "child" });
      return { ok: true };
    },
  );

  assertEquals(outcome.outcome, "completed");
  const childCreated = published.map((message) =>
    JSON.parse(new TextDecoder().decode(message.payload)) as {
      eventType: string;
      context: JobContext;
      trigger?: Record<string, unknown>;
      lineage?: Record<string, unknown>;
    }
  ).find((event) => event.eventType === "created");
  assertExists(childCreated);
  assertEquals(childCreated.context, jobContext);
  assertEquals(childCreated.trigger, {
    kind: "parentJob",
    parentJobId: "parent-1",
    operationId: "operation-1",
    traceId: jobContext.traceId,
    requestId: jobContext.requestId,
  });
  assertEquals(childCreated.lineage, {
    parentJobId: "parent-1",
    rootJobId: "root-1",
    operationId: "operation-1",
  });
});

Deno.test("JobManager active waitFor publishes wait edge and preserves result or error", async () => {
  const published: PublishedMessage[] = [];
  const manager = new JobManager<{ siteId: string }, { ok: boolean }>({
    nc: {
      publish(subject, payload, opts) {
        published.push({ subject, payload, headers: opts?.headers });
      },
    },
    jobs: {
      serviceName: "svc",
      namespace: "svc",
      queues: {
        refresh: {
          queueType: "refresh",
          publishPrefix: "trellis.jobs.svc.refresh",
          workSubject: "trellis.work.svc.refresh",
          consumerName: "svc-refresh",
          payload: { schema: "RefreshPayload" },
          result: { schema: "RefreshResult" },
          maxDeliver: 3,
          backoffMs: [],
          ackWaitMs: 1_000,
        },
      },
    },
    meta: {
      nextJobId: () => "unused",
      nowIso: () => "2024-01-01T00:00:00.000Z",
    },
  });
  const active: Job<{ siteId: string }, { ok: boolean }> = {
    id: "job-1",
    service: "svc",
    type: "refresh",
    state: "active",
    context: jobContext,
    payload: { siteId: "site-1" },
    createdAt: "2024-01-01T00:00:00.000Z",
    updatedAt: "2024-01-01T00:00:00.000Z",
    tries: 1,
    maxTries: 3,
  };

  const value = await manager.withActiveJobAndHeartbeat(
    active,
    new JobCancellationToken(),
    async () => {},
    (job) => job.waitFor({ kind: "external", label: "remote API" }, () => 7),
  );
  assertEquals(value, 7);
  assertEquals(published.map(eventType), ["waiting", "resumed"]);

  published.length = 0;
  const thrown = await assertRejects(
    () =>
      manager.withActiveJobAndHeartbeat(
        active,
        new JobCancellationToken(),
        async () => {},
        (job) =>
          job.waitFor({ kind: "external", label: "remote API" }, () => {
            throw new Error("external failed");
          }),
      ),
    Error,
    "external failed",
  );
  assertEquals(thrown.message, "external failed");
  assertEquals(published.map(eventType), ["waiting", "resumed"]);
});

Deno.test("JobRef wait inside active job uses active wait edge", async () => {
  const calls: string[] = [];
  const terminal = {
    id: "child-1",
    service: "svc",
    type: "refresh",
    state: "completed" as const,
    context: jobContext,
    payload: { siteId: "child" },
    result: { ok: true },
    createdAt: "2024-01-01T00:00:00.000Z",
    updatedAt: "2024-01-01T00:00:00.000Z",
    completedAt: "2024-01-01T00:00:00.000Z",
    tries: 1,
    maxTries: 3,
  };
  const ref = new JobRef<{ siteId: string }, { ok: boolean }>(
    { id: "child-1", service: "svc", jobType: "refresh" },
    {
      get: () => AsyncResult.ok(terminal),
      wait: () => AsyncResult.ok(terminal),
      cancel: () => AsyncResult.ok(terminal),
    },
  );

  const result = await runWithActiveJobContext({
    job: {
      id: "parent-1",
      service: "svc",
      type: "refresh",
      state: "active",
      context: jobContext,
      payload: { siteId: "parent" },
      createdAt: "2024-01-01T00:00:00.000Z",
      updatedAt: "2024-01-01T00:00:00.000Z",
      tries: 1,
      maxTries: 3,
    },
    waitFor: async (target, fn) => {
      calls.push(
        `${target.kind}:${target.service}:${target.type}:${target.id}`,
      );
      return await fn();
    },
  }, async () => await ref.wait().orThrow());

  assertEquals(result, terminal);
  assertEquals(calls, ["job:svc:refresh:child-1"]);
});

Deno.test("JobManager leaves max-delivery exhaustion to the advisory", async () => {
  const published: PublishedMessage[] = [];
  const manager = new JobManager<{ siteId: string }, { ok: boolean }>({
    nc: {
      publish(subject, payload, opts) {
        published.push({ subject, payload, headers: opts?.headers });
      },
    },
    jobs: {
      serviceName: "svc",
      namespace: "svc",
      queues: {
        refresh: {
          queueType: "refresh",
          publishPrefix: "trellis.jobs.svc.refresh",
          workSubject: "trellis.work.svc.refresh",
          consumerName: "svc-refresh",
          payload: { schema: "RefreshPayload" },
          result: { schema: "RefreshResult" },
          maxDeliver: 1,
          backoffMs: [],
          ackWaitMs: 1_000,
        },
      },
    },
    meta: {
      nextJobId: () => "job-dead",
      nowIso: () => "2024-01-01T00:00:00.000Z",
    },
  });
  const job = await manager.create("refresh", { siteId: "site-1" });

  const outcome = await manager.processWithHeartbeat(
    job,
    new JobCancellationToken(),
    async () => {},
    async () => {
      throw JobProcessError.retryable("try again");
    },
  );

  assertEquals(outcome, { outcome: "retry", tries: 1, error: "try again" });
  const retryEvent = JSON.parse(
    new TextDecoder().decode(published[published.length - 1]!.payload),
  ) as { eventType?: string; state?: string; tries?: number; error?: string };
  assertEquals(retryEvent.eventType, "retry");
  assertEquals(retryEvent.state, "retry");
  assertEquals(retryEvent.tries, 1);
  assertEquals(retryEvent.error, "try again");
});

Deno.test("JobManager keyed create rejects and prepared create publishes skipped", async () => {
  const published: PublishedMessage[] = [];
  const coordinator: JobKeyCoordinator = {
    ...unsupportedCoordinator(),
    admitCreate: () =>
      Promise.resolve({
        kind: "rejected",
        key: "tenant-a",
        reason: "queue-depth",
        active: 0,
        queued: 1,
        limit: 1,
      }),
  };
  const manager = new JobManager<{ tenant: string }, { ok: boolean }>({
    nc: {
      publish(subject, payload, opts) {
        published.push({ subject, payload, headers: opts?.headers });
      },
    },
    jobs: {
      serviceName: "svc",
      namespace: "svc",
      queues: {
        sync: {
          queueType: "sync",
          publishPrefix: "trellis.jobs.svc.sync",
          workSubject: "trellis.work.svc.sync",
          consumerName: "svc-sync",
          payload: { schema: "SyncPayload" },
          maxDeliver: 3,
          backoffMs: [],
          ackWaitMs: 1_000,
          keyConcurrency: {
            key: ["/tenant"],
            maxActive: 1,
            heartbeatIntervalMs: 30_000,
            heartbeatTtlMs: 120_000,
            stalePolicy: "fail-stale",
          },
          queue: { maxQueuedPerKey: 0, whenFull: "reject" },
        },
      },
    },
    keyCoordinator: coordinator,
    meta: {
      nextJobId: () => "job-1",
      nowIso: () => "2024-01-01T00:00:00.000Z",
    },
  });

  const error = await assertRejects(
    () => manager.create("sync", { tenant: "a" }),
    JobNotEnqueuedError,
  );
  assertInstanceOf(error, JobNotEnqueuedError);
  assertEquals(error.reason, "queue-depth");
  assertEquals(published.length, 0);

  await assertRejects(
    () =>
      manager.createPrepared({
        submissionId: "submission-1",
        mode: "create",
        service: "svc",
        queue: "sync",
        jobId: "job-prepared",
        payload: { tenant: "a" },
        createdAt: "2024-01-01T00:00:00.000Z",
        context: jobContext,
        trigger: { kind: "serviceCode" },
      }),
    JobNotEnqueuedError,
  );
  assertEquals(published.length, 1);
  const skipped = JSON.parse(
    new TextDecoder().decode(published[0]!.payload),
  );
  assertEquals(skipped.eventType, "skipped");
  assertEquals(skipped.state, "skipped");
  assertEquals(skipped.error, "job submission not enqueued: queue-depth");
});

Deno.test("JobManager submit returns keyed policy outcomes", async () => {
  const published: PublishedMessage[] = [];
  const outcomes = [
    "accepted",
    "rejected",
    "coalesced",
    "replaced",
  ] as const;
  let outcomeIndex = 0;
  const coordinator: JobKeyCoordinator = {
    ...unsupportedCoordinator(),
    admitCreate: (request) => {
      const outcome = outcomes[outcomeIndex++] ?? "accepted";
      if (outcome === "rejected") {
        return Promise.resolve({
          kind: "rejected",
          key: "tenant-a",
          reason: "active-limit",
          active: 1,
          queued: 0,
          limit: 1,
        });
      }
      if (outcome === "coalesced") {
        return Promise.resolve({
          kind: "coalesced",
          key: "tenant-a",
          existing: { service: "svc", jobType: "sync", id: "job-existing" },
          reason: "active-limit",
        });
      }
      if (outcome === "replaced") {
        return Promise.resolve({
          kind: "replaced",
          key: "tenant-a",
          keyHash: "hash",
          replaced: {
            service: "svc",
            jobType: "sync",
            id: "job-old",
            createdAt: request.createdAt,
            requestId: request.context.requestId,
            context: request.context,
          },
          state: {
            version: 1,
            service: "svc",
            jobType: "sync",
            key: "tenant-a",
            keyHash: "hash",
            maxActive: 1,
            active: [],
            queued: [],
            staleTakeoverCount: 0,
            updatedAt: request.createdAt,
          },
        });
      }
      return Promise.resolve({
        kind: "accepted",
        key: "tenant-a",
        keyHash: "hash",
        state: {
          version: 1,
          service: "svc",
          jobType: "sync",
          key: "tenant-a",
          keyHash: "hash",
          maxActive: 1,
          active: [],
          queued: [],
          staleTakeoverCount: 0,
          updatedAt: request.createdAt,
        },
      });
    },
  };
  let id = 0;
  const manager = new JobManager<{ tenant: string }, { ok: boolean }>({
    nc: {
      publish(subject, payload, opts) {
        published.push({ subject, payload, headers: opts?.headers });
      },
    },
    jobs: keyedJobsBinding(),
    keyCoordinator: coordinator,
    meta: {
      nextJobId: () => `job-${++id}`,
      nowIso: () => "2024-01-01T00:00:00.000Z",
    },
  });

  assertEquals(
    (await manager.submit("sync", { tenant: "a" })).kind,
    "accepted",
  );
  assertEquals(
    (await manager.submit("sync", { tenant: "a" })).kind,
    "rejected",
  );
  assertEquals(
    (await manager.submit("sync", { tenant: "a" })).kind,
    "coalesced",
  );
  const replaced = await manager.submit("sync", { tenant: "a" });
  assertEquals(replaced.kind, "replaced");
  const eventTypes = published.map((message) =>
    (JSON.parse(new TextDecoder().decode(message.payload)) as {
      eventType: string;
    })
      .eventType
  );
  assertEquals(eventTypes, ["created", "skipped", "created"]);
});

Deno.test("JobManager create rejects coalesce and replace-oldest policy outcomes", async () => {
  const outcomes = ["coalesced", "replaced"] as const;
  let outcomeIndex = 0;
  let restored = 0;
  const coordinator: JobKeyCoordinator = {
    ...unsupportedCoordinator(),
    restoreReplacedQueuedJob: () => {
      restored += 1;
      return Promise.resolve({ kind: "restored", state: emptyKeyState() });
    },
    admitCreate: (request) => {
      const outcome = outcomes[outcomeIndex++];
      if (outcome === "replaced") {
        return Promise.resolve({
          kind: "replaced",
          key: "tenant-a",
          keyHash: "hash",
          replaced: {
            service: "svc",
            jobType: "sync",
            id: "job-old",
            createdAt: request.createdAt,
            requestId: request.context.requestId,
            context: request.context,
          },
          state: emptyKeyState(),
        });
      }
      return Promise.resolve({
        kind: "coalesced",
        key: "tenant-a",
        existing: { service: "svc", jobType: "sync", id: "job-existing" },
        reason: "active-limit",
      });
    },
  };
  const manager = new JobManager<{ tenant: string }, { ok: boolean }>({
    nc: { publish: () => {} },
    jobs: keyedJobsBinding(),
    keyCoordinator: coordinator,
    meta: {
      nextJobId: () => "job-new",
      nowIso: () => "2024-01-01T00:00:00.000Z",
    },
  });

  const coalesced = await assertRejects(
    () => manager.create("sync", { tenant: "a" }),
    JobNotEnqueuedError,
  );
  assertEquals(coalesced.reason, "coalesced");

  const replaced = await assertRejects(
    () => manager.create("sync", { tenant: "a" }),
    JobNotEnqueuedError,
  );
  assertEquals(replaced.reason, "queue-depth");
  assertEquals(restored, 1);
});

Deno.test("JobManager restores replaced queued reservation when skipped publish fails", async () => {
  let restored = 0;
  const coordinator: JobKeyCoordinator = {
    ...unsupportedCoordinator(),
    admitCreate: (request) =>
      Promise.resolve({
        kind: "replaced",
        key: "tenant-a",
        keyHash: "hash",
        replaced: {
          service: "svc",
          jobType: "sync",
          id: "job-old",
          createdAt: "2024-01-01T00:00:00.000Z",
          requestId: request.context.requestId,
          context: request.context,
        },
        state: emptyKeyState(),
      }),
    restoreReplacedQueuedJob: (args) => {
      restored += 1;
      assertEquals(args.replacementJobId, "job-new");
      assertEquals(args.replaced.id, "job-old");
      return Promise.resolve({ kind: "restored", state: emptyKeyState() });
    },
  };
  const manager = new JobManager<{ tenant: string }, { ok: boolean }>({
    nc: {
      publish(subject) {
        if (subject.endsWith(".skipped")) {
          throw new Error("skipped publish failed");
        }
      },
    },
    jobs: keyedJobsBinding(),
    keyCoordinator: coordinator,
    meta: {
      nextJobId: () => "job-new",
      nowIso: () => "2024-01-01T00:00:00.000Z",
    },
  });

  await assertRejects(
    () => manager.submit("sync", { tenant: "a" }),
    Error,
    "skipped publish failed",
  );
  assertEquals(restored, 1);
});

Deno.test("JobManager removes queued reservation when created publish fails", async () => {
  let removed = 0;
  const coordinator: JobKeyCoordinator = {
    ...unsupportedCoordinator(),
    admitCreate: (request) =>
      Promise.resolve({
        kind: "accepted",
        key: "tenant-a",
        keyHash: "hash",
        state: { ...emptyKeyState(), updatedAt: request.createdAt },
      }),
    removeQueuedJob: () => {
      removed += 1;
      return Promise.resolve({ kind: "removed", state: emptyKeyState() });
    },
  };
  const manager = new JobManager<{ tenant: string }, { ok: boolean }>({
    nc: {
      publish() {
        throw new Error("publish failed");
      },
    },
    jobs: keyedJobsBinding(),
    keyCoordinator: coordinator,
    meta: {
      nextJobId: () => "job-1",
      nowIso: () => "2024-01-01T00:00:00.000Z",
    },
  });

  await assertRejects(
    () => manager.submit("sync", { tenant: "a" }),
    Error,
    "publish failed",
  );
  assertEquals(removed, 1);
});

Deno.test("JobManager renews keyed leases independently from delivery progress", async () => {
  const published: PublishedMessage[] = [];
  let renewed = 0;
  let released = 0;
  const coordinator: JobKeyCoordinator = {
    ...unsupportedCoordinator(),
    acquireActiveSlot: () =>
      Promise.resolve({
        kind: "acquired",
        key: "tenant-a",
        keyHash: "hash",
        slotToken: "slot-1",
        stale: [],
        state: emptyKeyState(),
      }),
    renewHeartbeat: () => {
      renewed += 1;
      return Promise.resolve({ kind: "renewed", state: emptyKeyState() });
    },
    releaseActiveSlot: () => {
      released += 1;
      return Promise.resolve({ kind: "released", state: emptyKeyState() });
    },
  };
  const binding = keyedJobsBinding();
  binding.queues.sync!.keyConcurrency!.heartbeatIntervalMs = 1;
  const manager = new JobManager<{ tenant: string }, { ok: boolean }>({
    nc: {
      publish(subject, payload, opts) {
        published.push({ subject, payload, headers: opts?.headers });
      },
    },
    jobs: binding,
    keyCoordinator: coordinator,
    meta: {
      nextJobId: () => "unused",
      nowIso: () => "2024-01-01T00:00:00.000Z",
    },
  });
  let jetstreamHeartbeats = 0;

  const outcome = await manager.processWithHeartbeat(
    keyedJob(),
    new JobCancellationToken(),
    () => {
      jetstreamHeartbeats += 1;
      return Promise.resolve();
    },
    async (job) => {
      await job.heartbeat();
      await new Promise((resolve) => setTimeout(resolve, 5));
      return { ok: true };
    },
    { instanceId: "worker-1" },
  );

  assertEquals(outcome.outcome, "completed");
  assertEquals(jetstreamHeartbeats, 1);
  assertEquals(renewed > 0, true);
  assertEquals(released, 1);
  assertEquals(published.map(eventType), ["started", "completed"]);
});

Deno.test("JobManager releases acquired slot when stale publish fails before handler", async () => {
  let released = 0;
  let handlerRan = false;
  const coordinator: JobKeyCoordinator = {
    ...unsupportedCoordinator(),
    acquireActiveSlot: () =>
      Promise.resolve({
        kind: "acquired",
        key: "tenant-a",
        keyHash: "hash",
        slotToken: "slot-1",
        stale: [{
          jobId: "job-stale",
          slotToken: "slot-stale",
          instanceId: "worker-old",
          startedAt: "2024-01-01T00:00:00.000Z",
          heartbeatAt: "2024-01-01T00:00:00.000Z",
          leaseExpiresAt: "2024-01-01T00:00:01.000Z",
          tries: 1,
          context: jobContext,
        }],
        state: emptyKeyState(),
      }),
    releaseActiveSlot: () => {
      released += 1;
      return Promise.resolve({ kind: "released", state: emptyKeyState() });
    },
  };
  const manager = new JobManager<{ tenant: string }, { ok: boolean }>({
    nc: {
      publish(subject) {
        if (subject.endsWith(".stale")) {
          throw new Error("stale publish failed");
        }
      },
    },
    jobs: keyedJobsBinding(),
    keyCoordinator: coordinator,
    meta: {
      nextJobId: () => "unused",
      nowIso: () => "2024-01-01T00:00:00.000Z",
    },
  });

  await assertRejects(
    () =>
      manager.processWithHeartbeat(
        keyedJob(),
        new JobCancellationToken(),
        () => Promise.resolve(),
        () => {
          handlerRan = true;
          return Promise.resolve({ ok: true });
        },
        { instanceId: "worker-1" },
      ),
    Error,
    "stale publish failed",
  );
  assertEquals(released, 1);
  assertEquals(handlerRan, false);
});

Deno.test("JobManager releases acquired slot when started publish fails before handler", async () => {
  let released = 0;
  let handlerRan = false;
  const coordinator: JobKeyCoordinator = {
    ...unsupportedCoordinator(),
    acquireActiveSlot: () =>
      Promise.resolve({
        kind: "acquired",
        key: "tenant-a",
        keyHash: "hash",
        slotToken: "slot-1",
        stale: [],
        state: emptyKeyState(),
      }),
    releaseActiveSlot: () => {
      released += 1;
      return Promise.resolve({ kind: "released", state: emptyKeyState() });
    },
  };
  const manager = new JobManager<{ tenant: string }, { ok: boolean }>({
    nc: {
      publish(subject) {
        if (subject.endsWith(".started")) {
          throw new Error("started publish failed");
        }
      },
    },
    jobs: keyedJobsBinding(),
    keyCoordinator: coordinator,
    meta: {
      nextJobId: () => "unused",
      nowIso: () => "2024-01-01T00:00:00.000Z",
    },
  });

  await assertRejects(
    () =>
      manager.processWithHeartbeat(
        keyedJob(),
        new JobCancellationToken(),
        () => Promise.resolve(),
        () => {
          handlerRan = true;
          return Promise.resolve({ ok: true });
        },
        { instanceId: "worker-1" },
      ),
    Error,
    "started publish failed",
  );
  assertEquals(released, 1);
  assertEquals(handlerRan, false);
});

Deno.test("JobManager publishes staleCompletionIgnored when slot is lost", async () => {
  const published: PublishedMessage[] = [];
  const coordinator: JobKeyCoordinator = {
    ...unsupportedCoordinator(),
    acquireActiveSlot: () =>
      Promise.resolve({
        kind: "acquired",
        key: "tenant-a",
        keyHash: "hash",
        slotToken: "slot-1",
        stale: [],
        state: emptyKeyState(),
      }),
    releaseActiveSlot: () => Promise.resolve({ kind: "staleCompletion" }),
  };
  const manager = new JobManager<{ tenant: string }, { ok: boolean }>({
    nc: {
      publish(subject, payload, opts) {
        published.push({ subject, payload, headers: opts?.headers });
      },
    },
    jobs: keyedJobsBinding(),
    keyCoordinator: coordinator,
    meta: {
      nextJobId: () => "unused",
      nowIso: () => "2024-01-01T00:00:00.000Z",
    },
  });

  const outcome = await manager.processWithHeartbeat(
    keyedJob(),
    new JobCancellationToken(),
    () => Promise.resolve(),
    () => Promise.resolve({ ok: true }),
    { instanceId: "worker-1" },
  );

  assertEquals(outcome.outcome, "stale_completion_ignored");
  assertEquals(published.map(eventType), ["started", "staleCompletionIgnored"]);
});

function keyedJobsBinding(): JobsBinding {
  return {
    serviceName: "svc",
    namespace: "svc",
    queues: {
      sync: {
        queueType: "sync",
        publishPrefix: "trellis.jobs.svc.sync",
        workSubject: "trellis.work.svc.sync",
        consumerName: "svc-sync",
        payload: { schema: "SyncPayload" },
        maxDeliver: 3,
        backoffMs: [],
        ackWaitMs: 1_000,
        keyConcurrency: {
          key: ["/tenant"],
          maxActive: 1,
          heartbeatIntervalMs: 30_000,
          heartbeatTtlMs: 120_000,
          stalePolicy: "fail-stale",
        },
        queue: { maxQueuedPerKey: 0, whenFull: "reject" },
      },
    },
  };
}

function keyedJob(): Job<{ tenant: string }, { ok: boolean }> {
  return {
    id: "job-1",
    service: "svc",
    type: "sync",
    state: "pending",
    context: jobContext,
    payload: { tenant: "a" },
    createdAt: "2024-01-01T00:00:00.000Z",
    updatedAt: "2024-01-01T00:00:00.000Z",
    tries: 0,
    maxTries: 3,
  };
}

function emptyKeyState() {
  return {
    version: 1 as const,
    service: "svc",
    jobType: "sync",
    key: "tenant-a",
    keyHash: "hash",
    maxActive: 1,
    active: [],
    queued: [],
    staleTakeoverCount: 0,
    updatedAt: "2024-01-01T00:00:00.000Z",
  };
}

function eventType(message: PublishedMessage): string {
  return (JSON.parse(new TextDecoder().decode(message.payload)) as {
    eventType: string;
  }).eventType;
}
