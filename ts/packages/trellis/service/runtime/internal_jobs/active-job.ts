import type {
  Job,
  JobContext,
  JobLogEntry,
  JobProgress,
  JobWaitTarget,
} from "./types.ts";

export class ActiveJobRuntimeError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ActiveJobRuntimeError";
  }
}

export type ActiveJobRuntimeMetadata = {
  redeliveryCount: number;
};

export class JobCancellationToken {
  readonly #controller = new AbortController();
  #reason: "none" | "job" | "shutdown" | "lease-lost" = "none";

  cancel(): void {
    if (this.#reason === "shutdown") {
      return;
    }
    this.#reason = "job";
    this.#controller.abort("job");
  }

  cancelForShutdown(): void {
    this.#reason = "shutdown";
    this.#controller.abort("shutdown");
  }

  cancelForLeaseLoss(): void {
    if (this.#reason !== "none") return;
    this.#reason = "lease-lost";
    this.#controller.abort("lease-lost");
  }

  get signal(): AbortSignal {
    return this.#controller.signal;
  }

  isCancelled(): boolean {
    return this.#reason !== "none";
  }

  isJobCancelled(): boolean {
    return this.#reason === "job";
  }

  isHostShutdown(): boolean {
    return this.#reason === "shutdown";
  }

  isLeaseLost(): boolean {
    return this.#reason === "lease-lost";
  }
}

type ActiveJobHooks = {
  updateProgress: (progress: JobProgress) => Promise<void>;
  log: (entry: JobLogEntry) => Promise<void>;
  heartbeat: () => Promise<void>;
  waitFor: <T>(target: JobWaitTarget, fn: () => Promise<T>) => Promise<T>;
};

export class ActiveJob<TPayload = unknown, TResult = unknown> {
  readonly #job: Job<TPayload, TResult>;
  readonly #cancellation: JobCancellationToken;
  readonly #hooks: ActiveJobHooks;
  readonly #metadata: ActiveJobRuntimeMetadata;

  constructor(
    job: Job<TPayload, TResult>,
    cancellation: JobCancellationToken,
    hooks: ActiveJobHooks,
    metadata: ActiveJobRuntimeMetadata,
  ) {
    this.#job = job;
    this.#cancellation = cancellation;
    this.#hooks = hooks;
    this.#metadata = metadata;
  }

  job(): Readonly<Job<TPayload, TResult>> {
    return this.#job;
  }

  context(): Readonly<JobContext> {
    return this.#job.context;
  }

  get signal(): AbortSignal {
    return this.#cancellation.signal;
  }

  isCancelled(): boolean {
    return this.#cancellation.isCancelled();
  }

  cancellationToken(): JobCancellationToken {
    return this.#cancellation;
  }

  redeliveryCount(): number {
    return this.#metadata.redeliveryCount;
  }

  isRedelivery(): boolean {
    return this.#metadata.redeliveryCount > 0;
  }

  heartbeat(): Promise<void> {
    return this.#hooks.heartbeat();
  }

  updateProgress(progress: JobProgress): Promise<void> {
    return this.#hooks.updateProgress(progress);
  }

  log(level: JobLogEntry["level"], message: string): Promise<void> {
    return this.#hooks.log({
      timestamp: new Date().toISOString(),
      level,
      message,
    });
  }

  waitFor<T>(target: JobWaitTarget, fn: () => Promise<T> | T): Promise<T> {
    return this.#hooks.waitFor(target, async () => await fn());
  }
}
