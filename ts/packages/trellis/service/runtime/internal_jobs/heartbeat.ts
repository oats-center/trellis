import type { WorkerHeartbeat } from "./types.ts";

type HeartbeatPublisher = {
  publish(subject: string, payload: Uint8Array): void | Promise<void>;
};

type WorkerHeartbeatOptions = {
  service: string;
  jobType: string;
  instanceId: string;
  concurrency?: number;
  version?: string;
  timestamp: string;
};

type WorkerHeartbeatLoopOptions = {
  publisher: HeartbeatPublisher;
  service: string;
  subjectService?: string;
  jobType: string;
  instanceId: string;
  concurrency?: number;
  version?: string;
  intervalMs?: number;
  nowIso?: () => string;
};

export class WorkerHeartbeatLoopError extends AggregateError {
  constructor(errors: unknown[]) {
    super(errors, "worker heartbeat loop failed");
    this.name = "WorkerHeartbeatLoopError";
  }
}

export function workerHeartbeatSubject(
  service: string,
  jobType: string,
  instanceId: string,
): string {
  return `trellis.jobs.workers.${service}.${jobType}.${instanceId}.heartbeat`;
}

export function newWorkerHeartbeat(
  options: WorkerHeartbeatOptions,
): WorkerHeartbeat {
  return {
    service: options.service,
    jobType: options.jobType,
    instanceId: options.instanceId,
    ...(options.concurrency !== undefined
      ? { concurrency: options.concurrency }
      : {}),
    ...(options.version !== undefined ? { version: options.version } : {}),
    timestamp: options.timestamp,
  };
}

export async function publishWorkerHeartbeat(
  publisher: HeartbeatPublisher,
  heartbeat: WorkerHeartbeat,
  subjectService = heartbeat.service,
): Promise<void> {
  await publisher.publish(
    workerHeartbeatSubject(
      subjectService,
      heartbeat.jobType,
      heartbeat.instanceId,
    ),
    new TextEncoder().encode(JSON.stringify(heartbeat)),
  );
}

export async function startWorkerHeartbeatLoop(
  options: WorkerHeartbeatLoopOptions,
): Promise<{ stop(): Promise<void> }> {
  const nowIso = options.nowIso ?? (() => new Date().toISOString());
  const intervalMs = options.intervalMs ?? 30000;
  const errors: unknown[] = [];
  const inFlight = new Set<Promise<void>>();

  const publish = async () => {
    await publishWorkerHeartbeat(
      options.publisher,
      newWorkerHeartbeat({
        service: options.service,
        jobType: options.jobType,
        instanceId: options.instanceId,
        ...(options.concurrency !== undefined
          ? { concurrency: options.concurrency }
          : {}),
        ...(options.version !== undefined ? { version: options.version } : {}),
        timestamp: nowIso(),
      }),
      options.subjectService,
    );
  };

  const publishSafely = (): void => {
    const task = publish()
      .catch((error) => {
        errors.push(error);
      })
      .finally(() => {
        inFlight.delete(task);
      });
    inFlight.add(task);
  };

  await publish();
  const timer = setInterval(() => {
    publishSafely();
  }, intervalMs);

  return {
    async stop(): Promise<void> {
      clearInterval(timer);
      await Promise.all(inFlight);
      if (errors.length > 0) {
        throw new WorkerHeartbeatLoopError(errors);
      }
    },
  };
}
