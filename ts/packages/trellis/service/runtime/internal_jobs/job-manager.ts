import { headers as natsHeaders, type MsgHdrs } from "@nats-io/nats-core";
import { ulid } from "ulid";

import {
  getActiveJobSnapshot,
  JobNotEnqueuedError,
  type JobSnapshot,
  runWithActiveJobContext,
} from "../../../jobs.ts";
import {
  createMapCarrier,
  injectTraceContext,
} from "../../../telemetry/carrier.ts";
import { recordTrellisError } from "../../../telemetry/mod.ts";
import {
  ActiveJob,
  ActiveJobRuntimeError,
  JobCancellationToken,
} from "./active-job.ts";
import type { JobsBinding, JobsQueueBinding } from "./bindings.ts";
import type {
  ActiveSlotLease,
  JobAdmissionOutcome,
  JobKeyActiveSlot,
  JobKeyCoordinator,
  NormalizedJobKeyPolicy,
  ReplacedQueuedJob,
} from "./key-coordinator.ts";
import { normalizeJobKeyPolicy } from "./key-coordinator.ts";
import type {
  Job,
  JobContext,
  JobEvent,
  JobLineage,
  JobLogEntry,
  JobProgress,
  JobTrigger,
  JobWaitEdge,
  JobWaitTarget,
  PreparedJobSubmission,
} from "./types.ts";

type Publisher = {
  publish(
    subject: string,
    payload: Uint8Array,
    opts?: { headers?: MsgHdrs },
  ): unknown | Promise<unknown>;
};

type JobMetaSource = {
  nextJobId(): string;
  nowIso(): string;
};

type JobManagerContext = {
  nc: Publisher;
  jobs?: JobsBinding;
  keyCoordinator?: JobKeyCoordinator;
  meta?: JobMetaSource;
};

type ActiveJobRuntimeMetadata = {
  redeliveryCount?: number;
  instanceId?: string;
  progressAckIntervalMs?: number;
  latestState?: Job["state"];
  workEventType?: JobEvent["eventType"];
};

type JobProcessValidation<TPayload, TResult> = {
  validateResult?: (
    result: TResult,
    job: Job<TPayload, TResult>,
  ) => Promise<void> | void;
};

export class JobProcessError extends Error {
  readonly kind: "retryable" | "failed";

  constructor(kind: "retryable" | "failed", message: string) {
    super(message);
    this.name = "JobProcessError";
    this.kind = kind;
  }

  static retryable(message: string): JobProcessError {
    return new JobProcessError("retryable", message);
  }

  static failed(message: string): JobProcessError {
    return new JobProcessError("failed", message);
  }
}

export type JobProcessOutcome<TResult> =
  | { outcome: "completed"; tries: number; result: TResult }
  | { outcome: "retry"; tries: number; error: string }
  | { outcome: "dead"; tries: number; error: string }
  | { outcome: "failed"; tries: number; error: string }
  | { outcome: "cancelled"; tries: number }
  | { outcome: "deferred"; tries: number; reason: string }
  | { outcome: "stale_completion_ignored"; tries: number }
  | { outcome: "interrupted"; tries: number };

export type JobManagerSubmitOutcome<TPayload, TResult> =
  | { kind: "accepted"; job: Job<TPayload, TResult>; key?: string }
  | {
    kind: "rejected";
    key: string;
    reason: "active-limit" | "queue-depth" | "stale-blocked";
    active: number;
    queued: number;
    limit: number;
  }
  | {
    kind: "coalesced";
    key: string;
    existing: { service: string; jobType: string; id: string };
    reason: string;
  }
  | {
    kind: "replaced";
    key: string;
    replaced: { service: string; jobType: string; id: string };
    job: Job<TPayload, TResult>;
  };

export class JobManager<TPayload = unknown, TResult = unknown> {
  readonly #context: JobManagerContext;

  constructor(context: JobManagerContext) {
    this.#context = context;
  }

  #meta(): Required<JobMetaSource> {
    return {
      nextJobId: this.#context.meta?.nextJobId ?? (() => ulid()),
      nowIso: this.#context.meta?.nowIso ?? (() => new Date().toISOString()),
    };
  }

  #getQueueBinding(type: string): JobsQueueBinding {
    const binding = this.#context.jobs?.queues[type];
    if (!binding || !this.#context.jobs) {
      throw new Error(`Missing jobs binding for queue '${type}'`);
    }
    return binding;
  }

  async #publishJobEvent(
    type: string,
    jobId: string,
    event: JobEvent<TPayload, TResult>,
    stableMessageId?: string,
  ): Promise<void> {
    const binding = this.#getQueueBinding(type);
    const headers = headersFromJobContext(event.context, stableMessageId);
    try {
      await this.#context.nc.publish(
        `${binding.publishPrefix}.${jobId}.${event.eventType}`,
        new TextEncoder().encode(JSON.stringify(event)),
        { headers },
      );
    } catch (error) {
      recordTrellisError(error, {
        surface: "job",
        direction: "worker",
        operation: type,
        phase: "publish",
        messagingSystem: "nats",
      });
      throw error;
    }
  }

  async create(
    type: string,
    payload: TPayload,
  ): Promise<Job<TPayload, TResult>> {
    const outcome = await this.#submit(type, payload, true);
    if (outcome.kind === "accepted") {
      return outcome.job;
    }
    if (outcome.kind === "coalesced") {
      throw new JobNotEnqueuedError({
        reason: "coalesced",
        key: outcome.key,
        active: 0,
        queued: 1,
        limit: 0,
        existingJobId: outcome.existing.id,
      });
    }
    if (outcome.kind === "replaced") {
      throw new JobNotEnqueuedError({
        reason: "queue-depth",
        key: outcome.key,
        active: 0,
        queued: 1,
        limit: 0,
        existingJobId: outcome.replaced.id,
      });
    }
    throw new JobNotEnqueuedError({
      reason: outcome.reason,
      key: outcome.key,
      active: outcome.active,
      queued: outcome.queued,
      limit: outcome.limit,
    });
  }

  async submit(
    type: string,
    payload: TPayload,
  ): Promise<JobManagerSubmitOutcome<TPayload, TResult>> {
    return await this.#submit(type, payload, false);
  }

  async createPrepared(
    submission: PreparedJobSubmission<TPayload>,
  ): Promise<Job<TPayload, TResult>> {
    const outcome = await this.#submitPrepared(submission, true, true);
    if (outcome.kind === "accepted") {
      return outcome.job;
    }
    await this.#publishSkipped(
      submission.queue,
      { id: submission.jobId, context: submission.context },
      submission.createdAt,
      `trellis-job-skipped:${submission.submissionId}:${submission.jobId}`,
      `job submission not enqueued: ${
        outcome.kind === "coalesced"
          ? "coalesced"
          : outcome.kind === "replaced"
          ? "queue-depth"
          : outcome.reason
      }`,
    );
    if (outcome.kind === "coalesced") {
      throw new JobNotEnqueuedError({
        reason: "coalesced",
        key: outcome.key,
        active: 0,
        queued: 1,
        limit: 0,
        existingJobId: outcome.existing.id,
      });
    }
    if (outcome.kind === "replaced") {
      throw new JobNotEnqueuedError({
        reason: "queue-depth",
        key: outcome.key,
        active: 0,
        queued: 1,
        limit: 0,
        existingJobId: outcome.replaced.id,
      });
    }
    throw new JobNotEnqueuedError({
      reason: outcome.reason,
      key: outcome.key,
      active: outcome.active,
      queued: outcome.queued,
      limit: outcome.limit,
    });
  }

  async submitPrepared(
    submission: PreparedJobSubmission<TPayload>,
  ): Promise<JobManagerSubmitOutcome<TPayload, TResult>> {
    const outcome = await this.#submitPrepared(submission, false, true);
    if (outcome.kind === "rejected" || outcome.kind === "coalesced") {
      await this.#publishSkipped(
        submission.queue,
        { id: submission.jobId, context: submission.context },
        submission.createdAt,
        `trellis-job-skipped:${submission.submissionId}:${submission.jobId}`,
        `job submission not enqueued: ${
          outcome.kind === "coalesced" ? "coalesced" : outcome.reason
        }`,
      );
    }
    return outcome;
  }

  async #submit(
    type: string,
    payload: TPayload,
    strictCreate: boolean,
  ): Promise<JobManagerSubmitOutcome<TPayload, TResult>> {
    const meta = this.#meta();
    const now = meta.nowIso();
    const id = meta.nextJobId();
    const parent = getActiveJobSnapshot();
    const context = createJobContext();
    const serviceName = this.#context.jobs!.serviceName;

    const submission = prepareJobSubmission({
      submissionId: id,
      mode: strictCreate ? "create" : "submit",
      service: serviceName,
      queue: type,
      jobId: id,
      payload,
      createdAt: now,
      context,
      parent,
    });

    return await this.#submitPrepared(submission, strictCreate);
  }

  async #submitPrepared(
    submission: PreparedJobSubmission<TPayload>,
    strictCreate: boolean,
    stableMessageIds?: boolean,
  ): Promise<JobManagerSubmitOutcome<TPayload, TResult>> {
    const {
      queue: type,
      jobId: id,
      createdAt: now,
      context,
      service,
      payload,
      trigger = { kind: "serviceCode" },
      lineage,
    } = submission;
    const binding = this.#getQueueBinding(type);
    const deadline = computeDeadline(now, binding.defaultDeadlineMs);
    const job: Job<TPayload, TResult> = {
      id,
      service,
      type,
      state: "pending",
      context,
      payload,
      createdAt: now,
      updatedAt: now,
      tries: 0,
      maxTries: binding.maxDeliver,
      ...(deadline ? { deadline } : {}),
      trigger,
      ...(lineage ? { lineage } : {}),
    };
    const event: JobEvent<TPayload, TResult> = {
      jobId: id,
      service,
      jobType: type,
      eventType: "created",
      state: "pending",
      context,
      tries: 0,
      maxTries: binding.maxDeliver,
      payload,
      ...(deadline ? { deadline } : {}),
      trigger,
      ...(lineage ? { lineage } : {}),
      timestamp: now,
    };

    const keyedPolicy = getKeyPolicy(binding);
    if (keyedPolicy) {
      const stableSubmissionId = stableMessageIds
        ? submission.submissionId
        : undefined;
      const admission = await this.#admitKeyedCreate({
        type,
        job,
        event,
        policy: keyedPolicy,
        strictCreate,
        ...(stableSubmissionId ? { submissionId: stableSubmissionId } : {}),
      });
      if (admission.kind !== "accepted" && admission.kind !== "replaced") {
        return admission;
      }
      if (strictCreate && admission.kind === "replaced") {
        await this.#restoreReplacedKeyedReservation(
          job,
          admission.replaced,
          keyedPolicy,
          stableSubmissionId,
        );
        return {
          kind: "rejected",
          key: admission.key,
          reason: "queue-depth",
          active: admission.state.active.length,
          queued: admission.state.queued.length,
          limit: keyedPolicy.queue.maxQueuedPerKey,
        };
      }
      if (admission.kind === "replaced") {
        try {
          const skippedMsgId = stableSubmissionId
            ? `trellis-job-skipped:${stableSubmissionId}:${admission.replaced.id}`
            : undefined;
          await this.#publishSkipped(
            type,
            admission.replaced,
            now,
            skippedMsgId,
          );
        } catch (error) {
          await this.#restoreReplacedKeyedReservation(
            job,
            admission.replaced,
            keyedPolicy,
            stableSubmissionId,
          );
          throw error;
        }
      }
      try {
        const createdMsgId = stableSubmissionId
          ? `trellis-job-created:${stableSubmissionId}`
          : undefined;
        await this.#publishJobEvent(type, id, event, createdMsgId);
      } catch (error) {
        await this.#removeQueuedKeyedReservation(
          job,
          keyedPolicy,
          stableSubmissionId,
        );
        throw error;
      }
      if (admission.kind === "replaced") {
        return {
          kind: "replaced",
          key: admission.key,
          replaced: {
            service: admission.replaced.service,
            jobType: admission.replaced.jobType,
            id: admission.replaced.id,
          },
          job,
        };
      }
      return { kind: "accepted", job, key: admission.key };
    }

    const createdMsgId = stableMessageIds
      ? `trellis-job-created:${submission.submissionId}`
      : undefined;
    await this.#publishJobEvent(type, id, event, createdMsgId);

    return { kind: "accepted", job };
  }

  async #admitKeyedCreate(args: {
    type: string;
    job: Job<TPayload, TResult>;
    event: JobEvent<TPayload, TResult>;
    policy: NormalizedJobKeyPolicy;
    strictCreate: boolean;
    submissionId?: string;
  }): Promise<JobAdmissionOutcome> {
    const coordinator = this.#context.keyCoordinator;
    if (!coordinator) {
      throw new Error(
        `Keyed jobs queue '${args.type}' requires the JOBS_KEYS coordinator`,
      );
    }
    return await coordinator.admitCreate({
      service: this.#coordinationService(),
      jobType: args.type,
      jobId: args.job.id,
      payload: args.job.payload,
      context: args.event.context,
      createdAt: args.event.timestamp,
      policy: args.policy,
      strictCreate: args.strictCreate,
      ...(args.submissionId ? { submissionId: args.submissionId } : {}),
    });
  }

  async cleanupQueuedKeyedJob(job: Job<TPayload, TResult>): Promise<void> {
    const queue = this.#getQueueBinding(job.type);
    const keyedPolicy = getKeyPolicy(queue);
    if (!keyedPolicy) return;
    try {
      await this.#removeQueuedKeyedReservation(job, keyedPolicy);
    } catch (cause) {
      const coordinator = this.#context.keyCoordinator;
      if (!coordinator?.removeQueuedJobById) throw cause;
      await coordinator.removeQueuedJobById({
        service: this.#coordinationService(),
        jobType: job.type,
        jobId: job.id,
        now: this.#meta().nowIso(),
      });
    }
  }

  async #removeQueuedKeyedReservation(
    job: Job<TPayload, TResult>,
    policy: NormalizedJobKeyPolicy,
    submissionId?: string,
  ): Promise<void> {
    const coordinator = this.#context.keyCoordinator;
    if (!coordinator) return;
    await coordinator.removeQueuedJob({
      service: this.#coordinationService(),
      jobType: job.type,
      jobId: job.id,
      payload: job.payload,
      now: this.#meta().nowIso(),
      policy,
      ...(submissionId ? { submissionId } : {}),
    });
  }

  async #restoreReplacedKeyedReservation(
    job: Job<TPayload, TResult>,
    replaced: ReplacedQueuedJob,
    policy: NormalizedJobKeyPolicy,
    submissionId?: string,
  ): Promise<void> {
    const coordinator = this.#context.keyCoordinator;
    if (!coordinator) return;
    await coordinator.restoreReplacedQueuedJob({
      service: this.#coordinationService(),
      jobType: job.type,
      replacementJobId: job.id,
      replaced,
      payload: job.payload,
      now: this.#meta().nowIso(),
      policy,
      ...(submissionId ? { submissionId } : {}),
    });
  }

  async #publishSkipped(
    type: string,
    replaced: { id: string; context: JobContext },
    timestamp: string,
    stableMessageId?: string,
    error = "replaced by newer keyed job",
  ): Promise<void> {
    await this.#publishJobEvent(type, replaced.id, {
      jobId: replaced.id,
      service: this.#context.jobs!.serviceName,
      jobType: type,
      eventType: "skipped",
      state: "skipped",
      previousState: "pending",
      context: replaced.context,
      tries: 0,
      error,
      timestamp,
    }, stableMessageId);
  }

  async processWithHeartbeat(
    job: Job<TPayload, TResult>,
    cancellation: JobCancellationToken,
    heartbeat: () => Promise<void>,
    handler: (job: ActiveJob<TPayload, TResult>) => Promise<TResult>,
    metadata: ActiveJobRuntimeMetadata = {},
    validation: JobProcessValidation<TPayload, TResult> = {},
  ): Promise<JobProcessOutcome<TResult>> {
    const tries = job.tries + 1;
    const queue = this.#getQueueBinding(job.type);
    const keyedPolicy = getKeyPolicy(queue);
    let lease: ActiveSlotLease | undefined;
    let staleSlots: JobKeyActiveSlot[] = [];
    if (keyedPolicy) {
      const coordinator = this.#context.keyCoordinator;
      if (!coordinator) {
        throw new Error(
          `Keyed jobs queue '${job.type}' requires the JOBS_KEYS coordinator`,
        );
      }
      const acquired = await coordinator.acquireActiveSlot({
        service: this.#coordinationService(),
        jobType: job.type,
        jobId: job.id,
        payload: job.payload,
        context: job.context,
        lifecycleState: metadata.latestState ?? job.state,
        tries,
        instanceId: metadata.instanceId ?? "unknown",
        now: this.#meta().nowIso(),
        policy: keyedPolicy,
        ...(metadata.workEventType
          ? { workEventType: metadata.workEventType }
          : {}),
      });
      if (acquired.kind === "blocked") {
        return { outcome: "deferred", tries, reason: acquired.reason };
      }
      lease = {
        key: acquired.key,
        keyHash: acquired.keyHash,
        slotToken: acquired.slotToken,
        policy: keyedPolicy,
      };
      staleSlots = acquired.stale;
    }

    try {
      for (const stale of staleSlots) {
        await this.#publishJobEvent(job.type, stale.jobId, {
          jobId: stale.jobId,
          service: job.service,
          jobType: job.type,
          eventType: "stale",
          state: "stale",
          previousState: "active",
          context: stale.context,
          tries: stale.tries,
          error: "keyed job lease expired",
          timestamp: this.#meta().nowIso(),
        });
      }
      await this.#publishJobEvent(job.type, job.id, {
        jobId: job.id,
        service: job.service,
        jobType: job.type,
        eventType: "started",
        state: "active",
        previousState: job.state,
        context: job.context,
        tries,
        timestamp: this.#meta().nowIso(),
      });
    } catch (error) {
      await this.#releaseKeyedSlotAfterPublishFailure(job, lease, error);
      throw error;
    }

    try {
      const result = await this.withActiveJobAndHeartbeat(
        { ...job, state: "active", tries },
        cancellation,
        heartbeat,
        handler,
        metadata,
        lease,
      );

      if (cancellation.isHostShutdown() || cancellation.isLeaseLost()) {
        await this.#releaseKeyedSlot(job, lease);
        return { outcome: "interrupted", tries };
      }
      if (cancellation.isCancelled()) {
        await this.#releaseKeyedSlot(job, lease);
        return { outcome: "cancelled", tries };
      }
      try {
        await validation.validateResult?.(result, {
          ...job,
          state: "active",
          tries,
        });
      } catch (error) {
        throw JobProcessError.failed(
          error instanceof Error ? error.message : String(error),
        );
      }

      const release = await this.#releaseKeyedSlot(job, lease);
      if (release === "staleCompletion") {
        await this.#publishStaleCompletionIgnored(job, tries);
        return { outcome: "stale_completion_ignored", tries };
      }
      await this.#publishJobEvent(job.type, job.id, {
        jobId: job.id,
        service: job.service,
        jobType: job.type,
        eventType: "completed",
        state: "completed",
        previousState: "active",
        context: job.context,
        tries,
        result,
        timestamp: this.#meta().nowIso(),
      });
      return { outcome: "completed", tries, result };
    } catch (error) {
      if (cancellation.isHostShutdown() || cancellation.isLeaseLost()) {
        await this.#releaseKeyedSlot(job, lease);
        return { outcome: "interrupted", tries };
      }
      if (cancellation.isCancelled()) {
        await this.#releaseKeyedSlot(job, lease);
        return { outcome: "cancelled", tries };
      }

      if (error instanceof JobProcessError) {
        const detail = error.message;
        if (error.kind === "retryable") {
          recordTrellisError(error, {
            surface: "job",
            direction: "worker",
            operation: job.type,
            phase: "handler_result",
          });
          const release = await this.#releaseKeyedSlot(job, lease);
          if (release === "staleCompletion") {
            await this.#publishStaleCompletionIgnored(job, tries);
            return { outcome: "stale_completion_ignored", tries };
          }
          await this.#publishJobEvent(job.type, job.id, {
            jobId: job.id,
            service: job.service,
            jobType: job.type,
            eventType: "retry",
            state: "retry",
            previousState: "active",
            context: job.context,
            tries,
            error: detail,
            timestamp: this.#meta().nowIso(),
          });
          return { outcome: "retry", tries, error: detail };
        }

        recordTrellisError(error, {
          surface: "job",
          direction: "worker",
          operation: job.type,
          phase: "handler_result",
        });
        const release = await this.#releaseKeyedSlot(job, lease);
        if (release === "staleCompletion") {
          await this.#publishStaleCompletionIgnored(job, tries);
          return { outcome: "stale_completion_ignored", tries };
        }
        await this.#publishJobEvent(job.type, job.id, {
          jobId: job.id,
          service: job.service,
          jobType: job.type,
          eventType: "failed",
          state: "failed",
          previousState: "active",
          context: job.context,
          tries,
          error: detail,
          timestamp: this.#meta().nowIso(),
        });
        return { outcome: "failed", tries, error: detail };
      }

      recordTrellisError(error, {
        surface: "job",
        direction: "worker",
        operation: job.type,
        phase: "runtime",
      });
      throw error;
    }
  }

  async emitProgress(
    job: Job<TPayload, TResult>,
    progress: JobProgress,
  ): Promise<void> {
    this.#getQueueBinding(job.type);
    if (job.state !== "active") {
      throw new Error(
        `Cannot emit progress for job '${job.id}' in state '${job.state}'`,
      );
    }

    await this.#publishJobEvent(job.type, job.id, {
      jobId: job.id,
      service: job.service,
      jobType: job.type,
      eventType: "progress",
      state: "active",
      previousState: "active",
      context: job.context,
      tries: job.tries,
      progress,
      timestamp: this.#meta().nowIso(),
    });
  }

  async emitLog(job: Job<TPayload, TResult>, log: JobLogEntry): Promise<void> {
    this.#getQueueBinding(job.type);
    if (job.state !== "active") {
      throw new Error(
        `Cannot emit log for job '${job.id}' in state '${job.state}'`,
      );
    }

    await this.#publishJobEvent(job.type, job.id, {
      jobId: job.id,
      service: job.service,
      jobType: job.type,
      eventType: "logged",
      state: "active",
      previousState: "active",
      context: job.context,
      tries: job.tries,
      logs: [log],
      timestamp: this.#meta().nowIso(),
    });
  }

  async waitFor<T>(
    job: Job<TPayload, TResult>,
    target: JobWaitTarget,
    fn: () => Promise<T>,
  ): Promise<T> {
    const edge: JobWaitEdge = {
      id: ulid(),
      target,
      startedAt: this.#meta().nowIso(),
      ...(target.label ? { label: target.label } : {}),
    };

    await this.#publishJobEvent(job.type, job.id, {
      jobId: job.id,
      service: job.service,
      jobType: job.type,
      eventType: "waiting",
      state: "active",
      previousState: "active",
      context: job.context,
      tries: job.tries,
      waitEdge: edge,
      timestamp: edge.startedAt,
    });

    try {
      return await fn();
    } finally {
      try {
        await this.#publishJobEvent(job.type, job.id, {
          jobId: job.id,
          service: job.service,
          jobType: job.type,
          eventType: "resumed",
          state: "active",
          previousState: "active",
          context: job.context,
          tries: job.tries,
          waitEdge: edge,
          timestamp: this.#meta().nowIso(),
        });
      } catch (error) {
        recordTrellisError(error, {
          surface: "job",
          direction: "worker",
          operation: job.type,
          phase: "publish",
        });
      }
    }
  }

  #keyedHeartbeat(
    job: Job<TPayload, TResult>,
    lease: ActiveSlotLease,
  ): () => Promise<void> {
    return async () => {
      const renewed = await this.#context.keyCoordinator!.renewHeartbeat({
        service: this.#coordinationService(),
        jobType: job.type,
        jobId: job.id,
        lease,
        now: this.#meta().nowIso(),
      });
      if (renewed.kind === "lost") {
        throw new ActiveJobRuntimeError("keyed job slot lease was lost");
      }
    };
  }

  async #releaseKeyedSlot(
    job: Job<TPayload, TResult>,
    lease: ActiveSlotLease | undefined,
  ): Promise<"released" | "staleCompletion" | undefined> {
    if (!lease) return undefined;
    const released = await this.#context.keyCoordinator!.releaseActiveSlot({
      service: this.#coordinationService(),
      jobType: job.type,
      jobId: job.id,
      lease,
      now: this.#meta().nowIso(),
    });
    return released.kind === "released" ? "released" : "staleCompletion";
  }

  async #releaseKeyedSlotAfterPublishFailure(
    job: Job<TPayload, TResult>,
    lease: ActiveSlotLease | undefined,
    publishError: unknown,
  ): Promise<void> {
    try {
      await this.#releaseKeyedSlot(job, lease);
    } catch (cleanupError) {
      recordTrellisError(cleanupError, {
        surface: "job",
        direction: "worker",
        operation: job.type,
        phase: "keyed_slot_cleanup",
      });
      recordTrellisError(publishError, {
        surface: "job",
        direction: "worker",
        operation: job.type,
        phase: "publish",
      });
    }
  }

  async #publishStaleCompletionIgnored(
    job: Job<TPayload, TResult>,
    tries: number,
  ): Promise<void> {
    await this.#publishJobEvent(job.type, job.id, {
      jobId: job.id,
      service: job.service,
      jobType: job.type,
      eventType: "staleCompletionIgnored",
      state: "stale",
      previousState: "active",
      context: job.context,
      tries,
      error: "keyed job slot lease was lost before completion",
      timestamp: this.#meta().nowIso(),
    });
  }

  async withActiveJobAndHeartbeat<T>(
    job: Job<TPayload, TResult>,
    cancellation: JobCancellationToken,
    heartbeat: () => Promise<void>,
    f: (job: ActiveJob<TPayload, TResult>) => Promise<T>,
    metadata: ActiveJobRuntimeMetadata = {},
    lease?: ActiveSlotLease,
  ): Promise<T> {
    const stopProgressAcks = startAutoHeartbeat(
      heartbeat,
      metadata.progressAckIntervalMs ?? 1_000,
      () => cancellation.cancelForLeaseLoss(),
    );
    const stopKeyHeartbeat = lease
      ? startAutoHeartbeat(
        this.#keyedHeartbeat(job, lease),
        lease.policy.heartbeatIntervalMs,
        () => cancellation.cancelForLeaseLoss(),
      )
      : () => {};
    const activeJob = new ActiveJob(job, cancellation, {
      updateProgress: (progress) => this.emitProgress(job, progress),
      log: (entry) => this.emitLog(job, entry),
      waitFor: (target, fn) => this.waitFor(job, target, fn),
      heartbeat: async () => {
        try {
          await heartbeat();
        } catch (error) {
          if (error instanceof ActiveJobRuntimeError) {
            throw error;
          }
          throw new ActiveJobRuntimeError(
            error instanceof Error ? error.message : String(error),
          );
        }
      },
    }, {
      redeliveryCount: metadata.redeliveryCount ?? 0,
    });

    try {
      return await runWithActiveJobContext({
        job,
        waitFor: (target, fn) => this.waitFor(job, target, fn),
      }, () => f(activeJob));
    } finally {
      stopProgressAcks();
      stopKeyHeartbeat();
    }
  }

  #coordinationService(): string {
    return this.#context.jobs!.namespace;
  }
}

/** Builds a durable job submission that can be dispatched after a transaction commits. */
export function prepareJobSubmission<TPayload>(args: {
  submissionId: string;
  mode: "create" | "submit";
  service: string;
  queue: string;
  jobId: string;
  payload: TPayload;
  createdAt: string;
  context?: JobContext;
  parent?: JobSnapshot<unknown, unknown>;
}): PreparedJobSubmission<TPayload> {
  const parent = args.parent ?? getActiveJobSnapshot();
  return {
    submissionId: args.submissionId,
    mode: args.mode,
    service: args.service,
    queue: args.queue,
    jobId: args.jobId,
    payload: args.payload,
    createdAt: args.createdAt,
    context: args.context ?? createJobContext(),
    trigger: parent ? parentJobTrigger(parent) : { kind: "serviceCode" },
    ...(parent ? { lineage: childJobLineage(parent) } : {}),
  };
}

function parentJobTrigger(
  parent: JobSnapshot<unknown, unknown>,
): JobTrigger {
  return {
    kind: "parentJob",
    parentJobId: parent.id,
    traceId: parent.context.traceId,
    requestId: parent.context.requestId,
    ...(parent.lineage?.operationId
      ? { operationId: parent.lineage.operationId }
      : {}),
  };
}

function childJobLineage(
  parent: JobSnapshot<unknown, unknown>,
): JobLineage {
  return {
    parentJobId: parent.id,
    rootJobId: parent.lineage?.rootJobId ?? parent.id,
    ...(parent.lineage?.operationId
      ? { operationId: parent.lineage.operationId }
      : {}),
  };
}

function getKeyPolicy(
  binding: JobsQueueBinding,
): NormalizedJobKeyPolicy | undefined {
  if (!binding.keyConcurrency) {
    return undefined;
  }
  return normalizeJobKeyPolicy({
    keyConcurrency: binding.keyConcurrency,
    queue: binding.queue,
  });
}

function startAutoHeartbeat(
  heartbeat: () => Promise<void>,
  intervalMs: number,
  onFailure: () => void,
): () => void {
  const timer = setInterval(() => {
    void heartbeat().catch((error) => {
      recordTrellisError(error, {
        surface: "job",
        direction: "worker",
        phase: "heartbeat",
      });
      clearInterval(timer);
      onFailure();
    });
  }, intervalMs);
  return () => clearInterval(timer);
}

function computeDeadline(now: string, deadlineMs?: number): string | undefined {
  if (deadlineMs === undefined) {
    return undefined;
  }

  const timestamp = new Date(now);
  timestamp.setTime(timestamp.getTime() + deadlineMs);
  return timestamp.toISOString();
}

export function createJobContext(): JobContext {
  const parent = getActiveJobSnapshot();
  if (parent) {
    return parent.context;
  }

  const carrier = createMapCarrier();
  injectTraceContext(carrier);
  const inheritedTraceparent = carrier.get("traceparent");
  const traceparent = isValidTraceparent(inheritedTraceparent)
    ? inheritedTraceparent
    : synthesizeTraceparent();
  const tracestate = carrier.get("tracestate");

  return {
    requestId: ulid(),
    traceId: traceparent.slice(3, 35),
    traceparent,
    ...(tracestate ? { tracestate } : {}),
  };
}

function headersFromJobContext(
  context: JobContext,
  stableMessageId?: string,
): MsgHdrs {
  const headers = natsHeaders();
  headers.set("request-id", context.requestId);
  headers.set("traceparent", context.traceparent);
  if (context.tracestate) {
    headers.set("tracestate", context.tracestate);
  }
  if (stableMessageId) {
    headers.set("Nats-Msg-Id", stableMessageId);
  }
  return headers;
}

function isValidTraceparent(value: string | undefined): value is string {
  if (
    value === undefined ||
    !/^[0-9a-f]{2}-[0-9a-f]{32}-[0-9a-f]{16}-[0-9a-f]{2}$/.test(value)
  ) {
    return false;
  }
  return !isAllZeroHex(value.slice(3, 35)) &&
    !isAllZeroHex(value.slice(36, 52));
}

function synthesizeTraceparent(): string {
  return `00-${randomNonZeroHex(16)}-${randomNonZeroHex(8)}-01`;
}

function randomNonZeroHex(byteLength: number): string {
  let value = "";
  do {
    const bytes = new Uint8Array(byteLength);
    crypto.getRandomValues(bytes);
    value = Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0"))
      .join("");
  } while (isAllZeroHex(value));
  return value;
}

function isAllZeroHex(value: string): boolean {
  return /^0+$/.test(value);
}

export type { JobsBinding, JobsQueueBinding };
export { ActiveJob, ActiveJobRuntimeError, JobCancellationToken };
