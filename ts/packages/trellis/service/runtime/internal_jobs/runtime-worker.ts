import { jetstream, jetstreamManager } from "@nats-io/jetstream";
import type { ConsumerInfo, JsMsg } from "@nats-io/jetstream";
import type { NatsConnection, Subscription } from "@nats-io/nats-core";

import { recordTrellisError } from "../../../telemetry/mod.ts";
import type { JobsQueueBinding, JobsRuntimeBinding } from "./bindings.ts";
import { ActiveJobCancellationRegistry } from "./cancellation-registry.ts";
import { startWorkerHeartbeatLoop } from "./heartbeat.ts";
import type { ActiveJob, JobProcessOutcome } from "./job-manager.ts";
import {
  JobCancellationToken,
  type JobManager,
  JobProcessError,
} from "./job-manager.ts";
import { isTerminal, jobFromWorkEvent } from "./projection.ts";
import type { Job, JobEvent } from "./types.ts";

export type WorkerAckAction = "ack" | "nak" | "await-max-deliver";
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

type WorkMessageLike = {
  data: Uint8Array;
  subject: string;
  info?: { redeliveryCount?: number };
  ack(): void | Promise<void>;
  nak(delay?: number): void | Promise<void>;
  inProgress(): void | Promise<void>;
};

type ConsumerMessagesLike = AsyncIterable<WorkMessageLike> & {
  stop?: () => void;
  close?: () => Promise<void | Error> | void;
};

type WorkerConsumerLike = {
  consume(): Promise<ConsumerMessagesLike>;
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
  nats: Pick<NatsConnection, "subscribe">;
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
  ): Promise<{ data: Uint8Array } | null>;
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

type StartNatsWorkerHostOptions<TResult> =
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
    handler: (job: ActiveJob<unknown, TResult>) => Promise<TResult>;
  };

type StartQueueWorkerLoopOptions<TResult> = {
  manager: JobManager<unknown, TResult>;
  consumer: WorkerConsumerLike;
  cancelSubscription: CancelSubscriptionLike;
  hostCancellation?: JobCancellationToken;
  getProjectedJob?: (
    job: Job<unknown, TResult>,
  ) => Promise<Job<unknown, TResult> | undefined>;
  getLatestLifecycleEvent?: (
    job: Job<unknown, TResult>,
  ) => Promise<JobEvent | undefined>;
  payloadSchema?: SchemaRef;
  validatePayload?: (
    args: PayloadValidationArgs<TResult>,
  ) => Promise<void> | void;
  resultSchema?: SchemaRef;
  validateResult?: (
    args: ResultValidationArgs<TResult>,
  ) => Promise<void> | void;
  handler: (job: ActiveJob<unknown, TResult>) => Promise<TResult>;
  instanceId?: string;
  deferralBackoffMs?: number;
  backoffMs?: number[];
  progressAckIntervalMs?: number;
};

function toWorkerConsumer(
  consumer: {
    consume(): Promise<
      AsyncIterable<JsMsg> & {
        stop?: () => void;
        close?: () => Promise<void | Error> | void;
      }
    >;
  },
): WorkerConsumerLike {
  return {
    async consume(): Promise<ConsumerMessagesLike> {
      const messages = await consumer.consume();
      return {
        stop: messages.stop?.bind(messages),
        close: messages.close?.bind(messages),
        async *[Symbol.asyncIterator]() {
          for await (const msg of messages) {
            yield {
              data: msg.data,
              subject: msg.subject,
              info: {
                redeliveryCount: Math.max(0, msg.info.deliveryCount - 1),
              },
              ack: msg.ack.bind(msg),
              nak: msg.nak.bind(msg),
              inProgress: msg.working.bind(msg),
            };
          }
        },
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
  return isTerminal(latest.state) ? "skip-ack" : "process";
}

export function ackActionForOutcome(
  outcome: JobProcessOutcome<unknown> | undefined,
  maxDeliver = Number.MAX_SAFE_INTEGER,
): WorkerAckAction {
  if (!outcome) {
    return "ack";
  }
  switch (outcome.outcome) {
    case "retry":
      return outcome.tries >= maxDeliver ? "await-max-deliver" : "nak";
    case "interrupted":
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
  }
}

export async function startQueueWorkerLoop<TResult>(
  options: StartQueueWorkerLoopOptions<TResult>,
): Promise<{ stop(): Promise<void> }> {
  const registry = new ActiveJobCancellationRegistry();
  const activeTokens = new Set<JobCancellationToken>();
  const messages = await options.consumer.consume();
  const hostCancellation = options.hostCancellation;
  const stopConsuming = () => {
    if (typeof messages.stop === "function") {
      messages.stop();
    }
    if (typeof messages.close === "function") {
      void messages.close();
    }
  };
  const cancelActiveForShutdown = () => {
    for (const token of activeTokens) {
      token.cancelForShutdown();
    }
  };
  const hostAbortHandler = () => {
    cancelActiveForShutdown();
    stopConsuming();
  };
  hostCancellation?.signal.addEventListener("abort", hostAbortHandler);
  if (hostCancellation?.signal.aborted) {
    hostAbortHandler();
  }

  const workTask = (async () => {
    for await (const msg of messages) {
      try {
        const event = parseWorkPayloadEvent(msg.data);
        if (!event) {
          await msg.ack();
          continue;
        }
        const job = jobFromWorkEvent(event) as
          | Job<unknown, TResult>
          | undefined;
        if (!job) {
          await msg.ack();
          continue;
        }
        const key = `${job.service}.${job.type}.${job.id}`;
        if (hostCancellation?.isHostShutdown()) {
          await msg.nak();
          continue;
        }
        const latestLifecycle = options.getLatestLifecycleEvent
          ? await options.getLatestLifecycleEvent(job)
          : undefined;
        if (hostCancellation?.isHostShutdown()) {
          await msg.nak();
          continue;
        }
        if (lifecycleWorkDecision(latestLifecycle) === "skip-ack") {
          await cleanupTerminalKeyState(options.manager, job);
          registry.clearPending(key);
          await msg.ack();
          continue;
        }
        if (!latestLifecycle) {
          const projected = options.getProjectedJob
            ? await options.getProjectedJob(job)
            : undefined;
          if (hostCancellation?.isHostShutdown()) {
            await msg.nak();
            continue;
          }
          if (projectedWorkDecision(projected, job) === "skip-ack") {
            await cleanupTerminalKeyState(options.manager, job);
            registry.clearPending(key);
            await msg.ack();
            continue;
          }
        }

        const token = new JobCancellationToken();
        if (hostCancellation?.isHostShutdown()) {
          token.cancelForShutdown();
        }
        activeTokens.add(token);
        const guard = registry.register(key, token);
        try {
          const currentJob = latestLifecycle
            ? {
              ...job,
              state: latestLifecycle.state,
              tries: latestLifecycle.tries,
            }
            : job;
          let outcome;
          do {
            outcome = await options.manager.processWithHeartbeat(
              currentJob,
              token,
              async () => {
                await msg.inProgress();
              },
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
                return await options.handler(activeJob);
              },
              {
                latestState: latestLifecycle?.state,
                workEventType: event.eventType,
                redeliveryCount: msg.info?.redeliveryCount,
                instanceId: options.instanceId,
                progressAckIntervalMs: options.progressAckIntervalMs,
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
              await msg.inProgress();
              await new Promise((resolve) =>
                setTimeout(resolve, options.progressAckIntervalMs ?? 1_000)
              );
            }
          } while (outcome.outcome === "deferred" && !token.isCancelled());
          const ackAction = ackActionForOutcome(outcome, job.maxTries);
          if (ackAction === "ack") {
            await msg.ack();
          } else if (ackAction === "await-max-deliver") {
            continue;
          } else if (outcome?.outcome === "deferred") {
            await msg.nak(options.deferralBackoffMs ?? 1_000);
          } else {
            await msg.nak(retryDelayMs(outcome?.tries ?? 1, options.backoffMs));
          }
        } finally {
          guard.dispose();
          activeTokens.delete(token);
        }
      } catch (error) {
        recordTrellisError(error, {
          surface: "job",
          direction: "worker",
          phase: "queue_loop",
          messagingSystem: "nats",
        });
        try {
          await msg.nak(options.deferralBackoffMs ?? 1_000);
        } catch (nakError) {
          recordTrellisError(nakError, {
            surface: "job",
            direction: "worker",
            phase: "queue_loop_nak",
            messagingSystem: "nats",
          });
        }
      }
    }
  })();
  let workFailure: unknown;
  const observedWorkTask = workTask.catch((error) => {
    workFailure = error;
  });

  const cancelTask = (async () => {
    for await (const msg of options.cancelSubscription) {
      const event = parseWorkPayloadEvent(msg.data);
      if (!event || event.eventType !== "cancelled") {
        continue;
      }
      registry.cancel(`${event.service}.${event.jobType}.${event.jobId}`);
    }
  })();
  let cancelFailure: unknown;
  const observedCancelTask = cancelTask.catch((error) => {
    cancelFailure = error;
  });

  return {
    async stop(): Promise<void> {
      options.cancelSubscription.unsubscribe();
      cancelActiveForShutdown();
      stopConsuming();
      await Promise.all([observedWorkTask, observedCancelTask]);
      hostCancellation?.signal.removeEventListener("abort", hostAbortHandler);
      const failures = [workFailure, cancelFailure].filter((error) =>
        error !== undefined
      );
      if (failures.length > 0) {
        throw new WorkerLoopStopError(failures);
      }
    },
  };
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
  for (const queueType of queueTypes) {
    const queue = getQueueBinding(binding, queueType);
    for (
      let workerIndex = 0;
      workerIndex < queueConcurrency[queueType]!;
      workerIndex += 1
    ) {
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
      const consumer = js.consumers.getConsumerFromInfo(info);
      const cancelSubscription = options.nats.subscribe(
        `${queue.publishPrefix}.*.cancelled`,
      ) as Subscription as CancelSubscriptionLike;
      const direct = jsm.direct;

      workers.push(
        await startQueueWorkerLoop({
          manager: options.manager,
          consumer,
          cancelSubscription,
          hostCancellation: cancellation,
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
      const failures = results
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

async function getConsumerInfo(
  jsm: {
    consumers: {
      info(stream: string, consumer: string): Promise<ConsumerInfoLike>;
    };
  },
  stream: string,
  queue: JobsQueueBinding,
): Promise<ConsumerInfoLike> {
  try {
    return await jsm.consumers.info(stream, queue.consumerName);
  } catch (error) {
    if (isStreamNotFoundError(error) || isConsumerNotFoundError(error)) {
      throw new JobsInfrastructureMissingError(stream, queue.queueType);
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

async function getLatestLifecycleEvent(
  direct: DirectMessageReader,
  stream: string,
  publishPrefix: string,
  job: Job,
): Promise<JobEvent | undefined> {
  try {
    const msg = await direct.getMessage(stream, {
      last_by_subj: `${publishPrefix}.${job.id}.*`,
    });
    if (!msg) {
      return undefined;
    }
    return parseWorkPayloadEvent(msg.data);
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
