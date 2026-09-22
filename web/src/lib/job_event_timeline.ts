import { boundedNumber, compactDuration } from "./format.ts";

type Numeric = number | bigint;

/** A wait relationship recorded in a job lifecycle event. */
export type JobTimelineWaitEdge = {
  id: string;
  label?: string;
  startedAt: string;
  target: {
    id?: string;
    operationId?: string;
    kind: string;
    key?: string;
    label?: string;
    operation?: string;
    service?: string;
    system?: string;
    type?: string;
  };
};

/** The Console fields used to render one projected job lifecycle event. */
export type JobTimelineEvent = {
  sequence: Numeric;
  timestamp: string;
  state: string;
  previousState?: string;
  type: string;
  message?: string;
  progress?: {
    current?: Numeric;
    message?: string;
    step?: string;
    total?: Numeric;
  };
  reason?: string;
  error?: string;
  logs?: Array<{
    timestamp: string;
    level: string;
    message: string;
  }>;
  waitEdge?: JobTimelineWaitEdge;
  workerInstanceId?: string;
  tries?: Numeric;
};

/** A paired dependency interval within an execution attempt. */
export type JobTimelineWait = {
  kind: "wait";
  label: string;
  detail?: string;
  startedAt: string;
  endTimestamp?: string;
  type: string;
  duration?: string;
  rawEvents: JobTimelineEvent[];
};

/** One service-authored progress update in execution order. */
export type JobTimelineStep = {
  kind: "step";
  label: string;
  detail?: string;
  timestamp: string;
  type: string;
  waits: JobTimelineWait[];
  rawEvents: JobTimelineEvent[];
};

/** A non-progress event retained as execution evidence. */
export type JobTimelineRuntimeEvent = {
  kind: "event";
  label: string;
  detail?: string;
  timestamp: string;
  type: string;
  logs?: JobTimelineEvent["logs"];
  rawEvents: JobTimelineEvent[];
};

/** Time spent waiting for a worker before an execution attempt. */
export type JobTimelineQueuePhase = {
  kind: "queue";
  label: "Queue" | "Retry delay";
  enteredAt: string;
  enteredLabel: string;
  enteredType: string;
  exitedAt?: string;
  exitedLabel?: string;
  exitedType?: string;
  duration?: string;
  transition?: string;
  rawEvents: JobTimelineEvent[];
};

/** Work performed by one execution attempt. */
export type JobTimelineExecutionPhase = {
  kind: "execution";
  label: "Execution";
  startedAt: string;
  startedType: string;
  endedAt?: string;
  duration?: string;
  attempt?: Numeric;
  workerInstanceId?: string;
  transition?: string;
  steps: JobTimelineStep[];
  waits: JobTimelineWait[];
  events: JobTimelineRuntimeEvent[];
  rawEvents: JobTimelineEvent[];
};

/** The state-changing result of queueing or execution. */
export type JobTimelineOutcomePhase = {
  kind: "outcome";
  label: string;
  state: string;
  timestamp: string;
  type: string;
  duration?: string;
  durationKind?: "queue" | "runtime";
  detail?: string;
  transition?: string;
  rawEvents: JobTimelineEvent[];
};

/** One operator-readable section in a job execution story. */
export type JobTimelinePhase =
  | JobTimelineQueuePhase
  | JobTimelineExecutionPhase
  | JobTimelineOutcomePhase;

const QUEUE_ENTRY_TYPES = new Set(["created", "retried", "retry"]);

const OUTCOME_LABELS: Record<string, string> = {
  cancelled: "Cancelled",
  completed: "Completed",
  dead: "Dead letter queue",
  dismissed: "Dismissed",
  expired: "Expired",
  failed: "Failed",
  retry: "Retrying",
  skipped: "Skipped",
  stale: "Stale",
};

const TERMINAL_STATES = new Set([
  "cancelled",
  "completed",
  "dead",
  "dismissed",
  "expired",
  "failed",
  "retry",
  "skipped",
  "stale",
]);

/** Builds phase, work-step, dependency, and outcome sections from raw events. */
export function buildJobTimeline(
  events: JobTimelineEvent[],
): JobTimelinePhase[] {
  const sorted = [...events].sort((a, b) =>
    boundedNumber(a.sequence) - boundedNumber(b.sequence)
  );
  const phases: JobTimelinePhase[] = [];
  const openWaits = new Map<string, JobTimelineWait>();
  let queue: JobTimelineQueuePhase | undefined;
  let execution: JobTimelineExecutionPhase | undefined;
  let currentStep: JobTimelineStep | undefined;
  let pendingRetry: JobTimelineEvent | undefined;

  const startQueue = (event: JobTimelineEvent, retry = false) => {
    queue = {
      kind: "queue",
      label: retry ? "Retry delay" : "Queue",
      enteredAt: event.timestamp,
      enteredLabel: retry
        ? "Retry scheduled"
        : event.type.toLowerCase() === "retried"
        ? "Requeued"
        : "Created",
      enteredType: event.type,
      rawEvents: [event],
    };
    phases.push(queue);
  };

  for (const event of sorted) {
    const type = event.type.toLowerCase();

    if (type === "created" || type === "retried") {
      openWaits.clear();
      pendingRetry = undefined;
      startQueue(event);
      execution = undefined;
      currentStep = undefined;
      continue;
    }

    if (type === "started") {
      openWaits.clear();
      if (!queue && pendingRetry) startQueue(pendingRetry, true);
      pendingRetry = undefined;
      if (queue) {
        closeQueue(queue, event, "Picked up");
        queue = undefined;
      }
      execution = {
        kind: "execution",
        label: "Execution",
        startedAt: event.timestamp,
        startedType: event.type,
        attempt: event.tries,
        workerInstanceId: event.workerInstanceId,
        transition: transitionLabel(event),
        steps: [],
        waits: [],
        events: [],
        rawEvents: [event],
      };
      phases.push(execution);
      currentStep = undefined;
      continue;
    }

    if (type === "progress") {
      execution ??= startSyntheticExecution(phases, event);
      const step = progressStep(event);
      execution.steps.push(step);
      execution.rawEvents.push(event);
      currentStep = step;
      continue;
    }

    if (type === "waiting" && event.waitEdge) {
      execution ??= startSyntheticExecution(phases, event);
      const wait = dependencyWait(event);
      if (currentStep) currentStep.waits.push(wait);
      else execution.waits.push(wait);
      execution.rawEvents.push(event);
      openWaits.set(event.waitEdge.id, wait);
      continue;
    }

    if (type === "resumed" && event.waitEdge) {
      const wait = openWaits.get(event.waitEdge.id);
      if (wait) {
        wait.endTimestamp = event.timestamp;
        wait.type = `${wait.type} → ${event.type}`;
        wait.duration = elapsedLabel(wait.startedAt, event.timestamp);
        wait.rawEvents.push(event);
        execution?.rawEvents.push(event);
        openWaits.delete(event.waitEdge.id);
        continue;
      }
    }

    if (type === "retry" && !execution) {
      pendingRetry = undefined;
      startQueue(event, true);
      currentStep = undefined;
      continue;
    }

    if (isOutcome(event, type)) {
      if (!queue && pendingRetry) startQueue(pendingRetry, true);
      pendingRetry = undefined;
      let duration: string | undefined;
      let durationKind: JobTimelineOutcomePhase["durationKind"];
      if (execution) {
        execution.endedAt = event.timestamp;
        execution.duration = elapsedLabel(execution.startedAt, event.timestamp);
        execution.rawEvents.push(event);
        duration = execution.duration;
        durationKind = "runtime";
        execution = undefined;
        currentStep = undefined;
      } else if (queue) {
        closeQueue(queue, event, outcomeLabel(event, type));
        duration = queue.duration;
        durationKind = "queue";
        queue = undefined;
      }

      phases.push({
        kind: "outcome",
        label: outcomeLabel(event, type),
        state: event.state,
        timestamp: event.timestamp,
        type: event.type,
        duration,
        durationKind,
        detail: event.reason ?? parseErrorMessage(event.error),
        transition: transitionLabel(event),
        rawEvents: [event],
      });

      openWaits.clear();
      if (type === "retry") pendingRetry = event;
      continue;
    }

    execution ??= startSyntheticExecution(phases, event);
    execution.events.push(runtimeEvent(event, type));
    execution.rawEvents.push(event);
  }

  return phases;
}

function startSyntheticExecution(
  phases: JobTimelinePhase[],
  event: JobTimelineEvent,
): JobTimelineExecutionPhase {
  const execution: JobTimelineExecutionPhase = {
    kind: "execution",
    label: "Execution",
    startedAt: event.timestamp,
    startedType: event.type,
    attempt: event.tries,
    workerInstanceId: event.workerInstanceId,
    steps: [],
    waits: [],
    events: [],
    rawEvents: [],
  };
  phases.push(execution);
  return execution;
}

function closeQueue(
  queue: JobTimelineQueuePhase,
  event: JobTimelineEvent,
  label: string,
): void {
  queue.exitedAt = event.timestamp;
  queue.exitedLabel = label;
  queue.exitedType = event.type;
  queue.duration = elapsedLabel(queue.enteredAt, event.timestamp);
  queue.transition = transitionLabel(event);
  queue.rawEvents.push(event);
}

function progressStep(event: JobTimelineEvent): JobTimelineStep {
  const progress = event.progress;
  const count = progress?.current !== undefined && progress.total !== undefined
    ? `${progress.current}/${progress.total}`
    : undefined;
  return {
    kind: "step",
    label: progress?.message ?? event.message ?? progress?.step ??
      event.reason ??
      "Progress update",
    detail: [progress?.step, count].filter(Boolean).join(" · ") || undefined,
    timestamp: event.timestamp,
    type: event.type,
    waits: [],
    rawEvents: [event],
  };
}

function dependencyWait(event: JobTimelineEvent): JobTimelineWait {
  const edge = event.waitEdge!;
  return {
    kind: "wait",
    label: waitTargetLabel(edge),
    detail: waitTargetDetail(edge),
    startedAt: edge.startedAt,
    type: event.type,
    rawEvents: [event],
  };
}

function runtimeEvent(
  event: JobTimelineEvent,
  type: string,
): JobTimelineRuntimeEvent {
  return {
    kind: "event",
    label: event.message ?? event.reason ?? activityLabel(type),
    detail: parseErrorMessage(event.error),
    timestamp: event.timestamp,
    type: event.type,
    logs: event.logs,
    rawEvents: [event],
  };
}

/** Selects one attempt plus the queue boundary that immediately preceded it. */
export function jobTimelineEventsForAttempt(
  events: JobTimelineEvent[],
  attempt: Numeric,
): JobTimelineEvent[] {
  const selected = events.filter((event) => event.tries === attempt);
  const started = selected.find((event) =>
    event.type.toLowerCase() === "started"
  );
  if (!started) return selected;

  const hasQueueEntry = selected.some((event) =>
    event.sequence < started.sequence &&
    QUEUE_ENTRY_TYPES.has(event.type.toLowerCase())
  );
  if (hasQueueEntry) return selected;

  const queueEntry = events
    .filter((event) =>
      event.sequence < started.sequence &&
      QUEUE_ENTRY_TYPES.has(event.type.toLowerCase())
    )
    .sort((a, b) => boundedNumber(b.sequence) - boundedNumber(a.sequence))[0];

  return queueEntry
    ? [...selected, queueEntry].sort((a, b) =>
      boundedNumber(a.sequence) - boundedNumber(b.sequence)
    )
    : selected;
}

function isOutcome(event: JobTimelineEvent, type: string): boolean {
  if (type in OUTCOME_LABELS) return true;
  if (!event.previousState || event.previousState === event.state) return false;
  return TERMINAL_STATES.has(event.state);
}

function outcomeLabel(event: JobTimelineEvent, type: string): string {
  if (type === "error" && event.state === "failed") return "Failed";
  return OUTCOME_LABELS[type] ?? OUTCOME_LABELS[event.state] ?? humanize(type);
}

function transitionLabel(event: JobTimelineEvent): string | undefined {
  return event.previousState && event.previousState !== event.state
    ? `${event.previousState} → ${event.state}`
    : undefined;
}

function activityLabel(type: string): string {
  switch (type) {
    case "heartbeat":
      return "Worker heartbeat";
    case "logged":
      return "Log entry recorded";
    case "resumed":
      return "Resumed";
    case "stalecompletionignored":
      return "Stale completion ignored";
    default:
      return humanize(type);
  }
}

function waitTargetLabel(edge: JobTimelineWaitEdge): string {
  return edge.target.label ?? edge.label ?? edge.target.operation ??
    edge.target.type ?? edge.target.key ?? edge.target.operationId ??
    edge.target.id ?? edge.target.kind;
}

function waitTargetDetail(edge: JobTimelineWaitEdge): string | undefined {
  const identity = edge.target.key ?? edge.target.operationId ?? edge.target.id;
  const parts = [
    edge.target.kind,
    edge.target.system ?? edge.target.service,
    edge.target.operation ?? edge.target.type,
    identity,
  ];
  const unique = parts.filter((part, index) =>
    Boolean(part) && parts.indexOf(part) === index
  );
  return unique.join(" · ") || undefined;
}

function elapsedLabel(start: string, end: string): string | undefined {
  const startMs = new Date(start).getTime();
  const endMs = new Date(end).getTime();
  if (Number.isNaN(startMs) || Number.isNaN(endMs) || endMs < startMs) {
    return undefined;
  }
  return compactDuration(endMs - startMs);
}

function parseErrorMessage(error?: string): string | undefined {
  if (!error) return undefined;
  try {
    const parsed: unknown = JSON.parse(error);
    if (typeof parsed === "object" && parsed !== null) {
      const message = "message" in parsed ? parsed.message : undefined;
      const type = "type" in parsed ? parsed.type : undefined;
      if (typeof message === "string") return message;
      if (typeof type === "string") return type;
    }
  } catch {
    // The wire format also permits plain-text errors.
  }
  return error.length > 120 ? `${error.slice(0, 120)}...` : error;
}

function humanize(value: string): string {
  return value
    .replace(/([a-z])([A-Z])/g, "$1 $2")
    .replace(/[_-]+/g, " ")
    .replace(/^./, (character) => character.toUpperCase());
}
