import type { AsyncLocalStorage } from "node:async_hooks";
import { AsyncResult, BaseError, Result } from "@qlever-llc/result";
import { type StaticDecode, Type } from "typebox";

import { UnexpectedError } from "./errors/index.ts";
import { setActiveJobWaitHook } from "./operations.ts";

export const JobLogEntrySchema = Type.Object({
  timestamp: Type.String({ format: "date-time" }),
  level: Type.Union([
    Type.Literal("info"),
    Type.Literal("warn"),
    Type.Literal("error"),
  ]),
  message: Type.String(),
});

export const JobProgressSchema = Type.Object({
  step: Type.Optional(Type.String()),
  message: Type.Optional(Type.String()),
  current: Type.Optional(Type.Integer({ minimum: 0 })),
  total: Type.Optional(Type.Integer({ minimum: 0 })),
});

export type JobProgress = StaticDecode<typeof JobProgressSchema>;
export type JobLogEntry = StaticDecode<typeof JobLogEntrySchema>;

export const JobContextSchema = Type.Object({
  requestId: Type.String({ minLength: 1 }),
  traceId: Type.String({ pattern: "^[0-9a-f]{32}$" }),
  traceparent: Type.String({
    pattern: "^[0-9a-f]{2}-[0-9a-f]{32}-[0-9a-f]{16}-[0-9a-f]{2}$",
  }),
  tracestate: Type.Optional(Type.String({ minLength: 1 })),
});

export type JobContext = StaticDecode<typeof JobContextSchema>;

export const JobTriggerSchema = Type.Object({
  kind: Type.Union([
    Type.Literal("schedule"),
    Type.Literal("operation"),
    Type.Literal("rpc"),
    Type.Literal("event"),
    Type.Literal("manualReplay"),
    Type.Literal("serviceCode"),
    Type.Literal("parentJob"),
  ]),
  id: Type.Optional(Type.String({ minLength: 1 })),
  subject: Type.Optional(Type.String({ minLength: 1 })),
  operationId: Type.Optional(Type.String({ minLength: 1 })),
  parentJobId: Type.Optional(Type.String({ minLength: 1 })),
  traceId: Type.Optional(Type.String({ pattern: "^[0-9a-f]{32}$" })),
  requestId: Type.Optional(Type.String({ minLength: 1 })),
});

export const JobLineageSchema = Type.Object({
  parentJobId: Type.Optional(Type.String({ minLength: 1 })),
  rootJobId: Type.Optional(Type.String({ minLength: 1 })),
  operationId: Type.Optional(Type.String({ minLength: 1 })),
  relatedKeys: Type.Optional(Type.Array(Type.String({ minLength: 1 }))),
});

export const JobWaitTargetSchema = Type.Object({
  kind: Type.Union([
    Type.Literal("job"),
    Type.Literal("operation"),
    Type.Literal("external"),
  ]),
  id: Type.Optional(Type.String({ minLength: 1 })),
  operationId: Type.Optional(Type.String({ minLength: 1 })),
  service: Type.Optional(Type.String({ minLength: 1 })),
  system: Type.Optional(Type.String({ minLength: 1 })),
  type: Type.Optional(Type.String({ minLength: 1 })),
  operation: Type.Optional(Type.String({ minLength: 1 })),
  key: Type.Optional(Type.String({ minLength: 1 })),
  label: Type.Optional(Type.String({ minLength: 1 })),
});

export const JobWaitEdgeSchema = Type.Object({
  id: Type.String({ minLength: 1 }),
  target: JobWaitTargetSchema,
  startedAt: Type.String({ format: "date-time" }),
  label: Type.Optional(Type.String({ minLength: 1 })),
});

export type JobTrigger = StaticDecode<typeof JobTriggerSchema>;
export type JobLineage = StaticDecode<typeof JobLineageSchema>;
export type JobWaitTarget = StaticDecode<typeof JobWaitTargetSchema>;
export type JobWaitEdge = StaticDecode<typeof JobWaitEdgeSchema>;

export type JobState =
  | "pending"
  | "active"
  | "retry"
  | "completed"
  | "failed"
  | "cancelled"
  | "expired"
  | "skipped"
  | "stale"
  | "dead"
  | "dismissed";

export type JobIdentity = {
  service: string;
  jobType: string;
  id: string;
};

export type JobNotEnqueuedReason =
  | "active-limit"
  | "queue-depth"
  | "stale-blocked"
  | "coalesced";

export type JobNotEnqueuedErrorData = {
  id: string;
  type: "JobNotEnqueuedError";
  message: string;
  reason: JobNotEnqueuedReason;
  key: string;
  active: number;
  queued: number;
  limit: number;
  existingJobId?: string;
  context?: Record<string, unknown>;
  traceId?: string;
};

export type RetryJobErrorData = {
  id: string;
  type: "RetryJobError";
  message: string;
  context?: Record<string, unknown>;
  traceId?: string;
};

/** Error returned when keyed job admission does not create a new job. */
export class JobNotEnqueuedError extends BaseError<JobNotEnqueuedErrorData> {
  override readonly name = "JobNotEnqueuedError" as const;
  readonly reason: JobNotEnqueuedReason;
  readonly key: string;
  readonly active: number;
  readonly queued: number;
  readonly limit: number;
  readonly existingJobId?: string;

  constructor(
    options: ErrorOptions & {
      reason: JobNotEnqueuedReason;
      key: string;
      active: number;
      queued: number;
      limit: number;
      existingJobId?: string;
      message?: string;
      context?: Record<string, unknown>;
      id?: string;
      traceId?: string;
    },
  ) {
    const {
      reason,
      key,
      active,
      queued,
      limit,
      existingJobId,
      message,
      ...baseOptions
    } = options;
    super(
      message ?? `Job was not enqueued for key '${key}': ${reason}`,
      baseOptions,
    );
    this.reason = reason;
    this.key = key;
    this.active = active;
    this.queued = queued;
    this.limit = limit;
    this.existingJobId = existingJobId;
  }

  /** Serializes the admission error for transport or logging. */
  override toSerializable(): JobNotEnqueuedErrorData {
    const base = this.baseSerializable();
    return {
      id: base.id,
      type: this.name,
      message: base.message,
      reason: this.reason,
      key: this.key,
      active: this.active,
      queued: this.queued,
      limit: this.limit,
      ...(this.existingJobId !== undefined
        ? { existingJobId: this.existingJobId }
        : {}),
      ...(base.context !== undefined ? { context: base.context } : {}),
      ...(base.traceId !== undefined ? { traceId: base.traceId } : {}),
    };
  }
}

/** Error returned or thrown by a job handler to request JetStream redelivery. */
export class RetryJobError extends BaseError<RetryJobErrorData> {
  override readonly name = "RetryJobError" as const;

  constructor(
    options: ErrorOptions & {
      message?: string;
      context?: Record<string, unknown>;
      id?: string;
      traceId?: string;
    } = {},
  ) {
    super(options.message ?? "Retry job", options);
  }

  /** Serializes the retry signal for worker lifecycle logging. */
  override toSerializable(): RetryJobErrorData {
    return this.baseSerializable() as RetryJobErrorData;
  }
}

export type JobSubmitOutcome<TPayload, TResult, TUpdate = never> =
  | { kind: "accepted"; ref: JobRef<TPayload, TResult, TUpdate>; key?: string }
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
    existing: JobIdentity;
    reason: string;
  }
  | {
    kind: "replaced";
    key: string;
    replaced: JobIdentity;
    ref: JobRef<TPayload, TResult, TUpdate>;
  };

export type JobSnapshot<TPayload, TResult> = {
  id: string;
  service: string;
  type: string;
  state: JobState;
  context: JobContext;
  payload: TPayload;
  result?: TResult;
  createdAt: string;
  updatedAt: string;
  startedAt?: string;
  completedAt?: string;
  tries: number;
  maxTries: number;
  lastError?: string;
  deadline?: string;
  progress?: JobProgress;
  logs?: JobLogEntry[];
  trigger?: JobTrigger;
  lineage?: JobLineage;
  waitingOn?: JobWaitEdge[];
};

export type Job<TPayload = unknown, TResult = unknown> = JobSnapshot<
  TPayload,
  TResult
>;

export type TerminalJob<TPayload, TResult> = JobSnapshot<TPayload, TResult> & {
  state:
    | "completed"
    | "failed"
    | "cancelled"
    | "expired"
    | "skipped"
    | "stale"
    | "dead"
    | "dismissed";
};

export type JobFilter = {
  service?: string;
  jobType?: string;
  state?: JobState | JobState[];
  since?: string;
  limit?: number;
};

export type WorkerInfo = {
  service: string;
  jobType: string;
  instanceId: string;
  concurrency?: number;
  version?: string;
  timestamp: string;
};

export type ServiceInfo = {
  name: string;
  workers: WorkerInfo[];
  healthy: boolean;
};

export type JobTypeMetadata = {
  payload: unknown;
  update: unknown;
  result: unknown;
};

/** A transient update emitted by one job handler attempt. */
export type JobUpdateEnvelope<TUpdate = unknown> = {
  jobId: string;
  attempt: number;
  sequence: number;
  timestamp: string;
  update: TUpdate;
};

/** Options for a queue-scoped job update subscription. */
export type JobUpdatesOptions = {
  signal?: AbortSignal;
};

/** A live stream of typed transient job updates. */
export type JobUpdateSubscription<TUpdate> = AsyncIterable<TUpdate> & {
  /** Stops the subscription immediately. */
  unsubscribe(): void;
};

/** Decodes the structural Core NATS envelope used for transient job updates. */
export function decodeJobUpdateEnvelope(
  data: Uint8Array,
): JobUpdateEnvelope | undefined {
  try {
    const value: unknown = JSON.parse(new TextDecoder().decode(data));
    if (!value || typeof value !== "object") return undefined;
    const jobId = Reflect.get(value, "jobId");
    const attempt = Reflect.get(value, "attempt");
    const sequence = Reflect.get(value, "sequence");
    const timestamp = Reflect.get(value, "timestamp");
    if (
      typeof jobId !== "string" || jobId.length === 0 ||
      !Number.isInteger(attempt) || attempt < 1 ||
      !Number.isInteger(sequence) || sequence < 1 ||
      typeof timestamp !== "string" || Number.isNaN(Date.parse(timestamp)) ||
      !Reflect.has(value, "update")
    ) {
      return undefined;
    }
    return {
      jobId,
      attempt,
      sequence,
      timestamp,
      update: Reflect.get(value, "update"),
    };
  } catch {
    return undefined;
  }
}

function toUnexpectedError(cause: unknown): UnexpectedError {
  return cause instanceof UnexpectedError
    ? cause
    : new UnexpectedError({ cause });
}

type ActiveJobContext = {
  readonly job: JobSnapshot<unknown, unknown>;
  waitFor<T>(target: JobWaitTarget, fn: () => Promise<T>): Promise<T>;
};

let activeJobStorage: AsyncLocalStorage<ActiveJobContext> | undefined;

/** @internal Runs work with the current active job available to child job creation and waits. */
export async function runWithActiveJobContext<T>(
  context: ActiveJobContext,
  fn: () => Promise<T>,
): Promise<T> {
  if (activeJobStorage === undefined) {
    const { AsyncLocalStorage } = await import("node:async_hooks");
    activeJobStorage ??= new AsyncLocalStorage<ActiveJobContext>();
  }
  return activeJobStorage.run(context, fn);
}

/** @internal Returns the current active job, if called inside a job handler. */
export function getActiveJobSnapshot():
  | JobSnapshot<unknown, unknown>
  | undefined {
  return activeJobStorage?.getStore()?.job;
}

/** @internal Records a current active-job wait if called inside a job handler. */
export function runActiveJobWait<T>(
  target: JobWaitTarget,
  fn: () => Promise<T>,
): Promise<T> | undefined {
  return activeJobStorage?.getStore()?.waitFor(target, fn);
}

setActiveJobWaitHook(runActiveJobWait);

export class JobRef<TPayload, TResult, TUpdate = never> {
  readonly id: string;
  readonly service: string;
  readonly type: string;

  readonly #get: () => AsyncResult<JobSnapshot<TPayload, TResult>, BaseError>;
  readonly #wait: () => AsyncResult<TerminalJob<TPayload, TResult>, BaseError>;
  readonly #cancel: () => AsyncResult<
    JobSnapshot<TPayload, TResult>,
    BaseError
  >;
  readonly #updates?: (
    options?: JobUpdatesOptions,
  ) => AsyncResult<JobUpdateSubscription<TUpdate>, BaseError>;

  constructor(
    ref: JobIdentity,
    impl: {
      get: () => AsyncResult<JobSnapshot<TPayload, TResult>, BaseError>;
      wait: () => AsyncResult<TerminalJob<TPayload, TResult>, BaseError>;
      cancel: () => AsyncResult<JobSnapshot<TPayload, TResult>, BaseError>;
      updates?: (
        options?: JobUpdatesOptions,
      ) => AsyncResult<JobUpdateSubscription<TUpdate>, BaseError>;
    },
  ) {
    this.id = ref.id;
    this.service = ref.service;
    this.type = ref.jobType;
    this.#get = impl.get;
    this.#wait = impl.wait;
    this.#cancel = impl.cancel;
    this.#updates = impl.updates;
  }

  get(): AsyncResult<JobSnapshot<TPayload, TResult>, BaseError> {
    try {
      return this.#get();
    } catch (cause) {
      return AsyncResult.err(toUnexpectedError(cause));
    }
  }

  wait(): AsyncResult<TerminalJob<TPayload, TResult>, BaseError> {
    return AsyncResult.from((async () => {
      try {
        const waited = runActiveJobWait({
          kind: "job",
          id: this.id,
          service: this.service,
          type: this.type,
        }, async () => await this.#wait());
        if (waited) return await waited;
        return await this.#wait();
      } catch (cause) {
        return Result.err(toUnexpectedError(cause));
      }
    })());
  }

  cancel(): AsyncResult<JobSnapshot<TPayload, TResult>, BaseError> {
    try {
      return this.#cancel();
    } catch (cause) {
      return AsyncResult.err(toUnexpectedError(cause));
    }
  }

  /** Subscribes to transient updates for this job. */
  updates(
    options?: JobUpdatesOptions,
  ): AsyncResult<JobUpdateSubscription<TUpdate>, BaseError> {
    if (!this.#updates) {
      return AsyncResult.err(toUnexpectedError(
        new Error("Job updates are unavailable for this reference"),
      ));
    }
    try {
      return this.#updates(options);
    } catch (cause) {
      return AsyncResult.err(toUnexpectedError(cause));
    }
  }
}

export class ActiveJob<TPayload, TResult, TUpdate = never> {
  readonly ref: JobRef<TPayload, TResult, TUpdate>;
  readonly payload: TPayload;
  readonly context: Readonly<JobContext>;
  readonly signal: AbortSignal;

  readonly #cancelled: () => boolean;
  readonly #heartbeat: () => AsyncResult<void, BaseError>;
  readonly #progress: (value: JobProgress) => AsyncResult<void, BaseError>;
  readonly #log: (entry: JobLogEntry) => AsyncResult<void, BaseError>;
  readonly #emitUpdate: (value: TUpdate) => AsyncResult<void, BaseError>;
  readonly #waitFor: <T>(
    target: JobWaitTarget,
    fn: () => Promise<T>,
  ) => Promise<T>;
  readonly #redeliveryCount: number;

  constructor(
    ref: JobRef<TPayload, TResult, TUpdate>,
    payload: TPayload,
    context: JobContext,
    cancelled: boolean | (() => boolean),
    impl: {
      heartbeat: () => AsyncResult<void, BaseError>;
      progress: (value: JobProgress) => AsyncResult<void, BaseError>;
      log: (entry: JobLogEntry) => AsyncResult<void, BaseError>;
      emitUpdate?: (value: TUpdate) => AsyncResult<void, BaseError>;
      waitFor: <T>(target: JobWaitTarget, fn: () => Promise<T>) => Promise<T>;
      redeliveryCount?: number;
      signal?: AbortSignal;
    },
  ) {
    this.ref = ref;
    this.payload = payload;
    this.context = Object.freeze({ ...context });
    this.#cancelled = typeof cancelled === "function"
      ? cancelled
      : () => cancelled;
    this.#heartbeat = impl.heartbeat;
    this.#progress = impl.progress;
    this.#log = impl.log;
    this.#emitUpdate = impl.emitUpdate ??
      (() =>
        AsyncResult.err(toUnexpectedError(
          new Error("Job updates are not configured for this queue"),
        )));
    this.#waitFor = impl.waitFor;
    this.#redeliveryCount = impl.redeliveryCount ?? 0;
    this.signal = impl.signal ?? new AbortController().signal;
  }

  get cancelled(): boolean {
    try {
      return this.#cancelled();
    } catch {
      return false;
    }
  }

  heartbeat(): AsyncResult<void, BaseError> {
    try {
      return this.#heartbeat();
    } catch (cause) {
      return AsyncResult.err(toUnexpectedError(cause));
    }
  }

  progress(value: JobProgress): AsyncResult<void, BaseError> {
    try {
      return this.#progress(value);
    } catch (cause) {
      return AsyncResult.err(toUnexpectedError(cause));
    }
  }

  log(entry: JobLogEntry): AsyncResult<void, BaseError> {
    try {
      return this.#log(entry);
    } catch (cause) {
      return AsyncResult.err(toUnexpectedError(cause));
    }
  }

  /** Emits a validated transient update for the current handler attempt. */
  emitUpdate(value: TUpdate): AsyncResult<void, BaseError> {
    try {
      return this.#emitUpdate(value);
    } catch (cause) {
      return AsyncResult.err(toUnexpectedError(cause));
    }
  }

  /**
   * Records that this job is waiting on another job, operation, or external async work.
   * The wrapped function's return value or thrown error is preserved.
   */
  waitFor<T>(target: JobWaitTarget, fn: () => Promise<T> | T): Promise<T> {
    return this.#waitFor(target, async () => await fn());
  }

  redeliveryCount(): number {
    return this.#redeliveryCount;
  }

  isRedelivery(): boolean {
    return this.#redeliveryCount > 0;
  }
}

export class JobQueue<TPayload, TResult, TUpdate = never> {
  readonly #create: (
    payload: TPayload,
  ) => AsyncResult<JobRef<TPayload, TResult, TUpdate>, BaseError>;
  readonly #handle: (
    handler: (
      job: ActiveJob<TPayload, TResult, TUpdate>,
    ) => Promise<Result<TResult, BaseError>>,
    options?: JobHandlerOptions,
  ) => void;
  readonly #submit: (
    payload: TPayload,
  ) => AsyncResult<JobSubmitOutcome<TPayload, TResult, TUpdate>, BaseError>;
  readonly #updates: (
    jobId: string,
    options?: JobUpdatesOptions,
  ) => AsyncResult<JobUpdateSubscription<TUpdate>, BaseError>;

  constructor(impl: {
    create: (
      payload: TPayload,
    ) => AsyncResult<JobRef<TPayload, TResult, TUpdate>, BaseError>;
    handle: (
      handler: (
        job: ActiveJob<TPayload, TResult, TUpdate>,
      ) => Promise<Result<TResult, BaseError>>,
      options?: JobHandlerOptions,
    ) => void;
    submit?: (
      payload: TPayload,
    ) => AsyncResult<JobSubmitOutcome<TPayload, TResult, TUpdate>, BaseError>;
    updates?: (
      jobId: string,
      options?: JobUpdatesOptions,
    ) => AsyncResult<JobUpdateSubscription<TUpdate>, BaseError>;
  }) {
    this.#create = impl.create;
    this.#handle = impl.handle;
    this.#submit = impl.submit ??
      ((payload) =>
        impl.create(payload).map((ref) => ({ kind: "accepted", ref })));
    this.#updates = impl.updates ?? (() =>
      AsyncResult.err(toUnexpectedError(
        new Error("Job updates are not configured for this queue"),
      )));
  }

  create(
    payload: TPayload,
  ): AsyncResult<JobRef<TPayload, TResult, TUpdate>, BaseError> {
    try {
      return this.#create(payload);
    } catch (cause) {
      return AsyncResult.err(toUnexpectedError(cause));
    }
  }

  /** Subscribes to transient updates for a known or preallocated job id. */
  updates(
    jobId: string,
    options?: JobUpdatesOptions,
  ): AsyncResult<JobUpdateSubscription<TUpdate>, BaseError> {
    try {
      return this.#updates(jobId, options);
    } catch (cause) {
      return AsyncResult.err(toUnexpectedError(cause));
    }
  }

  /**
   * Submits a job using queue policy outcomes for keyed queues.
   * Unkeyed queues accept and return a new job reference.
   */
  submit(
    payload: TPayload,
  ): AsyncResult<JobSubmitOutcome<TPayload, TResult, TUpdate>, BaseError> {
    try {
      return this.#submit(payload);
    } catch (cause) {
      return AsyncResult.err(toUnexpectedError(cause));
    }
  }

  handle(
    handler: (
      job: ActiveJob<TPayload, TResult, TUpdate>,
    ) => Promise<Result<TResult, BaseError>>,
    options?: JobHandlerOptions,
  ): void {
    this.#handle(handler, options);
  }
}

/** Per-service-instance job handler configuration. */
export type JobHandlerOptions = {
  /** Worker loops started for this job type. Defaults to one. */
  concurrency?: number;
};

export interface JobWorkerHost {
  stop(): AsyncResult<void, BaseError>;
  join(): AsyncResult<void, BaseError>;
}

export class JobWorkerHostAdapter implements JobWorkerHost {
  readonly #stop: () => AsyncResult<void, BaseError>;
  readonly #join: () => AsyncResult<void, BaseError>;

  constructor(impl: {
    stop: () => AsyncResult<void, BaseError>;
    join: () => AsyncResult<void, BaseError>;
  }) {
    this.#stop = impl.stop;
    this.#join = impl.join;
  }

  stop(): AsyncResult<void, BaseError> {
    try {
      return this.#stop();
    } catch (cause) {
      return AsyncResult.err(toUnexpectedError(cause));
    }
  }

  join(): AsyncResult<void, BaseError> {
    try {
      return this.#join();
    } catch (cause) {
      return AsyncResult.err(toUnexpectedError(cause));
    }
  }
}

export type JobsFacade = {};

export type JobsFacadeOf<TJobs extends Record<string, JobTypeMetadata>> =
  & {
    [K in keyof TJobs]: JobQueue<
      TJobs[K]["payload"],
      TJobs[K]["result"],
      TJobs[K]["update"]
    >;
  }
  & JobsFacade;
