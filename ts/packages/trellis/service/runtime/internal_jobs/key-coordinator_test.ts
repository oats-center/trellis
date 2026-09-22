import { assertEquals, assertRejects } from "@std/assert";
import {
  deriveJobKey,
  isJobKeyState,
  normalizeJobKeyPolicy,
  reduceAcquireActiveSlot,
  reduceAdmission,
  reduceReleaseActiveSlot,
  reduceRemoveQueuedJob,
  reduceRenewHeartbeat,
  reduceRestoreReplacedQueuedJob,
} from "./key-coordinator.ts";
import type { JobContext } from "./types.ts";

const context: JobContext = {
  requestId: "request-1",
  traceId: "0123456789abcdef0123456789abcdef",
  traceparent: "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01",
};

Deno.test("deriveJobKey builds display key and stable hash from template", async () => {
  const first = await deriveJobKey({
    service: "svc",
    jobType: "sync",
    payload: { origin: "zendesk", ticket: { id: 42 }, urgent: true },
    template: ["tenant", "/origin", "/ticket/id", "/urgent"],
  });
  const second = await deriveJobKey({
    service: "svc",
    jobType: "sync",
    payload: { origin: "zendesk", ticket: { id: 42 }, urgent: true },
    template: ["tenant", "/origin", "/ticket/id", "/urgent"],
  });

  assertEquals(first.key, "tenant:zendesk:42:true");
  assertEquals(first.keyHash, second.keyHash);
  assertEquals(first.keyHash.length, 64);
  assertEquals(first.kvKey, `svc.sync.${first.keyHash}`);
});

Deno.test("deriveJobKey hashes typed structured segments without display-key collisions", async () => {
  const first = await deriveJobKey({
    service: "svc",
    jobType: "sync",
    payload: { value: "b:c" },
    template: ["a", "/value"],
  });
  const second = await deriveJobKey({
    service: "svc",
    jobType: "sync",
    payload: { value: "c" },
    template: ["a:b", "/value"],
  });

  assertEquals(first.key, "a:b:c");
  assertEquals(second.key, "a:b:c");
  assertEquals(first.keyHash === second.keyHash, false);
});

Deno.test("deriveJobKey rejects non-scalar pointer values", async () => {
  await assertRejects(
    () =>
      deriveJobKey({
        service: "svc",
        jobType: "sync",
        payload: { nested: { id: "bad" } },
        template: ["tenant", "/nested"],
      }),
    Error,
    "did not resolve to a string, finite number, or boolean",
  );
});

Deno.test("deriveJobKey rejects non-finite numeric pointer values", async () => {
  for (const value of [NaN, Infinity, -Infinity]) {
    await assertRejects(
      () =>
        deriveJobKey({
          service: "svc",
          jobType: "sync",
          payload: { value },
          template: ["tenant", "/value"],
        }),
      Error,
      "did not resolve to a string, finite number, or boolean",
    );
  }
});

Deno.test("reduceAdmission applies default keyed queue reject policy", async () => {
  const policy = normalizeJobKeyPolicy({
    keyConcurrency: { key: ["/tenant"] },
  });
  const derived = await deriveJobKey({
    service: "svc",
    jobType: "sync",
    payload: { tenant: "a" },
    template: policy.key,
  });
  const accepted = reduceAdmission({
    state: undefined,
    derived,
    request: {
      service: "svc",
      jobType: "sync",
      jobId: "job-1",
      context,
      createdAt: "2024-01-01T00:00:00.000Z",
      strictCreate: true,
    },
    policy,
  });
  assertEquals(accepted.kind, "accepted");
  if (accepted.kind !== "accepted") return;

  const rejected = reduceAdmission({
    state: accepted.state,
    derived,
    request: {
      service: "svc",
      jobType: "sync",
      jobId: "job-2",
      context,
      createdAt: "2024-01-01T00:00:01.000Z",
      strictCreate: true,
    },
    policy,
  });
  assertEquals(rejected, {
    kind: "rejected",
    key: "a",
    reason: "queue-depth",
    active: 0,
    queued: 1,
    limit: 1,
  });
});

Deno.test("reduceAdmission supports coalesce and replace-oldest", async () => {
  const derived = await deriveJobKey({
    service: "svc",
    jobType: "sync",
    payload: { tenant: "a" },
    template: ["/tenant"],
  });
  const basePolicy = normalizeJobKeyPolicy({
    keyConcurrency: { key: ["/tenant"] },
  });
  const accepted = reduceAdmission({
    state: undefined,
    derived,
    request: {
      service: "svc",
      jobType: "sync",
      jobId: "job-1",
      context,
      createdAt: "2024-01-01T00:00:00.000Z",
      strictCreate: true,
    },
    policy: basePolicy,
  });
  if (accepted.kind !== "accepted") return;

  const coalesced = reduceAdmission({
    state: accepted.state,
    derived,
    request: {
      service: "svc",
      jobType: "sync",
      jobId: "job-2",
      context,
      createdAt: "2024-01-01T00:00:01.000Z",
      strictCreate: false,
    },
    policy: normalizeJobKeyPolicy({
      keyConcurrency: { key: ["/tenant"] },
      queue: { whenFull: "coalesce" },
    }),
  });
  assertEquals(coalesced.kind, "coalesced");
  if (coalesced.kind === "coalesced") {
    assertEquals(coalesced.existing.id, "job-1");
  }

  const replaced = reduceAdmission({
    state: accepted.state,
    derived,
    request: {
      service: "svc",
      jobType: "sync",
      jobId: "job-3",
      context,
      createdAt: "2024-01-01T00:00:02.000Z",
      strictCreate: false,
    },
    policy: normalizeJobKeyPolicy({
      keyConcurrency: { key: ["/tenant"] },
      queue: { whenFull: "replace-oldest" },
    }),
  });
  assertEquals(replaced.kind, "replaced");
  if (replaced.kind === "replaced") {
    assertEquals(replaced.replaced.id, "job-1");
    assertEquals(replaced.state.queued.map((entry) => entry.jobId), ["job-3"]);
  }
});

Deno.test("reduceAdmission keeps create strict for coalesce and replace-oldest queues", async () => {
  const derived = await deriveJobKey({
    service: "svc",
    jobType: "sync",
    payload: { tenant: "a" },
    template: ["/tenant"],
  });
  const accepted = reduceAdmission({
    state: undefined,
    derived,
    request: {
      service: "svc",
      jobType: "sync",
      jobId: "job-1",
      context,
      createdAt: "2024-01-01T00:00:00.000Z",
      strictCreate: true,
    },
    policy: normalizeJobKeyPolicy({
      keyConcurrency: { key: ["/tenant"] },
    }),
  });
  if (accepted.kind !== "accepted") return;

  for (const whenFull of ["coalesce", "replace-oldest"] as const) {
    const rejected = reduceAdmission({
      state: accepted.state,
      derived,
      request: {
        service: "svc",
        jobType: "sync",
        jobId: `job-${whenFull}`,
        context,
        createdAt: "2024-01-01T00:00:01.000Z",
        strictCreate: true,
      },
      policy: normalizeJobKeyPolicy({
        keyConcurrency: { key: ["/tenant"] },
        queue: { whenFull },
      }),
    });
    assertEquals(rejected.kind, "rejected");
  }
});

Deno.test("reduceAdmission remembers an accepted submission after its queue entry is removed", async () => {
  const derived = await deriveJobKey({
    service: "svc",
    jobType: "sync",
    payload: { tenant: "a" },
    template: ["/tenant"],
  });
  const policy = normalizeJobKeyPolicy({
    keyConcurrency: { key: ["/tenant"] },
  });
  const request = {
    service: "svc",
    jobType: "sync",
    jobId: "job-1",
    context,
    createdAt: "2024-01-01T00:00:00.000Z",
    strictCreate: true,
    submissionId: "submission-1",
  };
  const accepted = reduceAdmission({
    state: undefined,
    derived,
    request,
    policy,
  });
  if (accepted.kind !== "accepted") return;
  const removed = reduceRemoveQueuedJob({
    state: accepted.state,
    jobId: request.jobId,
    now: "2024-01-01T00:00:01.000Z",
  });
  if (removed.kind !== "removed") return;

  const replayed = reduceAdmission({
    state: removed.state,
    derived,
    request,
    policy,
  });
  assertEquals(replayed.kind, "accepted");
  if (replayed.kind === "accepted") {
    assertEquals(replayed.state.queued, []);
    assertEquals(replayed.state.acceptedBySubmissionId, {
      "submission-1": { jobId: "job-1" },
    });
  }
});

Deno.test("active slot reducers acquire renew release and detect stale completion", async () => {
  const policy = normalizeJobKeyPolicy({
    keyConcurrency: { key: ["/tenant"] },
  });
  const derived = await deriveJobKey({
    service: "svc",
    jobType: "sync",
    payload: { tenant: "a" },
    template: policy.key,
  });
  const admitted = reduceAdmission({
    state: undefined,
    derived,
    request: {
      service: "svc",
      jobType: "sync",
      jobId: "job-1",
      context,
      createdAt: "2024-01-01T00:00:00.000Z",
      strictCreate: true,
    },
    policy,
  });
  if (admitted.kind !== "accepted") return;
  const acquired = reduceAcquireActiveSlot({
    state: admitted.state,
    derived,
    request: {
      service: "svc",
      jobType: "sync",
      jobId: "job-1",
      context,
      tries: 1,
      instanceId: "worker-1",
      now: "2024-01-01T00:00:01.000Z",
    },
    policy,
    slotToken: "slot-1",
  });
  assertEquals(acquired.kind, "acquired");
  if (acquired.kind !== "acquired") return;
  assertEquals(acquired.state.active.length, 1);
  assertEquals(acquired.state.queued.length, 0);

  const renewed = reduceRenewHeartbeat({
    state: acquired.state,
    jobId: "job-1",
    slotToken: "slot-1",
    now: "2024-01-01T00:00:02.000Z",
    policy,
  });
  assertEquals(renewed.kind, "renewed");
  if (renewed.kind !== "renewed") return;

  assertEquals(
    reduceReleaseActiveSlot({
      state: renewed.state,
      jobId: "job-1",
      slotToken: "missing",
      now: "2024-01-01T00:00:03.000Z",
    }).kind,
    "staleCompletion",
  );
  assertEquals(
    reduceReleaseActiveSlot({
      state: renewed.state,
      jobId: "job-1",
      slotToken: "slot-1",
      now: "2024-01-01T00:00:03.000Z",
    }).kind,
    "released",
  );
});

Deno.test("reduceAcquireActiveSlot blocks replaced queued work that is no longer queued", async () => {
  const policy = normalizeJobKeyPolicy({
    keyConcurrency: { key: ["/tenant"] },
    queue: { whenFull: "replace-oldest" },
  });
  const derived = await deriveJobKey({
    service: "svc",
    jobType: "sync",
    payload: { tenant: "a" },
    template: policy.key,
  });
  const admitted = reduceAdmission({
    state: undefined,
    derived,
    request: {
      service: "svc",
      jobType: "sync",
      jobId: "job-old",
      context,
      createdAt: "2024-01-01T00:00:00.000Z",
      strictCreate: false,
    },
    policy,
  });
  if (admitted.kind !== "accepted") return;
  const replaced = reduceAdmission({
    state: admitted.state,
    derived,
    request: {
      service: "svc",
      jobType: "sync",
      jobId: "job-new",
      context,
      createdAt: "2024-01-01T00:00:01.000Z",
      strictCreate: false,
    },
    policy,
  });
  if (replaced.kind !== "replaced") return;

  const acquired = reduceAcquireActiveSlot({
    state: replaced.state,
    derived,
    request: {
      service: "svc",
      jobType: "sync",
      jobId: "job-old",
      context,
      lifecycleState: "pending",
      tries: 1,
      instanceId: "worker-1",
      now: "2024-01-01T00:00:02.000Z",
    },
    policy,
    slotToken: "slot-1",
  });

  assertEquals(acquired.kind, "blocked");
  if (acquired.kind === "blocked") {
    assertEquals(acquired.reason, "not-queued");
  }
});

Deno.test("reduceAcquireActiveSlot allows manual retried work without queued reservation", async () => {
  const policy = normalizeJobKeyPolicy({
    keyConcurrency: { key: ["/tenant"] },
  });
  const derived = await deriveJobKey({
    service: "svc",
    jobType: "sync",
    payload: { tenant: "a" },
    template: policy.key,
  });

  const acquired = reduceAcquireActiveSlot({
    state: undefined,
    derived,
    request: {
      service: "svc",
      jobType: "sync",
      jobId: "job-retried",
      context,
      workEventType: "retried",
      lifecycleState: "pending",
      tries: 1,
      instanceId: "worker-1",
      now: "2024-01-01T00:00:02.000Z",
    },
    policy,
    slotToken: "slot-1",
  });

  assertEquals(acquired.kind, "acquired");
  if (acquired.kind === "acquired") {
    assertEquals(acquired.state.active[0]?.jobId, "job-retried");
  }
});

Deno.test("reduceAcquireActiveSlot allows active redelivery without queued reservation", async () => {
  const policy = normalizeJobKeyPolicy({
    keyConcurrency: { key: ["/tenant"] },
  });
  const derived = await deriveJobKey({
    service: "svc",
    jobType: "sync",
    payload: { tenant: "a" },
    template: policy.key,
  });

  const acquired = reduceAcquireActiveSlot({
    state: undefined,
    derived,
    request: {
      service: "svc",
      jobType: "sync",
      jobId: "job-active",
      context,
      workEventType: "created",
      lifecycleState: "active",
      tries: 1,
      instanceId: "worker-1",
      now: "2024-01-01T00:00:02.000Z",
    },
    policy,
    slotToken: "slot-1",
  });

  assertEquals(acquired.kind, "acquired");
  if (acquired.kind === "acquired") {
    assertEquals(acquired.state.active[0]?.jobId, "job-active");
  }
});

Deno.test("reduceRemoveQueuedJob releases pending keyed reservation", async () => {
  const policy = normalizeJobKeyPolicy({
    keyConcurrency: { key: ["/tenant"] },
  });
  const derived = await deriveJobKey({
    service: "svc",
    jobType: "sync",
    payload: { tenant: "a" },
    template: policy.key,
  });
  const admitted = reduceAdmission({
    state: undefined,
    derived,
    request: {
      service: "svc",
      jobType: "sync",
      jobId: "job-1",
      context,
      createdAt: "2024-01-01T00:00:00.000Z",
      strictCreate: true,
      submissionId: "submission-1",
    },
    policy,
  });
  if (admitted.kind !== "accepted") return;

  const removed = reduceRemoveQueuedJob({
    state: admitted.state,
    jobId: "job-1",
    now: "2024-01-01T00:00:01.000Z",
    submissionId: "submission-1",
  });

  assertEquals(removed.kind, "removed");
  if (removed.kind === "removed") {
    assertEquals(removed.state.queued, []);
    assertEquals(removed.state.acceptedBySubmissionId, undefined);
  }
});

Deno.test("reduceRestoreReplacedQueuedJob restores old queued work and removes replacement", async () => {
  const policy = normalizeJobKeyPolicy({
    keyConcurrency: { key: ["/tenant"] },
    queue: { whenFull: "replace-oldest" },
  });
  const derived = await deriveJobKey({
    service: "svc",
    jobType: "sync",
    payload: { tenant: "a" },
    template: policy.key,
  });
  const admitted = reduceAdmission({
    state: undefined,
    derived,
    request: {
      service: "svc",
      jobType: "sync",
      jobId: "job-old",
      context,
      createdAt: "2024-01-01T00:00:00.000Z",
      strictCreate: false,
    },
    policy,
  });
  if (admitted.kind !== "accepted") return;
  const replaced = reduceAdmission({
    state: admitted.state,
    derived,
    request: {
      service: "svc",
      jobType: "sync",
      jobId: "job-new",
      context,
      createdAt: "2024-01-01T00:00:01.000Z",
      strictCreate: false,
    },
    policy,
  });
  if (replaced.kind !== "replaced") return;

  const restored = reduceRestoreReplacedQueuedJob({
    state: replaced.state,
    derived,
    replacementJobId: "job-new",
    replaced: replaced.replaced,
    now: "2024-01-01T00:00:02.000Z",
    policy,
  });

  assertEquals(restored.state.queued.map((entry) => entry.jobId), ["job-old"]);
});

Deno.test("isJobKeyState rejects malformed capacity-blocking active and queued entries", () => {
  const validState = {
    version: 1,
    service: "svc",
    jobType: "sync",
    key: "tenant-a",
    keyHash: "hash",
    maxActive: 1,
    maxQueuedPerKey: 1,
    active: [{
      jobId: "job-active",
      slotToken: "slot-1",
      instanceId: "worker-1",
      startedAt: "2024-01-01T00:00:00.000Z",
      heartbeatAt: "2024-01-01T00:00:01.000Z",
      leaseExpiresAt: "2024-01-01T00:02:00.000Z",
      tries: 1,
      context,
    }],
    queued: [{
      jobId: "job-queued",
      createdAt: "2024-01-01T00:00:00.000Z",
      requestId: context.requestId,
      context,
    }],
    staleTakeoverCount: 0,
    updatedAt: "2024-01-01T00:00:01.000Z",
  };

  assertEquals(isJobKeyState(validState), true);
  assertEquals(
    isJobKeyState({
      ...validState,
      active: [{ ...validState.active[0], slotToken: "" }],
    }),
    false,
  );
  assertEquals(
    isJobKeyState({
      ...validState,
      queued: [{ ...validState.queued[0], createdAt: "not-a-date" }],
    }),
    false,
  );
});
