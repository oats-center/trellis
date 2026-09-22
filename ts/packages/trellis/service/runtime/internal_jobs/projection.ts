import type { Job, JobEvent, JobState } from "./types.ts";

const TERMINAL_STATES = new Set<JobState>([
  "completed",
  "failed",
  "cancelled",
  "expired",
  "skipped",
  "stale",
  "dead",
  "dismissed",
]);

export function isTerminal(state: JobState): boolean {
  return TERMINAL_STATES.has(state);
}

export function jobFromWorkEvent<TPayload = unknown, TResult = unknown>(
  event: JobEvent<TPayload, TResult>,
): Job<TPayload, TResult> | undefined {
  switch (event.eventType) {
    case "created":
    case "retried":
      return seedJobFromEvent(event);
    default:
      return undefined;
  }
}

function seedJobFromEvent<TPayload = unknown, TResult = unknown>(
  event: JobEvent<TPayload, TResult>,
): Job<TPayload, TResult> | undefined {
  if (event.payload === undefined) {
    return undefined;
  }

  return {
    id: event.jobId,
    service: event.service,
    type: event.jobType,
    state: event.state,
    context: event.context,
    payload: event.payload,
    createdAt: event.timestamp,
    updatedAt: event.timestamp,
    tries: event.tries,
    maxTries: event.maxTries ?? 1,
    ...(event.deadline !== undefined ? { deadline: event.deadline } : {}),
    ...(event.trigger !== undefined ? { trigger: event.trigger } : {}),
    ...(event.lineage !== undefined ? { lineage: event.lineage } : {}),
  };
}
