import { type StaticDecode, Type } from "typebox";

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

export type JobTrigger = StaticDecode<typeof JobTriggerSchema>;

export const JobLineageSchema = Type.Object({
  parentJobId: Type.Optional(Type.String({ minLength: 1 })),
  rootJobId: Type.Optional(Type.String({ minLength: 1 })),
  operationId: Type.Optional(Type.String({ minLength: 1 })),
  relatedKeys: Type.Optional(Type.Array(Type.String({ minLength: 1 }))),
});

export type JobLineage = StaticDecode<typeof JobLineageSchema>;

export const PreparedJobSubmissionSchema = Type.Object({
  submissionId: Type.String({ minLength: 1 }),
  mode: Type.Union([Type.Literal("create"), Type.Literal("submit")]),
  service: Type.String({ minLength: 1 }),
  queue: Type.String({ minLength: 1 }),
  jobId: Type.String({ minLength: 1 }),
  payload: Type.Unknown(),
  createdAt: Type.String({ format: "date-time" }),
  context: JobContextSchema,
  trigger: Type.Optional(JobTriggerSchema),
  lineage: Type.Optional(JobLineageSchema),
});

export type PreparedJobSubmission<TPayload = unknown> =
  & Omit<
    StaticDecode<typeof PreparedJobSubmissionSchema>,
    "payload"
  >
  & { payload: TPayload };

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

export type JobWaitTarget = StaticDecode<typeof JobWaitTargetSchema>;

export const JobWaitEdgeSchema = Type.Object({
  id: Type.String({ minLength: 1 }),
  target: JobWaitTargetSchema,
  startedAt: Type.String({ format: "date-time" }),
  label: Type.Optional(Type.String({ minLength: 1 })),
});

export type JobWaitEdge = StaticDecode<typeof JobWaitEdgeSchema>;

export const JobStateSchema = Type.Union([
  Type.Literal("pending"),
  Type.Literal("active"),
  Type.Literal("retry"),
  Type.Literal("completed"),
  Type.Literal("failed"),
  Type.Literal("cancelled"),
  Type.Literal("expired"),
  Type.Literal("skipped"),
  Type.Literal("stale"),
  Type.Literal("dead"),
  Type.Literal("dismissed"),
]);

export type JobState = StaticDecode<typeof JobStateSchema>;

export const JobLogEntrySchema = Type.Object({
  timestamp: Type.String({ format: "date-time" }),
  level: Type.Union([
    Type.Literal("info"),
    Type.Literal("warn"),
    Type.Literal("error"),
  ]),
  message: Type.String(),
});

export type JobLogEntry = StaticDecode<typeof JobLogEntrySchema>;

export const JobProgressSchema = Type.Object({
  step: Type.Optional(Type.String()),
  message: Type.Optional(Type.String()),
  current: Type.Optional(Type.Integer({ minimum: 0 })),
  total: Type.Optional(Type.Integer({ minimum: 0 })),
});

export type JobProgress = StaticDecode<typeof JobProgressSchema>;

export const JobSchema = Type.Object({
  id: Type.String({ minLength: 1 }),
  service: Type.String({ minLength: 1 }),
  type: Type.String({ minLength: 1 }),
  state: JobStateSchema,
  context: JobContextSchema,
  payload: Type.Unknown(),
  result: Type.Optional(Type.Unknown()),
  createdAt: Type.String({ format: "date-time" }),
  updatedAt: Type.String({ format: "date-time" }),
  startedAt: Type.Optional(Type.String({ format: "date-time" })),
  completedAt: Type.Optional(Type.String({ format: "date-time" })),
  tries: Type.Integer({ minimum: 0 }),
  maxTries: Type.Integer({ minimum: 1 }),
  lastError: Type.Optional(Type.String()),
  deadline: Type.Optional(Type.String({ format: "date-time" })),
  progress: Type.Optional(JobProgressSchema),
  logs: Type.Optional(Type.Array(JobLogEntrySchema)),
  trigger: Type.Optional(JobTriggerSchema),
  lineage: Type.Optional(JobLineageSchema),
  waitingOn: Type.Optional(Type.Array(JobWaitEdgeSchema)),
});

export type Job<TPayload = unknown, TResult = unknown> =
  & Omit<StaticDecode<typeof JobSchema>, "payload" | "result">
  & {
    payload: TPayload;
    result?: TResult;
  };

export const JobEventSchema = Type.Object({
  jobId: Type.String({ minLength: 1 }),
  service: Type.String({ minLength: 1 }),
  jobType: Type.String({ minLength: 1 }),
  eventType: Type.Union([
    Type.Literal("created"),
    Type.Literal("started"),
    Type.Literal("retry"),
    Type.Literal("progress"),
    Type.Literal("logged"),
    Type.Literal("completed"),
    Type.Literal("failed"),
    Type.Literal("cancelled"),
    Type.Literal("expired"),
    Type.Literal("skipped"),
    Type.Literal("stale"),
    Type.Literal("heartbeat"),
    Type.Literal("staleCompletionIgnored"),
    Type.Literal("retried"),
    Type.Literal("dead"),
    Type.Literal("dismissed"),
    Type.Literal("waiting"),
    Type.Literal("resumed"),
  ]),
  state: JobStateSchema,
  previousState: Type.Optional(JobStateSchema),
  context: JobContextSchema,
  tries: Type.Integer({ minimum: 0 }),
  maxTries: Type.Optional(Type.Integer({ minimum: 1 })),
  error: Type.Optional(Type.String()),
  progress: Type.Optional(JobProgressSchema),
  logs: Type.Optional(Type.Array(JobLogEntrySchema)),
  payload: Type.Optional(Type.Unknown()),
  result: Type.Optional(Type.Unknown()),
  deadline: Type.Optional(Type.String({ format: "date-time" })),
  trigger: Type.Optional(JobTriggerSchema),
  lineage: Type.Optional(JobLineageSchema),
  waitEdge: Type.Optional(JobWaitEdgeSchema),
  timestamp: Type.String({ format: "date-time" }),
});

export type JobEvent<TPayload = unknown, TResult = unknown> =
  & Omit<StaticDecode<typeof JobEventSchema>, "payload" | "result">
  & {
    payload?: TPayload;
    result?: TResult;
  };

export const WorkerHeartbeatSchema = Type.Object({
  service: Type.String({ minLength: 1 }),
  jobType: Type.String({ minLength: 1 }),
  instanceId: Type.String({ minLength: 1 }),
  concurrency: Type.Optional(Type.Integer({ minimum: 1 })),
  version: Type.Optional(Type.String({ minLength: 1 })),
  timestamp: Type.String({ format: "date-time" }),
});

export type WorkerHeartbeat = StaticDecode<typeof WorkerHeartbeatSchema>;
