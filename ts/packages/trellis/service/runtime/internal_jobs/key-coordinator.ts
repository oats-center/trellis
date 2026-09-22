import { type KV, type KvEntry, Kvm } from "@nats-io/kv";
import type { NatsConnection } from "@nats-io/nats-core";
import { ulid } from "ulid";

import type { JobContext, JobEvent, JobState } from "./types.ts";

const JOBS_KEYS_BUCKET = "JOBS_KEYS";
const DEFAULT_HEARTBEAT_INTERVAL_MS = 30_000;
const DEFAULT_HEARTBEAT_TTL_MS = 120_000;
const DEFAULT_MAX_ACTIVE = 1;
const DEFAULT_MAX_QUEUED_PER_KEY = 0;
const DEFAULT_QUEUE_WHEN_FULL = "reject";
const DEFAULT_STALE_POLICY = "fail-stale";
const MAX_CAS_ATTEMPTS = 8;

export type JobKeyStalePolicy = "fail-stale" | "block";
export type JobQueueWhenFull = "reject" | "coalesce" | "replace-oldest";

export type JobKeyConcurrencyBinding = {
  key: string[];
  maxActive?: number;
  heartbeatIntervalMs?: number;
  heartbeatTtlMs?: number;
  stalePolicy?: JobKeyStalePolicy;
};

export type JobQueuePolicyBinding = {
  maxQueuedPerKey?: number;
  whenFull?: JobQueueWhenFull;
};

export type NormalizedJobKeyPolicy = {
  key: string[];
  maxActive: number;
  heartbeatIntervalMs: number;
  heartbeatTtlMs: number;
  stalePolicy: JobKeyStalePolicy;
  queue: {
    maxQueuedPerKey: number;
    whenFull: JobQueueWhenFull;
  };
};

export type DerivedJobKey = {
  service: string;
  jobType: string;
  key: string;
  keyHash: string;
  kvKey: string;
};

export type JobKeyQueued = {
  jobId: string;
  createdAt: string;
  requestId: string;
  context: JobContext;
  submissionId?: string;
};

export type JobKeyActiveSlot = {
  jobId: string;
  slotToken: string;
  instanceId: string;
  startedAt: string;
  heartbeatAt: string;
  leaseExpiresAt: string;
  tries: number;
  context: JobContext;
};

export type JobKeyState = {
  version: 1;
  service: string;
  jobType: string;
  key: string;
  keyHash: string;
  maxActive: number;
  maxQueuedPerKey?: number;
  active: JobKeyActiveSlot[];
  queued: JobKeyQueued[];
  staleTakeoverCount: number;
  updatedAt: string;
  coalescedBySubmissionId?: Record<
    string,
    { existingJobId: string; reason: "queue-full" | "active-limit" }
  >;
  acceptedBySubmissionId?: Record<string, { jobId: string }>;
  replacedBySubmissionId?: Record<string, ReplacedQueuedJob>;
};

export type JobKeyIdentity = {
  service: string;
  jobType: string;
  id: string;
};

export type ReplacedQueuedJob = JobKeyIdentity & {
  createdAt: string;
  requestId: string;
  context: JobContext;
};

export type JobAdmissionRequest = {
  service: string;
  jobType: string;
  jobId: string;
  payload: unknown;
  context: JobContext;
  createdAt: string;
  policy: NormalizedJobKeyPolicy;
  strictCreate: boolean;
  submissionId?: string;
};

export type JobAdmissionOutcome =
  | { kind: "accepted"; key: string; keyHash: string; state: JobKeyState }
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
    existing: JobKeyIdentity;
    reason: "queue-full" | "active-limit";
  }
  | {
    kind: "replaced";
    key: string;
    keyHash: string;
    replaced: ReplacedQueuedJob;
    state: JobKeyState;
  };

export type ActiveSlotAcquireRequest = {
  service: string;
  jobType: string;
  jobId: string;
  payload: unknown;
  context: JobContext;
  workEventType?: JobEvent["eventType"];
  lifecycleState?: JobState;
  tries: number;
  instanceId: string;
  now: string;
  policy: NormalizedJobKeyPolicy;
};

export type ActiveSlotAcquireOutcome =
  | {
    kind: "acquired";
    key: string;
    keyHash: string;
    slotToken: string;
    stale: JobKeyActiveSlot[];
    state: JobKeyState;
  }
  | {
    kind: "blocked";
    key: string;
    reason: "active-limit" | "not-queued" | "stale-blocked";
    active: number;
    queued: number;
    limit: number;
  };

export type ActiveSlotLease = {
  key: string;
  keyHash: string;
  slotToken: string;
  policy: NormalizedJobKeyPolicy;
};

export type ActiveSlotRenewOutcome =
  | { kind: "renewed"; state: JobKeyState }
  | { kind: "lost" };

export type ActiveSlotReleaseOutcome =
  | { kind: "released"; state: JobKeyState }
  | { kind: "staleCompletion" };

export type JobKeyCoordinator = {
  admitCreate(request: JobAdmissionRequest): Promise<JobAdmissionOutcome>;
  restoreReplacedQueuedJob(args: {
    service: string;
    jobType: string;
    replacementJobId: string;
    replaced: ReplacedQueuedJob;
    payload: unknown;
    now: string;
    policy: NormalizedJobKeyPolicy;
    submissionId?: string;
  }): Promise<QueuedJobRestoreOutcome>;
  removeQueuedJob(args: {
    service: string;
    jobType: string;
    jobId: string;
    payload: unknown;
    now: string;
    policy: NormalizedJobKeyPolicy;
    submissionId?: string;
  }): Promise<QueuedJobRemovalOutcome>;
  removeQueuedJobById?(args: {
    service: string;
    jobType: string;
    jobId: string;
    now: string;
  }): Promise<QueuedJobRemovalOutcome>;
  acquireActiveSlot(
    request: ActiveSlotAcquireRequest,
  ): Promise<ActiveSlotAcquireOutcome>;
  renewHeartbeat(args: {
    service: string;
    jobType: string;
    jobId: string;
    lease: ActiveSlotLease;
    now: string;
  }): Promise<ActiveSlotRenewOutcome>;
  releaseActiveSlot(args: {
    service: string;
    jobType: string;
    jobId: string;
    lease: ActiveSlotLease;
    now: string;
  }): Promise<ActiveSlotReleaseOutcome>;
};

export type QueuedJobRemovalOutcome =
  | { kind: "removed"; state: JobKeyState }
  | { kind: "not-found" };

export type QueuedJobRestoreOutcome = { kind: "restored"; state: JobKeyState };

/** Normalizes keyed queue policy defaults when bindings omit normalized values. */
export function normalizeJobKeyPolicy(args: {
  keyConcurrency: JobKeyConcurrencyBinding;
  queue?: JobQueuePolicyBinding;
}): NormalizedJobKeyPolicy {
  return {
    key: [...args.keyConcurrency.key],
    maxActive: args.keyConcurrency.maxActive ?? DEFAULT_MAX_ACTIVE,
    heartbeatIntervalMs: args.keyConcurrency.heartbeatIntervalMs ??
      DEFAULT_HEARTBEAT_INTERVAL_MS,
    heartbeatTtlMs: args.keyConcurrency.heartbeatTtlMs ??
      DEFAULT_HEARTBEAT_TTL_MS,
    stalePolicy: args.keyConcurrency.stalePolicy ?? DEFAULT_STALE_POLICY,
    queue: {
      maxQueuedPerKey: args.queue?.maxQueuedPerKey ??
        DEFAULT_MAX_QUEUED_PER_KEY,
      whenFull: args.queue?.whenFull ?? DEFAULT_QUEUE_WHEN_FULL,
    },
  };
}

/** Derives the display key and stable hash for a keyed job payload. */
export async function deriveJobKey(args: {
  service: string;
  jobType: string;
  payload: unknown;
  template: string[];
}): Promise<DerivedJobKey> {
  const segments = args.template.map((segment) => {
    if (!segment.startsWith("/")) {
      return segment;
    }
    const value = resolveJsonPointer(args.payload, segment);
    if (!isScalarKeySegment(value)) {
      throw new Error(
        `Job key pointer '${segment}' did not resolve to a string, finite number, or boolean`,
      );
    }
    return value;
  });
  const key = segments.map((segment) => String(segment)).join(":");
  const keyHash = await stableKeyHash(
    JSON.stringify({ version: 1, segments }),
  );
  return {
    service: args.service,
    jobType: args.jobType,
    key,
    keyHash,
    kvKey: `${args.service}.${args.jobType}.${keyHash}`,
  };
}

/** Produces a stable SHA-256 hex hash for a display key. */
export async function stableKeyHash(value: string): Promise<string> {
  const digest = await crypto.subtle.digest(
    "SHA-256",
    new TextEncoder().encode(value),
  );
  return Array.from(
    new Uint8Array(digest),
    (byte) => byte.toString(16).padStart(2, "0"),
  ).join("");
}

/** Applies keyed admission policy to an existing key state. */
export function reduceAdmission(args: {
  state: JobKeyState | undefined;
  derived: DerivedJobKey;
  request: Omit<JobAdmissionRequest, "payload" | "policy">;
  policy: NormalizedJobKeyPolicy;
}): JobAdmissionOutcome {
  const state = args.state ?? emptyState({
    service: args.request.service,
    jobType: args.request.jobType,
    key: args.derived.key,
    keyHash: args.derived.keyHash,
    now: args.request.createdAt,
    policy: args.policy,
  });

  // Idempotent retry: if the same submissionId has already produced an outcome,
  // return it without modifying state again.
  const submissionId = args.request.submissionId;
  if (submissionId) {
    const replacedEntry = state.replacedBySubmissionId?.[submissionId];
    if (replacedEntry) {
      return {
        kind: "replaced",
        key: state.key,
        keyHash: state.keyHash,
        replaced: replacedEntry,
        state,
      };
    }

    const coalescedEntry = state.coalescedBySubmissionId?.[submissionId];
    if (coalescedEntry) {
      return {
        kind: "coalesced",
        key: state.key,
        existing: {
          service: state.service,
          jobType: state.jobType,
          id: coalescedEntry.existingJobId,
        },
        reason: coalescedEntry.reason,
      };
    }

    if (state.acceptedBySubmissionId?.[submissionId]) {
      return {
        kind: "accepted",
        key: state.key,
        keyHash: state.keyHash,
        state,
      };
    }

    const alreadyQueued = state.queued.some((entry) =>
      entry.submissionId === submissionId
    );
    if (alreadyQueued) {
      return {
        kind: "accepted",
        key: state.key,
        keyHash: state.keyHash,
        state,
      };
    }
  }

  const active = state.active.length;
  const queued = state.queued.length;
  const queueLimit = args.policy.queue.maxQueuedPerKey +
    Math.max(0, args.policy.maxActive - active);

  if (queued < queueLimit) {
    return {
      kind: "accepted",
      key: state.key,
      keyHash: state.keyHash,
      state: appendQueued(state, args.request, args.policy),
    };
  }

  if (args.request.strictCreate) {
    const reason = active >= args.policy.maxActive
      ? "active-limit"
      : "queue-depth";
    return {
      kind: "rejected",
      key: state.key,
      reason,
      active,
      queued,
      limit: reason === "active-limit" ? args.policy.maxActive : queueLimit,
    };
  }

  if (args.policy.queue.whenFull === "coalesce") {
    const existing = state.queued[0] ?? state.active[0];
    if (existing) {
      const reason = state.queued[0] ? "queue-full" : "active-limit";
      const outcome: {
        kind: "coalesced";
        key: string;
        existing: JobKeyIdentity;
        reason: "queue-full" | "active-limit";
        state?: JobKeyState;
      } = {
        kind: "coalesced",
        key: state.key,
        existing: {
          service: state.service,
          jobType: state.jobType,
          id: existing.jobId,
        },
        reason,
      };
      if (submissionId) {
        outcome.state = {
          ...state,
          coalescedBySubmissionId: {
            ...state.coalescedBySubmissionId,
            [submissionId]: { existingJobId: existing.jobId, reason },
          },
          updatedAt: args.request.createdAt,
        };
      }
      return outcome;
    }
  }

  if (args.policy.queue.whenFull === "replace-oldest" && state.queued[0]) {
    const [replaced, ...remaining] = state.queued;
    const replacedEntry: ReplacedQueuedJob = {
      service: state.service,
      jobType: state.jobType,
      id: replaced.jobId,
      createdAt: replaced.createdAt,
      requestId: replaced.requestId,
      context: replaced.context,
    };
    const nextState = appendQueued(
      { ...state, queued: remaining },
      args.request,
      args.policy,
    );
    if (submissionId) {
      nextState.replacedBySubmissionId = {
        ...state.replacedBySubmissionId,
        [submissionId]: replacedEntry,
      };
    }
    return {
      kind: "replaced",
      key: state.key,
      keyHash: state.keyHash,
      replaced: replacedEntry,
      state: nextState,
    };
  }

  const reason = active >= args.policy.maxActive
    ? "active-limit"
    : "queue-depth";
  return {
    kind: "rejected",
    key: state.key,
    reason,
    active,
    queued,
    limit: reason === "active-limit" ? args.policy.maxActive : queueLimit,
  };
}

/** Restores the replaced queued reservation when replacement lifecycle publish fails. */
export function reduceRestoreReplacedQueuedJob(args: {
  state: JobKeyState | undefined;
  derived: DerivedJobKey;
  replacementJobId: string;
  replaced: ReplacedQueuedJob;
  now: string;
  policy: NormalizedJobKeyPolicy;
  submissionId?: string;
}): QueuedJobRestoreOutcome {
  const base = args.state ?? emptyState({
    service: args.replaced.service,
    jobType: args.replaced.jobType,
    key: args.derived.key,
    keyHash: args.derived.keyHash,
    now: args.now,
    policy: args.policy,
  });
  const queuedWithoutReplacement = base.queued.filter((entry) =>
    entry.jobId !== args.replacementJobId
  );
  const alreadyPresent =
    queuedWithoutReplacement.some((entry) =>
      entry.jobId === args.replaced.id
    ) || base.active.some((slot) => slot.jobId === args.replaced.id);
  const restoredQueued = alreadyPresent ? queuedWithoutReplacement : [{
    jobId: args.replaced.id,
    createdAt: args.replaced.createdAt,
    requestId: args.replaced.requestId,
    context: args.replaced.context,
  }, ...queuedWithoutReplacement];

  const replacedBySubmissionId = { ...base.replacedBySubmissionId };
  const acceptedBySubmissionId = { ...base.acceptedBySubmissionId };
  if (args.submissionId) {
    delete replacedBySubmissionId[args.submissionId];
    delete acceptedBySubmissionId[args.submissionId];
  }

  return {
    kind: "restored",
    state: {
      ...base,
      maxActive: args.policy.maxActive,
      maxQueuedPerKey: args.policy.queue.maxQueuedPerKey,
      queued: restoredQueued,
      replacedBySubmissionId: Object.keys(replacedBySubmissionId).length > 0
        ? replacedBySubmissionId
        : undefined,
      acceptedBySubmissionId: Object.keys(acceptedBySubmissionId).length > 0
        ? acceptedBySubmissionId
        : undefined,
      updatedAt: args.now,
    },
  };
}

/** Applies active slot acquisition policy to an existing key state. */
export function reduceAcquireActiveSlot(args: {
  state: JobKeyState | undefined;
  derived: DerivedJobKey;
  request: Omit<ActiveSlotAcquireRequest, "payload" | "policy">;
  policy: NormalizedJobKeyPolicy;
  slotToken: string;
}): ActiveSlotAcquireOutcome {
  const base = args.state ?? emptyState({
    service: args.request.service,
    jobType: args.request.jobType,
    key: args.derived.key,
    keyHash: args.derived.keyHash,
    now: args.request.now,
    policy: args.policy,
  });
  const nowMs = Date.parse(args.request.now);
  const expired = base.active.filter((slot) =>
    Date.parse(slot.leaseExpiresAt) <= nowMs
  );
  if (
    expired.length > 0 && args.policy.stalePolicy === "block" &&
    base.active.length >= args.policy.maxActive
  ) {
    return {
      kind: "blocked",
      key: base.key,
      reason: "stale-blocked",
      active: base.active.length,
      queued: base.queued.length,
      limit: args.policy.maxActive,
    };
  }
  const active = args.policy.stalePolicy === "fail-stale"
    ? base.active.filter((slot) => Date.parse(slot.leaseExpiresAt) > nowMs)
    : base.active;
  const isQueued = base.queued.some((entry) =>
    entry.jobId === args.request.jobId
  );
  const isAlreadyActive = base.active.some((slot) =>
    slot.jobId === args.request.jobId
  );
  const isRetry = args.request.lifecycleState === "retry" ||
    args.request.workEventType === "retried";
  const isActiveRedelivery = args.request.lifecycleState === "active";
  if (!isQueued && !isAlreadyActive && !isRetry && !isActiveRedelivery) {
    return {
      kind: "blocked",
      key: base.key,
      reason: "not-queued",
      active: active.length,
      queued: base.queued.length,
      limit: args.policy.queue.maxQueuedPerKey,
    };
  }
  if (active.length >= args.policy.maxActive) {
    return {
      kind: "blocked",
      key: base.key,
      reason: "active-limit",
      active: active.length,
      queued: base.queued.length,
      limit: args.policy.maxActive,
    };
  }

  const leaseExpiresAt = new Date(nowMs + args.policy.heartbeatTtlMs)
    .toISOString();
  const queued = base.queued.filter((entry) =>
    entry.jobId !== args.request.jobId
  );
  const slot: JobKeyActiveSlot = {
    jobId: args.request.jobId,
    slotToken: args.slotToken,
    instanceId: args.request.instanceId,
    startedAt: args.request.now,
    heartbeatAt: args.request.now,
    leaseExpiresAt,
    tries: args.request.tries,
    context: args.request.context,
  };
  const state: JobKeyState = {
    ...base,
    maxActive: args.policy.maxActive,
    maxQueuedPerKey: args.policy.queue.maxQueuedPerKey,
    active: [...active, slot],
    queued,
    staleTakeoverCount: base.staleTakeoverCount + expired.length,
    updatedAt: args.request.now,
  };
  return {
    kind: "acquired",
    key: base.key,
    keyHash: base.keyHash,
    slotToken: args.slotToken,
    stale: args.policy.stalePolicy === "fail-stale" ? expired : [],
    state,
  };
}

/** Removes a queued keyed-job reservation when work is terminal before start. */
export function reduceRemoveQueuedJob(args: {
  state: JobKeyState | undefined;
  jobId: string;
  now: string;
  submissionId?: string;
}): QueuedJobRemovalOutcome {
  if (!args.state) {
    return { kind: "not-found" };
  }
  const queued = args.state.queued.filter((entry) =>
    entry.jobId !== args.jobId
  );
  if (queued.length === args.state.queued.length) {
    return { kind: "not-found" };
  }
  const acceptedBySubmissionId = { ...args.state.acceptedBySubmissionId };
  if (args.submissionId) {
    delete acceptedBySubmissionId[args.submissionId];
  }
  return {
    kind: "removed",
    state: {
      ...args.state,
      queued,
      acceptedBySubmissionId: Object.keys(acceptedBySubmissionId).length > 0
        ? acceptedBySubmissionId
        : undefined,
      updatedAt: args.now,
    },
  };
}

/** Renews a matching active slot lease in key state. */
export function reduceRenewHeartbeat(args: {
  state: JobKeyState | undefined;
  jobId: string;
  slotToken: string;
  now: string;
  policy: NormalizedJobKeyPolicy;
}): ActiveSlotRenewOutcome {
  if (!args.state) {
    return { kind: "lost" };
  }
  const nowMs = Date.parse(args.now);
  let renewed = false;
  const active = args.state.active.map((slot) => {
    if (slot.jobId !== args.jobId || slot.slotToken !== args.slotToken) {
      return slot;
    }
    renewed = true;
    return {
      ...slot,
      heartbeatAt: args.now,
      leaseExpiresAt: new Date(nowMs + args.policy.heartbeatTtlMs)
        .toISOString(),
    };
  });
  if (!renewed) {
    return { kind: "lost" };
  }
  return {
    kind: "renewed",
    state: { ...args.state, active, updatedAt: args.now },
  };
}

/** Releases a matching active slot or reports a stale completion. */
export function reduceReleaseActiveSlot(args: {
  state: JobKeyState | undefined;
  jobId: string;
  slotToken: string;
  now: string;
}): ActiveSlotReleaseOutcome {
  if (!args.state) {
    return { kind: "staleCompletion" };
  }
  const nextActive = args.state.active.filter((slot) =>
    slot.jobId !== args.jobId || slot.slotToken !== args.slotToken
  );
  if (nextActive.length === args.state.active.length) {
    return { kind: "staleCompletion" };
  }
  return {
    kind: "released",
    state: { ...args.state, active: nextActive, updatedAt: args.now },
  };
}

/** Creates a NATS KV backed coordinator for the Trellis-owned JOBS_KEYS bucket. */
export function createNatsJobKeyCoordinator(
  nats: NatsConnection,
  bucketPrefix = JOBS_KEYS_BUCKET,
): JobKeyCoordinator {
  const kvPromises = new Map<string, Promise<KV>>();
  const openKv = (service: string): Promise<KV> => {
    const bucket = jobKeysBucketName(bucketPrefix, service);
    const existing = kvPromises.get(bucket);
    if (existing) {
      return existing;
    }
    const opened = new Kvm(nats).open(bucket);
    kvPromises.set(bucket, opened);
    return opened;
  };

  return {
    async admitCreate(request) {
      const derived = await deriveJobKey({
        service: request.service,
        jobType: request.jobType,
        payload: request.payload,
        template: request.policy.key,
      });
      return await updateState(
        () => openKv(request.service),
        derived,
        (state) =>
          reduceAdmission({
            state,
            derived,
            request,
            policy: request.policy,
          }),
      );
    },
    async restoreReplacedQueuedJob(args) {
      const derived = await deriveJobKey({
        service: args.service,
        jobType: args.jobType,
        payload: args.payload,
        template: args.policy.key,
      });
      return await updateState(
        () => openKv(args.service),
        derived,
        (state) =>
          reduceRestoreReplacedQueuedJob({
            state,
            derived,
            replacementJobId: args.replacementJobId,
            replaced: args.replaced,
            now: args.now,
            policy: args.policy,
            submissionId: args.submissionId,
          }),
      );
    },
    async removeQueuedJob(args) {
      const derived = await deriveJobKey({
        service: args.service,
        jobType: args.jobType,
        payload: args.payload,
        template: args.policy.key,
      });
      return await updateState(
        () => openKv(args.service),
        derived,
        (state) =>
          reduceRemoveQueuedJob({
            state,
            jobId: args.jobId,
            now: args.now,
            submissionId: args.submissionId,
          }),
      );
    },
    async removeQueuedJobById(args) {
      const kv = await openKv(args.service);
      const keys = await kv.keys(`${args.service}.${args.jobType}.>`);
      for await (const kvKey of keys) {
        const entry = await getStateEntry(kv, kvKey);
        const state = entry?.json<unknown>();
        if (
          !isJobKeyState(state) || state.service !== args.service ||
          state.jobType !== args.jobType ||
          !state.queued.some((queued) => queued.jobId === args.jobId)
        ) {
          continue;
        }
        return await updateState(
          () => Promise.resolve(kv),
          {
            service: state.service,
            jobType: state.jobType,
            key: state.key,
            keyHash: state.keyHash,
            kvKey,
          },
          (current) =>
            reduceRemoveQueuedJob({
              state: current,
              jobId: args.jobId,
              now: args.now,
            }),
        );
      }
      return { kind: "not-found" };
    },
    async acquireActiveSlot(request) {
      const derived = await deriveJobKey({
        service: request.service,
        jobType: request.jobType,
        payload: request.payload,
        template: request.policy.key,
      });
      const slotToken = ulid();
      return await updateState(
        () => openKv(request.service),
        derived,
        (state) =>
          reduceAcquireActiveSlot({
            state,
            derived,
            request,
            policy: request.policy,
            slotToken,
          }),
      );
    },
    async renewHeartbeat(args) {
      const derived = {
        service: args.service,
        jobType: args.jobType,
        key: args.lease.key,
        keyHash: args.lease.keyHash,
        kvKey: `${args.service}.${args.jobType}.${args.lease.keyHash}`,
      };
      return await updateState(
        () => openKv(args.service),
        derived,
        (state) =>
          reduceRenewHeartbeat({
            state,
            jobId: args.jobId,
            slotToken: args.lease.slotToken,
            now: args.now,
            policy: args.lease.policy,
          }),
      );
    },
    async releaseActiveSlot(args) {
      const derived = {
        service: args.service,
        jobType: args.jobType,
        key: args.lease.key,
        keyHash: args.lease.keyHash,
        kvKey: `${args.service}.${args.jobType}.${args.lease.keyHash}`,
      };
      return await updateState(
        () => openKv(args.service),
        derived,
        (state) =>
          reduceReleaseActiveSlot({
            state,
            jobId: args.jobId,
            slotToken: args.lease.slotToken,
            now: args.now,
          }),
      );
    },
  };
}

function jobKeysBucketName(prefix: string, service: string): string {
  return `${prefix}_${service}`;
}

function appendQueued(
  state: JobKeyState,
  request: Omit<JobAdmissionRequest, "payload" | "policy">,
  policy: NormalizedJobKeyPolicy,
): JobKeyState {
  return {
    ...state,
    maxActive: policy.maxActive,
    maxQueuedPerKey: policy.queue.maxQueuedPerKey,
    queued: [
      ...state.queued,
      {
        jobId: request.jobId,
        createdAt: request.createdAt,
        requestId: request.context.requestId,
        context: request.context,
        ...(request.submissionId ? { submissionId: request.submissionId } : {}),
      },
    ],
    acceptedBySubmissionId: request.submissionId
      ? {
        ...state.acceptedBySubmissionId,
        [request.submissionId]: { jobId: request.jobId },
      }
      : state.acceptedBySubmissionId,
    updatedAt: request.createdAt,
  };
}

function emptyState(args: {
  service: string;
  jobType: string;
  key: string;
  keyHash: string;
  now: string;
  policy: NormalizedJobKeyPolicy;
}): JobKeyState {
  return {
    version: 1,
    service: args.service,
    jobType: args.jobType,
    key: args.key,
    keyHash: args.keyHash,
    maxActive: args.policy.maxActive,
    maxQueuedPerKey: args.policy.queue.maxQueuedPerKey,
    active: [],
    queued: [],
    staleTakeoverCount: 0,
    updatedAt: args.now,
  };
}

function resolveJsonPointer(payload: unknown, pointer: string): unknown {
  const tokens = pointer.slice(1).split("/").map((token) =>
    token.replaceAll("~1", "/").replaceAll("~0", "~")
  );
  let current = payload;
  for (const token of tokens) {
    if (Array.isArray(current)) {
      if (!/^0$|^[1-9][0-9]*$/.test(token)) {
        return undefined;
      }
      current = current[Number(token)];
      continue;
    }
    if (current !== null && typeof current === "object") {
      current = (current as Record<string, unknown>)[token];
      continue;
    }
    return undefined;
  }
  return current;
}

function isScalarKeySegment(
  value: unknown,
): value is string | number | boolean {
  return typeof value === "string" ||
    (typeof value === "number" && Number.isFinite(value)) ||
    typeof value === "boolean";
}

async function updateState<TOutcome>(
  openKv: () => Promise<KV>,
  derived: DerivedJobKey,
  reducer: (state: JobKeyState | undefined) => TOutcome,
): Promise<TOutcome> {
  const kv = await openKv();
  for (let attempt = 0; attempt < MAX_CAS_ATTEMPTS; attempt += 1) {
    const entry = await getStateEntry(kv, derived.kvKey);
    const current = entry ? parseState(entry, derived) : undefined;
    const outcome = reducer(current);
    const next = outcomeState(outcome);
    if (!next) {
      return outcome;
    }
    try {
      if (entry) {
        await kv.update(derived.kvKey, JSON.stringify(next), entry.revision);
      } else {
        await kv.create(derived.kvKey, JSON.stringify(next));
      }
      return outcome;
    } catch (error) {
      if (!isCasConflict(error)) {
        throw error;
      }
    }
  }
  throw new Error(`Could not update keyed job state '${derived.kvKey}'`);
}

async function getStateEntry(
  kv: KV,
  key: string,
): Promise<KvEntry | undefined> {
  const entry = await kv.get(key);
  if (!entry || entry.operation === "DEL" || entry.operation === "PURGE") {
    return undefined;
  }
  return entry;
}

function parseState(
  entry: KvEntry,
  derived: DerivedJobKey,
): JobKeyState | undefined {
  const decoded = entry.json<unknown>();
  if (!isJobKeyState(decoded)) {
    return undefined;
  }
  assertStateMatchesDerived(decoded, derived);
  return decoded;
}

function assertStateMatchesDerived(
  state: JobKeyState,
  derived: DerivedJobKey,
): void {
  if (
    state.service !== derived.service || state.jobType !== derived.jobType ||
    state.keyHash !== derived.keyHash
  ) {
    throw new Error(
      `keyed job state '${derived.kvKey}' does not match derived service, job type, and key hash`,
    );
  }
}

/** Returns whether an unknown value is a well-formed keyed job coordination state. */
export function isJobKeyState(value: unknown): value is JobKeyState {
  if (value === null || typeof value !== "object") return false;
  const state = value as Partial<JobKeyState>;
  return state.version === 1 && isNonEmptyString(state.service) &&
    isNonEmptyString(state.jobType) && typeof state.key === "string" &&
    isNonEmptyString(state.keyHash) && isPositiveInteger(state.maxActive) &&
    (state.maxQueuedPerKey === undefined ||
      isNonNegativeInteger(state.maxQueuedPerKey)) &&
    Array.isArray(state.active) && state.active.every(isJobKeyActiveSlot) &&
    Array.isArray(state.queued) && state.queued.every(isJobKeyQueued) &&
    isNonNegativeInteger(state.staleTakeoverCount) &&
    isValidIsoTimestamp(state.updatedAt) &&
    (state.acceptedBySubmissionId === undefined ||
      isRecordOfAccepted(state.acceptedBySubmissionId)) &&
    (state.coalescedBySubmissionId === undefined ||
      isRecordOfCoalesced(state.coalescedBySubmissionId)) &&
    (state.replacedBySubmissionId === undefined ||
      isRecordOfReplaced(state.replacedBySubmissionId));
}

function isRecordOfAccepted(
  value: unknown,
): value is Record<string, { jobId: string }> {
  if (value === null || typeof value !== "object") return false;
  return Object.values(value).every((entry) => {
    if (entry === null || typeof entry !== "object") return false;
    return isNonEmptyString((entry as Record<string, unknown>).jobId);
  });
}

function isRecordOfCoalesced(
  value: unknown,
): value is Record<
  string,
  { existingJobId: string; reason: "queue-full" | "active-limit" }
> {
  if (value === null || typeof value !== "object") return false;
  return Object.values(value).every((entry) => {
    if (entry === null || typeof entry !== "object") return false;
    const e = entry as Record<string, unknown>;
    return isNonEmptyString(e.existingJobId) &&
      (e.reason === "queue-full" || e.reason === "active-limit");
  });
}

function isRecordOfReplaced(
  value: unknown,
): value is Record<string, ReplacedQueuedJob> {
  if (value === null || typeof value !== "object") return false;
  return Object.values(value).every((entry) => {
    if (entry === null || typeof entry !== "object") return false;
    const r = entry as Partial<ReplacedQueuedJob>;
    return isNonEmptyString(r.service) &&
      isNonEmptyString(r.jobType) &&
      isNonEmptyString(r.id) &&
      isValidIsoTimestamp(r.createdAt) &&
      isNonEmptyString(r.requestId) &&
      isJobContext(r.context);
  });
}

function isJobKeyQueued(value: unknown): value is JobKeyQueued {
  if (value === null || typeof value !== "object") return false;
  const entry = value as Partial<JobKeyQueued>;
  return isNonEmptyString(entry.jobId) &&
    isValidIsoTimestamp(entry.createdAt) &&
    isNonEmptyString(entry.requestId) && isJobContext(entry.context) &&
    (entry.submissionId === undefined || isNonEmptyString(entry.submissionId));
}

function isJobKeyActiveSlot(value: unknown): value is JobKeyActiveSlot {
  if (value === null || typeof value !== "object") return false;
  const slot = value as Partial<JobKeyActiveSlot>;
  return isNonEmptyString(slot.jobId) && isNonEmptyString(slot.slotToken) &&
    isNonEmptyString(slot.instanceId) && isValidIsoTimestamp(slot.startedAt) &&
    isValidIsoTimestamp(slot.heartbeatAt) &&
    isValidIsoTimestamp(slot.leaseExpiresAt) &&
    isNonNegativeInteger(slot.tries) && isJobContext(slot.context);
}

function isJobContext(value: unknown): value is JobContext {
  if (value === null || typeof value !== "object") return false;
  const context = value as Partial<JobContext>;
  return isNonEmptyString(context.requestId) &&
    isNonEmptyString(context.traceId) &&
    isNonEmptyString(context.traceparent) &&
    (context.tracestate === undefined ||
      typeof context.tracestate === "string");
}

function isNonEmptyString(value: unknown): value is string {
  return typeof value === "string" && value.length > 0;
}

function isPositiveInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isInteger(value) && value > 0;
}

function isNonNegativeInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isInteger(value) && value >= 0;
}

function isValidIsoTimestamp(value: unknown): value is string {
  return typeof value === "string" && Number.isFinite(Date.parse(value));
}

function outcomeState(outcome: unknown): JobKeyState | undefined {
  if (outcome === null || typeof outcome !== "object") return undefined;
  const candidate = outcome as { state?: unknown };
  return isJobKeyState(candidate.state) ? candidate.state : undefined;
}

function isCasConflict(error: unknown): boolean {
  if (!(error instanceof Error)) return false;
  const message = error.message.toLowerCase();
  return message.includes("wrong last sequence") ||
    message.includes("wrong last_seq") ||
    message.includes("sequence mismatch") ||
    message.includes("entry exists") ||
    message.includes("already exists");
}
