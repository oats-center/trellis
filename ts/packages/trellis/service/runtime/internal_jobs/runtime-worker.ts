import {
  jetstream,
  JetStreamError,
  jetstreamManager,
} from "@nats-io/jetstream";
import type { ConsumerInfo, JsMsg } from "@nats-io/jetstream";
import type { NatsConnection, Subscription } from "@nats-io/nats-core";

import { recordTrellisError } from "../../../telemetry/mod.ts";
import { recordCatalogCounter } from "../../../telemetry/metrics.ts";
import type { JobsQueueBinding, JobsRuntimeBinding } from "./bindings.ts";
import { ActiveJobCancellationRegistry } from "./cancellation-registry.ts";
import { startWorkerHeartbeatLoop } from "./heartbeat.ts";
import type { ActiveJob, JobProcessOutcome } from "./job-manager.ts";
import {
  JobCancellationToken,
  type JobManager,
  JobProcessError,
  startAutoHeartbeat,
} from "./job-manager.ts";
import { isTerminal, jobFromWorkEvent } from "./projection.ts";
import type { Job, JobEvent } from "./types.ts";

export type WorkerAckAction = "ack" | "nak";
export type ProjectedWorkDecision = "process" | "skip-ack";
export type SchemaRef = { schema: string };
export type PayloadValidationArgs<TResult> = {
  schema?: SchemaRef;
  job: Job<unknown, TResult>;
};

export type ResultValidationArgs<TResult> = {
  schema?: SchemaRef;
  job: Job<unknown, TResult>;
  result: TResult;
};

export class WorkerLoopStopError extends AggregateError {
  constructor(errors: unknown[]) {
    super(errors, "queue worker loop failed");
    this.name = "WorkerLoopStopError";
  }
}

export class WorkerHostStopError extends AggregateError {
  constructor(errors: unknown[]) {
    super(errors, "worker host stop failed");
    this.name = "WorkerHostStopError";
  }
}

export class JobsInfrastructureMissingError extends Error {
  constructor(stream: string, queueType: string) {
    super(
      `Jobs work stream '${stream}' was not found while starting queue '${queueType}'. ` +
        "The built-in Trellis jobs infrastructure is missing or not provisioned for this environment. " +
        `Start Trellis and bootstrap the service again so '${stream}' exists before workers start.`,
    );
    this.name = "JobsInfrastructureMissingError";
  }
}

/** A provisioned job queue's durable consumer is absent from its work stream. */
export class JobsConsumerMissingError extends Error {
  constructor(stream: string, consumer: string, queueType: string) {
    super(
      `Jobs consumer '${consumer}' was not found in stream '${stream}' while starting queue '${queueType}'. ` +
        "The queue's provisioned JetStream resource is missing; restore the matching NATS store or reprovision the deployment.",
    );
    this.name = "JobsConsumerMissingError";
  }
}

type WorkMessageLike = {
  data: Uint8Array;
  subject: string;
  info?: { redeliveryCount?: number };
  ack(): void | Promise<void>;
  nak(delay?: number): void | Promise<void>;
  inProgress(): void | Promise<void>;
};

type WorkerConsumerLike = {
  next(): Promise<WorkMessageLike | null>;
};

type CancelMessageLike = {
  subject: string;
  data: Uint8Array;
};

type CancelSubscriptionLike = AsyncIterable<CancelMessageLike> & {
  unsubscribe(): void;
};

type ConsumerInfoLike = unknown;

type StartNatsConsumerDeps = {
  nats: Pick<NatsConnection, "subscribe" | "flush">;
  jsm: {
    consumers: {
      info(stream: string, consumer: string): Promise<ConsumerInfoLike>;
    };
    direct?: DirectMessageReader;
  };
  js: {
    consumers: {
      getConsumerFromInfo(info: ConsumerInfoLike): WorkerConsumerLike;
    };
  };
};

type DirectMessageReader = {
  getMessage(
    stream: string,
    query: { last_by_subj: string },
  ): Promise<{ data: Uint8Array; seq: number } | null>;
};

type StartNatsConnectionDeps = {
  nats: NatsConnection;
  jsm?: undefined;
  js?: undefined;
};

type StartNatsRuntimeDeps = StartNatsConsumerDeps | StartNatsConnectionDeps;

function isCustomNatsRuntimeDeps(
  args: StartNatsRuntimeDeps,
): args is StartNatsConsumerDeps {
  return args.jsm !== undefined && args.js !== undefined;
}

export type StartNatsWorkerHostOptions<TResult> =
  & StartNatsRuntimeDeps
  & {
    instanceId: string;
    queueTypes?: string[];
    queueConcurrency?: Record<string, number>;
    heartbeatPublisher?: {
      publish(subject: string, payload: Uint8Array): void | Promise<void>;
    };
    heartbeatIntervalMs?: number;
    version?: string;
    nowIso?: () => string;
    manager: JobManager<unknown, TResult>;
    getProjectedJob?: (
      job: Job<unknown, TResult>,
    ) => Promise<Job<unknown, TResult> | undefined>;
    getLatestLifecycleEvent?: (
      job: Job<unknown, TResult>,
    ) => Promise<JobEvent | undefined>;
    validatePayload?: (
      args: PayloadValidationArgs<TResult>,
    ) => Promise<void> | void;
    validateResult?: (
      args: ResultValidationArgs<TResult>,
    ) => Promise<void> | void;
    handler: (
      job: ActiveJob<unknown, TResult>,
      session: JobReceivingSession<TResult>,
    ) => Promise<TResult>;
  };

/** @internal Immutable transport inputs retained until a bounded receive and its disposition settle. */
export type JobReceivingSession<TResult> = {
  readonly nc?: NatsConnection;
  readonly manager: JobManager<unknown, TResult>;
  readonly consumer: WorkerConsumerLike;
  readonly physicalLoss?: AbortSignal;
  /** Releases the receiving source's ownership; implementations must be idempotent. */
  release(): void | Promise<void>;
  readonly getProjectedJob?: (
    job: Job<unknown, TResult>,
  ) => Promise<Job<unknown, TResult> | undefined>;
  readonly getLatestLifecycleEvent?: (
    job: Job<unknown, TResult>,
  ) => Promise<JobEvent | undefined>;
};

type StartQueueWorkerLoopOptions<TResult> = {
  /** Shutdown aborts acquisition only; factories reject with the signal reason or AbortError. */
  acquireSession(signal: AbortSignal): Promise<JobReceivingSession<TResult>>;
  cancellationRegistry: ActiveJobCancellationRegistry;
  hostCancellation?: JobCancellationToken;
  payloadSchema?: SchemaRef;
  validatePayload?: (
    args: PayloadValidationArgs<TResult>,
  ) => Promise<void> | void;
  resultSchema?: SchemaRef;
  validateResult?: (
    args: ResultValidationArgs<TResult>,
  ) => Promise<void> | void;
  handler: (
    job: ActiveJob<unknown, TResult>,
    session: JobReceivingSession<TResult>,
  ) => Promise<TResult>;
  instanceId?: string;
  deferralBackoffMs?: number;
  backoffMs?: number[];
  progressAckIntervalMs?: number;
};

/** @internal Converts a native consumer to one broker-bounded receiving slot input. */
export function toWorkerConsumer(
  consumer: {
    next(options: { expires: number }): Promise<JsMsg | null>;
  },
): WorkerConsumerLike {
  return {
    async next(): Promise<WorkMessageLike | null> {
      const msg = await consumer.next({ expires: 1_000 });
      if (!msg) return null;
      return {
        data: msg.data,
        subject: msg.subject,
        info: {
          redeliveryCount: Math.max(0, msg.info.deliveryCount - 1),
        },
        ack: msg.ack.bind(msg),
        nak: msg.nak.bind(msg),
        inProgress: msg.working.bind(msg),
      };
    },
  };
}

export function projectedWorkDecision(
  projected: Job | undefined,
  _work: Job,
): ProjectedWorkDecision {
  if (!projected) {
    return "process";
  }
  return isTerminal(projected.state) ? "skip-ack" : "process";
}

export function lifecycleWorkDecision(
  latest: JobEvent | undefined,
): ProjectedWorkDecision {
  if (!latest) {
    return "process";
  }
  // Only terminal lifecycle settlement, not an attempt diagnostic, finishes work.
  return latest.eventType === latest.state && isTerminal(latest.state)
    ? "skip-ack"
    : "process";
}

export function ackActionForOutcome(
  outcome: JobProcessOutcome<unknown> | undefined,
): WorkerAckAction {
  if (!outcome) {
    return "ack";
  }
  switch (outcome.outcome) {
    case "retry":
    case "interrupted":
    case "stale_completion_ignored":
    case "deferred":
      return "nak";
    default:
      return "ack";
  }
}

async function cleanupTerminalKeyState(
  manager: JobManager<unknown, unknown>,
  job: Job,
): Promise<void> {
  try {
    await manager.cleanupQueuedKeyedJob(job);
  } catch (error) {
    recordTrellisError(error, {
      surface: "job",
      direction: "worker",
      phase: "terminal_key_cleanup",
      messagingSystem: "nats",
    });
    throw error;
  }
}

/** @internal Owns cancellation observation independently of receiving slots or source retirement. */
export async function startJobCancellationCoverage(
  nats: Pick<NatsConnection, "subscribe" | "flush">,
  subject: string,
  registry: ActiveJobCancellationRegistry,
  signal?: AbortSignal,
): Promise<{ stop(): Promise<void> }> {
  signal?.throwIfAborted();
  const subscription = nats.subscribe(
    subject,
  ) as Subscription as CancelSubscriptionLike;
  let failure: unknown;
  const task = (async () => {
    for await (const msg of subscription) {
      const event = parseWorkPayloadEvent(msg.data);
      if (event?.eventType === "cancelled") {
        registry.cancel(`${event.service}.${event.jobType}.${event.jobId}`);
      }
    }
  })().catch((error) => {
    failure = error;
  });
  const aborted = Promise.withResolvers<never>();
  const abort = () => aborted.reject(signal?.reason);
  signal?.addEventListener("abort", abort, { once: true });
  try {
    await Promise.race([nats.flush(), aborted.promise]);
    signal?.throwIfAborted();
  } catch (error) {
    subscription.unsubscribe();
    await task;
    throw error;
  } finally {
    signal?.removeEventListener("abort", abort);
  }
  return {
    async stop() {
      subscription.unsubscribe();
      await task;
      if (failure !== undefined) throw failure;
    },
  };
}

/** Starts one sequential slot; stop accounts its outstanding receive before releasing its source. */
export function startQueueWorkerLoop<TResult>(
  options: StartQueueWorkerLoopOptions<TResult>,
): Promise<{ stop(): Promise<void> }> {
  const registry = options.cancellationRegistry;
  const acquisition = new AbortController();
  const activeTokens = new Set<JobCancellationToken>();
  let stopping = false;
  const hostCancellation = options.hostCancellation;
  const cancelActiveForShutdown = () => {
    for (const token of activeTokens) {
      token.cancelForShutdown();
    }
  };
  const hostAbortHandler = () => {
    stopping = true;
    acquisition.abort();
    cancelActiveForShutdown();
  };
  hostCancellation?.signal.addEventListener("abort", hostAbortHandler);
  if (hostCancellation?.signal.aborted) {
    hostAbortHandler();
  }

  const workTask = (async () => {
    while (!stopping) {
      let session: JobReceivingSession<TResult>;
      try {
        session = await options.acquireSession(acquisition.signal);
      } catch (error) {
        if (
          stopping && (error === acquisition.signal.reason ||
            (error instanceof DOMException && error.name === "AbortError"))
        ) break;
        throw error;
      }
      let retryReceive = false;
      const token = new JobCancellationToken();
      const physicalLossHandler = () => token.cancelForLeaseLoss();
      let guard: ReturnType<typeof registry.register> | undefined;
      let stopProgressAcks: (() => void) | undefined;
      try {
        if (stopping) continue;
        // A slot remains reserved until its broker pull expires or its delivery
        // is accounted for. Never cancel the outstanding receive on shutdown.
        const msg = await session.consumer.next();
        if (!msg) continue;
        session.physicalLoss?.addEventListener("abort", physicalLossHandler);
        if (session.physicalLoss?.aborted) physicalLossHandler();
        if (stopping) token.cancelForShutdown();
        activeTokens.add(token);
        const heartbeat = async () => {
          await msg.inProgress();
        };
        stopProgressAcks = startAutoHeartbeat(
          heartbeat,
          options.progressAckIntervalMs ?? 1_000,
          physicalLossHandler,
        );
        const disposition = async (action: "ack" | "nak", delay?: number) => {
          let outcome = "error";
          try {
            if (action === "ack") await msg.ack();
            else await msg.nak(delay);
            outcome = "ok";
          } finally {
            recordCatalogCounter("trellis.delivery.dispositions", 1, {
              "trellis.family": "job",
              "trellis.action": action,
              "trellis.outcome": outcome,
            });
          }
        };
        try {
          if (stopping) {
            await disposition("nak");
            continue;
          }
          const event = parseWorkPayloadEvent(msg.data);
          if (!event) {
            await disposition("ack");
            continue;
          }
          const job = jobFromWorkEvent(event) as
            | Job<unknown, TResult>
            | undefined;
          if (!job) {
            await disposition("ack");
            continue;
          }
          const key = `${job.service}.${job.type}.${job.id}`;
          guard = registry.register(key, token);
          if (stopping || token.isLeaseLost()) {
            await disposition("nak");
            continue;
          }
          const latestLifecycle = session.getLatestLifecycleEvent
            ? await session.getLatestLifecycleEvent(job)
            : undefined;
          if (stopping || token.isLeaseLost()) {
            await disposition("nak");
            continue;
          }
          if (lifecycleWorkDecision(latestLifecycle) === "skip-ack") {
            await cleanupTerminalKeyState(session.manager, job);
            registry.clearPending(key);
            await disposition("ack");
            continue;
          }
          if (!latestLifecycle) {
            const projected = session.getProjectedJob
              ? await session.getProjectedJob(job)
              : undefined;
            if (stopping || token.isLeaseLost()) {
              await disposition("nak");
              continue;
            }
            if (projectedWorkDecision(projected, job) === "skip-ack") {
              await cleanupTerminalKeyState(session.manager, job);
              registry.clearPending(key);
              await disposition("ack");
              continue;
            }
          }

          const currentJob = latestLifecycle
            ? {
              ...job,
              state: latestLifecycle.state,
              tries: latestLifecycle.tries,
            }
            : job;
          let outcome;
          do {
            outcome = await session.manager.processWithHeartbeat(
              currentJob,
              token,
              heartbeat,
              async (activeJob) => {
                try {
                  await options.validatePayload?.({
                    schema: options.payloadSchema,
                    job: activeJob.job(),
                  });
                } catch (error) {
                  throw JobProcessError.failed(
                    error instanceof Error ? error.message : String(error),
                  );
                }
                return await options.handler(activeJob, session);
              },
              {
                latestState: latestLifecycle?.state,
                workEventType: event.eventType,
                redeliveryCount: msg.info?.redeliveryCount,
                instanceId: options.instanceId,
                progressAckIntervalMs: options.progressAckIntervalMs,
                progressAckManaged: true,
              },
              {
                validateResult: options.validateResult
                  ? (result, resultJob) =>
                    options.validateResult!({
                      schema: options.resultSchema,
                      result,
                      job: resultJob,
                    })
                  : undefined,
              },
            );
            if (outcome.outcome === "deferred") {
              await new Promise((resolve) =>
                setTimeout(resolve, options.progressAckIntervalMs ?? 1_000)
              );
            }
          } while (outcome.outcome === "deferred" && !token.isCancelled());
          const ackAction = ackActionForOutcome(outcome);
          if (ackAction === "ack") {
            if (
              outcome.outcome === "expired" || outcome.outcome === "dead" ||
              outcome.outcome === "stale"
            ) {
              await cleanupTerminalKeyState(session.manager, job);
            }
            await disposition("ack");
          } else if (outcome?.outcome === "deferred") {
            await disposition("nak", options.deferralBackoffMs ?? 1_000);
          } else {
            await disposition(
              "nak",
              retryDelayMs(outcome?.tries ?? 1, options.backoffMs),
            );
          }
        } catch (error) {
          recordTrellisError(error, {
            surface: "job",
            direction: "worker",
            phase: "queue_loop",
            messagingSystem: "nats",
          });
          try {
            await disposition("nak", options.deferralBackoffMs ?? 1_000);
          } catch (nakError) {
            recordTrellisError(nakError, {
              surface: "job",
              direction: "worker",
              phase: "queue_loop_nak",
              messagingSystem: "nats",
            });
          }
        }
      } catch (error) {
        // Managed sessions can reacquire a fresh consumer after the installed
        // library's finite-pull heartbeat timeout or broker no-responders status.
        // Configuration, permission and missing-resource errors remain terminal;
        // fixed hosts keep their existing failure reporting.
        retryReceive = session.physicalLoss !== undefined &&
          !session.physicalLoss.aborted && error instanceof JetStreamError &&
          (error.message === "heartbeats missed" ||
            ("code" in error && error.code === 503));
        if (!session.physicalLoss?.aborted && !retryReceive) throw error;
        recordTrellisError(error, {
          surface: "job",
          direction: "worker",
          phase: "queue_receive",
          messagingSystem: "nats",
        });
      } finally {
        try {
          // The receipt is accounted for. Detach its maintenance and cancellation
          // before release can dispose a drained, replaced receiving source.
          stopProgressAcks?.();
          guard?.dispose();
          activeTokens.delete(token);
          session.physicalLoss?.removeEventListener(
            "abort",
            physicalLossHandler,
          );
        } finally {
          await session.release();
        }
      }
      if (retryReceive && !stopping) {
        // Release the settled receive before backoff. Shutdown wakes this wait
        // immediately and the next loop always reacquires published authority.
        const wake = Promise.withResolvers<void>();
        const abort = () => wake.resolve();
        acquisition.signal.addEventListener("abort", abort, { once: true });
        const timer = setTimeout(abort, 100);
        try {
          if (!acquisition.signal.aborted) await wake.promise;
        } finally {
          clearTimeout(timer);
          acquisition.signal.removeEventListener("abort", abort);
        }
      }
    }
  })();
  let workFailure: unknown;
  const observedWorkTask = workTask.catch((error) => {
    workFailure = error;
  });

  return Promise.resolve({
    async stop(): Promise<void> {
      hostAbortHandler();
      await observedWorkTask;
      hostCancellation?.signal.removeEventListener("abort", hostAbortHandler);
      const failures = [workFailure].filter((error) => error !== undefined);
      if (failures.length > 0) {
        throw new WorkerLoopStopError(failures);
      }
    },
  });
}

export async function startNatsWorkerHostFromBinding<TResult>(
  binding: JobsRuntimeBinding,
  options: StartNatsWorkerHostOptions<TResult>,
): Promise<{ workerCount(): number; stop(): Promise<void> }> {
  const queueTypes = options.queueTypes ??
    Object.keys(binding.jobs.queues).sort();
  const queueConcurrency: Record<string, number> = {};
  for (const queueType of queueTypes) {
    const queue = binding.jobs.queues[queueType];
    if (!queue) {
      throw new Error(
        `Requested worker queue binding '${queueType}' is missing`,
      );
    }
    progressAckIntervalMs(queue);
    const concurrency = options.queueConcurrency?.[queueType] ?? 1;
    if (!Number.isInteger(concurrency) || concurrency < 1) {
      throw new Error(
        `Worker queue '${queueType}' has invalid concurrency ${concurrency}; expected a positive integer`,
      );
    }
    queueConcurrency[queueType] = concurrency;
  }

  const cancellation = new JobCancellationToken();
  const heartbeatLoops = options.heartbeatPublisher
    ? await Promise.all(queueTypes.map((queueType) => {
      const queue = binding.jobs.queues[queueType];
      if (!queue) {
        throw new Error(`Worker queue '${queueType}' is not configured`);
      }
      return startWorkerHeartbeatLoop({
        publisher: options.heartbeatPublisher!,
        service: binding.jobs.serviceName,
        subjectService: binding.jobs.namespace,
        jobType: queueType,
        instanceId: options.instanceId,
        concurrency: queueConcurrency[queueType],
        version: options.version,
        intervalMs: options.heartbeatIntervalMs,
        nowIso: options.nowIso,
      });
    }))
    : [];

  const workers: Array<{ stop(): Promise<void> }> = [];
  const registry = new ActiveJobCancellationRegistry();
  const coverages: Array<{ stop(): Promise<void> }> = [];
  try {
    for (const queueType of queueTypes) {
      const queue = getQueueBinding(binding, queueType);
      const jsm = isCustomNatsRuntimeDeps(options)
        ? options.jsm
        : await jetstreamManager(options.nats);
      const js = isCustomNatsRuntimeDeps(options) ? options.js : {
        consumers: {
          getConsumerFromInfo(info: ConsumerInfoLike) {
            return toWorkerConsumer(
              jetstream(options.nats).consumers.getConsumerFromInfo(
                info as ConsumerInfo,
              ),
            );
          },
        },
      };
      const info = await getConsumerInfo(jsm, binding.workStream, queue);
      coverages.push(
        await startJobCancellationCoverage(
          options.nats,
          `${queue.publishPrefix}.*.cancelled`,
          registry,
        ),
      );
      const direct = jsm.direct;
      for (
        let workerIndex = 0;
        workerIndex < queueConcurrency[queueType]!;
        workerIndex += 1
      ) {
        const consumer = js.consumers.getConsumerFromInfo(info);
        const session: JobReceivingSession<TResult> = {
          nc: isCustomNatsRuntimeDeps(options) ? undefined : options.nats,
          manager: options.manager,
          consumer,
          release() {},
          getProjectedJob: options.getProjectedJob,
          getLatestLifecycleEvent: options.getLatestLifecycleEvent ??
            (direct
              ? (job) =>
                getLatestLifecycleEvent(
                  direct,
                  "JOBS",
                  queue.publishPrefix,
                  job,
                )
              : undefined),
        };

        workers.push(
          await startQueueWorkerLoop({
            acquireSession: () => Promise.resolve(session),
            cancellationRegistry: registry,
            hostCancellation: cancellation,
            payloadSchema: queue.payload,
            validatePayload: options.validatePayload,
            resultSchema: queue.result,
            validateResult: options.validateResult,
            handler: options.handler,
            instanceId: options.instanceId,
            deferralBackoffMs: queue.backoffMs[0] ?? 1_000,
            backoffMs: queue.backoffMs,
            progressAckIntervalMs: progressAckIntervalMs(queue),
          }),
        );
      }
    }
  } catch (error) {
    cancellation.cancelForShutdown();
    const settled = await Promise.allSettled([
      ...workers.map((worker) => worker.stop()),
      ...heartbeatLoops.map((loop) => loop.stop()),
    ]);
    const coverageSettled = await Promise.allSettled(
      coverages.map((coverage) => coverage.stop()),
    );
    const failures = [...settled, ...coverageSettled]
      .filter((result): result is PromiseRejectedResult =>
        result.status === "rejected"
      )
      .map((result) => result.reason);
    if (failures.length) throw new WorkerHostStopError([error, ...failures]);
    throw error;
  }

  return {
    workerCount(): number {
      return workers.length;
    },
    async stop(): Promise<void> {
      cancellation.cancelForShutdown();
      const results = await Promise.allSettled([
        ...workers.map((worker) => worker.stop()),
        ...heartbeatLoops.map((loop) => loop.stop()),
      ]);
      const coverageResults = await Promise.allSettled(
        coverages.map((coverage) => coverage.stop()),
      );
      const failures = [...results, ...coverageResults]
        .filter((result): result is PromiseRejectedResult =>
          result.status === "rejected"
        )
        .map((result) => result.reason);
      if (failures.length > 0) {
        throw new WorkerHostStopError(failures);
      }
    },
  };
}

function retryDelayMs(
  delivery: number,
  configured: number[] | undefined,
): number {
  const schedule = configured?.length
    ? configured
    : [5_000, 30_000, 120_000, 600_000];
  return schedule[Math.min(Math.max(0, delivery - 1), schedule.length - 1)]!;
}

export function progressAckIntervalMs(queue: JobsQueueBinding): number {
  const wait = queue.backoffMs.length > 0
    ? Math.min(...queue.backoffMs)
    : queue.ackWaitMs;
  if (!Number.isInteger(wait) || wait < 1) {
    throw new Error(
      `Worker queue '${queue.queueType}' has invalid acknowledgement wait ${wait}ms; expected a positive whole millisecond`,
    );
  }
  return Math.max(1, Math.floor(wait / 3));
}

/** @internal Reads the provisioned consumer without starting a receive. */
export async function getConsumerInfo<T>(
  jsm: {
    consumers: {
      info(stream: string, consumer: string): Promise<T>;
    };
  },
  stream: string,
  queue: JobsQueueBinding,
): Promise<T> {
  try {
    return await jsm.consumers.info(stream, queue.consumerName);
  } catch (error) {
    if (isStreamNotFoundError(error)) {
      throw new JobsInfrastructureMissingError(stream, queue.queueType);
    }
    if (isConsumerNotFoundError(error)) {
      throw new JobsConsumerMissingError(
        stream,
        queue.consumerName,
        queue.queueType,
      );
    }
    throw error;
  }
}

function isStreamNotFoundError(error: unknown): boolean {
  return error instanceof Error && (
    error.name === "StreamNotFoundError" ||
    error.message.includes("stream not found")
  );
}

function isConsumerNotFoundError(error: unknown): boolean {
  return error instanceof Error && (
    error.name === "ConsumerNotFoundError" ||
    error.message.includes("consumer not found")
  );
}

function getQueueBinding(
  binding: JobsRuntimeBinding,
  queueType: string,
): JobsQueueBinding {
  const queue = binding.jobs.queues[queueType];
  if (!queue) {
    throw new Error(`Requested worker queue binding '${queueType}' is missing`);
  }
  return queue;
}

/**
 * @internal Reads the newest execution-state transition by broker sequence.
 * Explicit retries supersede earlier settlements; observation-only events cannot
 * replace authoritative state or attempt counts.
 */
export async function getLatestLifecycleEvent(
  direct: DirectMessageReader,
  stream: string,
  publishPrefix: string,
  job: Job,
): Promise<JobEvent | undefined> {
  try {
    const transitions = await Promise.all([
      "created",
      "retried",
      "started",
      "retry",
      "completed",
      "failed",
      "cancelled",
      "expired",
      "skipped",
      "stale",
      "dead",
      "dismissed",
    ].map(async (type) => {
      try {
        const msg = await direct.getMessage(stream, {
          last_by_subj: `${publishPrefix}.${job.id}.${type}`,
        });
        if (!msg) return undefined;
        const event = parseWorkPayloadEvent(msg.data);
        return event?.jobId === job.id && event.service === job.service &&
            event.jobType === job.type
          ? { seq: msg.seq, event }
          : undefined;
      } catch (error) {
        if (isMessageNotFoundError(error)) return undefined;
        throw error;
      }
    }));
    let newest: { seq: number; event: JobEvent } | undefined;
    for (const transition of transitions) {
      if (transition && (!newest || transition.seq > newest.seq)) {
        newest = transition;
      }
    }
    return newest?.event;
  } catch (error) {
    if (isMessageNotFoundError(error)) {
      return undefined;
    }
    if (isStreamNotFoundError(error)) {
      throw new JobsInfrastructureMissingError(stream, job.type);
    }
    throw error;
  }
}

function isMessageNotFoundError(error: unknown): boolean {
  return error instanceof Error && (
    error.name === "MessageNotFoundError" ||
    error.message.includes("message not found") ||
    error.message.includes("no message found")
  );
}

function parseWorkPayloadEvent(payload: Uint8Array): JobEvent | undefined {
  try {
    return JSON.parse(new TextDecoder().decode(payload)) as JobEvent;
  } catch {
    return undefined;
  }
}
