import { deepEqual, equal } from "node:assert/strict";

import {
  buildJobTimeline,
  type JobTimelineEvent,
  jobTimelineEventsForAttempt,
} from "./job_event_timeline.ts";

declare const Deno: {
  test(name: string, fn: () => void): void;
};

const start = new Date("2026-07-10T20:00:40.000Z");

function event(
  sequence: number,
  type: string,
  state: string,
  milliseconds: number,
  values: Partial<JobTimelineEvent> = {},
): JobTimelineEvent {
  return {
    sequence,
    type,
    state,
    timestamp: new Date(start.getTime() + milliseconds).toISOString(),
    tries: 1,
    ...values,
  };
}

Deno.test("buildJobTimeline creates an execution story and nests dependency waits", () => {
  const waitEdge = {
    id: "wait-ai",
    startedAt: new Date(start.getTime() + 35).toISOString(),
    target: {
      kind: "external" as const,
      system: "ai",
      operation: "chat-completions",
      key: "glm-ocr:latest",
      label: "AI API",
    },
  };
  const timeline = buildJobTimeline([
    event(1, "created", "pending", -580_000),
    event(2, "started", "active", 0, { previousState: "pending" }),
    event(3, "progress", "active", 17, {
      progress: { step: "reading-input", message: "Reading OCR input." },
    }),
    event(4, "progress", "active", 31, {
      progress: {
        step: "ocr-page",
        message: "Extracting page 1.",
        current: 1,
        total: 1,
      },
    }),
    event(5, "waiting", "active", 41, { waitEdge }),
    event(6, "resumed", "active", 2_225, { waitEdge }),
    event(7, "progress", "active", 2_231, {
      progress: {
        step: "completed",
        message: "OCR completed.",
        current: 1,
        total: 1,
      },
    }),
    event(8, "completed", "completed", 2_243, { previousState: "active" }),
  ]);

  deepEqual(timeline.map((phase) => phase.kind), [
    "queue",
    "execution",
    "outcome",
  ]);

  const queue = timeline[0];
  equal(queue.kind, "queue");
  if (queue.kind !== "queue") return;
  equal(queue.duration, "9m 40.000s");
  equal(queue.transition, "pending → active");

  const execution = timeline[1];
  equal(execution.kind, "execution");
  if (execution.kind !== "execution") return;
  equal(execution.duration, "2.243s");
  equal(execution.steps.length, 3);
  equal(execution.steps[1].detail, "ocr-page · 1/1");
  equal(execution.steps[1].waits.length, 1);
  equal(execution.steps[1].waits[0].label, "AI API");
  equal(execution.steps[1].waits[0].duration, "2.190s");
  equal(execution.steps[1].waits[0].type, "waiting → resumed");

  const outcome = timeline[2];
  equal(outcome.kind, "outcome");
  if (outcome.kind !== "outcome") return;
  equal(outcome.label, "Completed");
  equal(outcome.duration, "2.243s");
});

Deno.test("buildJobTimeline keeps dependency waits without a preceding step at execution level", () => {
  const waitEdge = {
    id: "wait-job",
    startedAt: start.toISOString(),
    target: { kind: "job" as const, id: "job-2" },
  };
  const timeline = buildJobTimeline([
    event(1, "started", "active", 0),
    event(2, "waiting", "active", 5, { waitEdge }),
    event(3, "resumed", "active", 25, { waitEdge }),
  ]);

  const execution = timeline[0];
  equal(execution.kind, "execution");
  if (execution.kind !== "execution") return;
  equal(execution.waits.length, 1);
  equal(execution.waits[0].duration, "25ms");
});

Deno.test("buildJobTimeline retains unmatched and uncommon runtime events", () => {
  const resumed = event(1, "resumed", "active", 0, {
    waitEdge: {
      id: "missing-wait",
      startedAt: start.toISOString(),
      target: { kind: "job", id: "job-2" },
    },
  });
  const heartbeat = event(2, "heartbeat", "active", 1);
  const timeline = buildJobTimeline([heartbeat, resumed]);

  equal(timeline.length, 1);
  const execution = timeline[0];
  equal(execution.kind, "execution");
  if (execution.kind !== "execution") return;
  deepEqual(execution.events.map((item) => item.label), [
    "Resumed",
    "Worker heartbeat",
  ]);
});

Deno.test("buildJobTimeline preserves terminal error evidence", () => {
  const timeline = buildJobTimeline([
    event(1, "started", "active", 0),
    event(2, "error", "failed", 243, {
      error: JSON.stringify({ message: "Model request timed out" }),
      previousState: "active",
    }),
  ]);

  const outcome = timeline[1];
  equal(outcome.kind, "outcome");
  if (outcome.kind !== "outcome") return;
  equal(outcome.label, "Failed");
  equal(outcome.detail, "Model request timed out");
  equal(outcome.duration, "243ms");
});

Deno.test("buildJobTimeline represents retries as a new queue and execution cycle", () => {
  const timeline = buildJobTimeline([
    event(1, "created", "pending", 0),
    event(2, "started", "active", 100),
    event(3, "retry", "retry", 250, { previousState: "active" }),
    event(4, "started", "active", 500, { previousState: "retry", tries: 2 }),
    event(5, "completed", "completed", 800, {
      previousState: "active",
      tries: 2,
    }),
  ]);

  deepEqual(timeline.map((phase) => phase.kind), [
    "queue",
    "execution",
    "outcome",
    "queue",
    "execution",
    "outcome",
  ]);
  const retryQueue = timeline[3];
  equal(retryQueue.kind, "queue");
  if (retryQueue.kind !== "queue") return;
  equal(retryQueue.label, "Retry delay");
  equal(retryQueue.duration, "250ms");
});

Deno.test("buildJobTimeline labels queued terminal duration separately from runtime", () => {
  const timeline = buildJobTimeline([
    event(1, "created", "pending", 0, { tries: 0 }),
    event(2, "cancelled", "cancelled", 243, {
      previousState: "pending",
      tries: 0,
    }),
  ]);

  const outcome = timeline[1];
  equal(outcome.kind, "outcome");
  if (outcome.kind !== "outcome") return;
  equal(outcome.duration, "243ms");
  equal(outcome.durationKind, "queue");
});

Deno.test("jobTimelineEventsForAttempt includes the preceding queue boundary", () => {
  const created = event(1, "created", "pending", 0, { tries: 0 });
  const started = event(2, "started", "active", 100, {
    previousState: "pending",
    tries: 1,
  });
  const completed = event(3, "completed", "completed", 200, {
    previousState: "active",
    tries: 1,
  });

  deepEqual(
    jobTimelineEventsForAttempt([created, started, completed], 1).map((item) =>
      item.sequence
    ),
    [1, 2, 3],
  );

  const retry = event(4, "retry", "retry", 300, {
    previousState: "active",
    tries: 1,
  });
  const restarted = event(5, "started", "active", 500, {
    previousState: "retry",
    tries: 2,
  });
  const selectedRetry = jobTimelineEventsForAttempt(
    [created, started, completed, retry, restarted],
    2,
  );
  deepEqual(selectedRetry.map((item) => item.sequence), [4, 5]);
  deepEqual(buildJobTimeline(selectedRetry).map((phase) => phase.kind), [
    "queue",
    "execution",
  ]);

  const priorAttempt = jobTimelineEventsForAttempt(
    [created, started, retry, restarted],
    1,
  );
  deepEqual(priorAttempt.map((item) => item.sequence), [1, 2, 4]);
  deepEqual(buildJobTimeline(priorAttempt).map((phase) => phase.kind), [
    "queue",
    "execution",
    "outcome",
  ]);
});

Deno.test("buildJobTimeline preserves recorded log entries", () => {
  const timeline = buildJobTimeline([
    event(1, "started", "active", 0),
    event(2, "logged", "active", 10, {
      logs: [{
        timestamp: new Date(start.getTime() + 9).toISOString(),
        level: "warn",
        message: "OCR response omitted page confidence",
      }],
    }),
  ]);

  const execution = timeline[0];
  equal(execution.kind, "execution");
  if (execution.kind !== "execution") return;
  equal(execution.events[0].logs?.[0].level, "warn");
  equal(
    execution.events[0].logs?.[0].message,
    "OCR response omitted page confidence",
  );
});
