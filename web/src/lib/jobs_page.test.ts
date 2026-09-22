import { AsyncResult, BaseError, UnexpectedError } from "@qlever-llc/result";
import { deepEqual, rejects } from "node:assert/strict";
import { type apis } from "trellis-web-generated";

import {
  cancelJob,
  dismissDlqJob,
  loadJobDetailData,
  loadJobsQueryPage,
  loadJobsServices,
  loadJobsSummary,
  replayDlqJob,
  retryJob,
} from "./jobs_page.ts";

declare const Deno: {
  test(name: string, fn: () => void | Promise<void>): void;
};

const jobContext = {
  requestId: "req_test",
  traceId: "trace_test",
  traceparent: "00-trace_test-span_test-01",
};

class JobsNotFoundTestError extends BaseError {
  override readonly name = "NotFoundError" as const;

  override toSerializable() {
    return {
      id: this.id,
      type: this.name,
      message: this.message,
      context: this.getContext(),
    };
  }
}

Deno.test("split jobs loaders keep query, services, and summary independent", async () => {
  const serviceCalls: unknown[] = [];
  const queryCalls: unknown[] = [];
  const summaryCalls: unknown[] = [];

  const query = await loadJobsQueryPage({
    queryJobs: (input) => {
      queryCalls.push(input);
      return AsyncResult.ok({ items: [], page: { nextCursor: "next" } });
    },
  }, { groupBy: "type", state: ["pending"], page: { limit: 50 } });

  const services = await loadJobsServices({
    listServices: (input) => {
      serviceCalls.push(input);
      return AsyncResult.ok({
        items: [{
          name: input.page?.cursor ? "reports" : "documents",
          healthy: true,
          workers: [],
        }],
        page: input.page?.cursor ? {} : { nextCursor: "services-2" },
      });
    },
  });

  const summary = await loadJobsSummary({
    summarizeJobs: (input) => {
      summaryCalls.push(input);
      return AsyncResult.ok({
        count: 2n,
        groups: [{
          count: 2n,
          depth: 2n,
          key: "document-process",
          label: "document-process",
        }],
        stats: { byState: { pending: 2n }, queued: 2n, total: 2n },
      });
    },
  }, { groupBy: "type", service: "documents" });

  deepEqual(queryCalls, [{
    groupBy: "type",
    state: ["pending"],
    page: { limit: 50 },
  }]);
  deepEqual(serviceCalls, [
    { page: { limit: 100 } },
    { page: { cursor: "services-2", limit: 100 } },
  ]);
  deepEqual(summaryCalls, [{ groupBy: "type", service: "documents" }]);
  deepEqual(query.available && query.nextCursor, "next");
  deepEqual(
    services.available && services.services.map((service) => service.name),
    ["documents", "reports"],
  );
  deepEqual(summary.available && summary.count, 2n);
});

Deno.test("a split loader failure does not affect the others", async () => {
  const query = await loadJobsQueryPage({
    queryJobs: () => AsyncResult.ok({ items: [], page: {} }),
  }, { page: { limit: 50 } });
  const services = await loadJobsServices({
    listServices: () =>
      AsyncResult.err(
        new UnexpectedError({ cause: new Error("no responders") }),
      ),
  });
  deepEqual(query.available, true);
  deepEqual(services, {
    available: false,
    message: "Jobs admin runtime is not currently reachable.",
  });
});

Deno.test("loadJobsServices rejects a cursor cycle", async () => {
  await rejects(() =>
    loadJobsServices({
      listServices: () =>
        AsyncResult.ok({ items: [], page: { nextCursor: "cycle" } }),
    })
  );
});

for (
  const [name, cause, message] of [
    [
      "uppercase",
      "No responders available for request",
      "Jobs admin runtime is not currently reachable.",
    ],
    [
      "lowercase",
      "no responders: 'rpc.v1.dHJlbGxpcy5qb2JzQHYx.am9icy1ydW50aW1l.ListServices'",
      "Jobs admin runtime is not currently reachable.",
    ],
    [
      "permissions",
      'nats: Permissions Violation for Publish to "rpc.v1.dHJlbGxpcy5qb2JzQHYx.am9icy1ydW50aW1l.ListServices"',
      "Your current session is not approved for Jobs RPCs. Sign out and sign back in to refresh permissions.",
    ],
  ] as const
) {
  Deno.test(`loadJobsSummary normalizes ${name} failures`, async () => {
    const data = await loadJobsSummary({
      summarizeJobs: () =>
        AsyncResult.err(new UnexpectedError({ cause: new Error(cause) })),
    }, {});
    deepEqual(data.available, false);
    deepEqual(data.message, message);
  });
}

Deno.test("loadJobDetailData requests detail by id", async () => {
  const calls: Array<{ method: string; input: unknown }> = [];
  const data = await loadJobDetailData({
    inspect: (input) => {
      calls.push({ method: "Jobs.Inspect", input });
      return AsyncResult.ok<apis.jobs.InspectOutput>({
        attempts: [],
        errors: [],
        job: {
          id: "job-1",
          service: "documents",
          type: "document-process",
          state: "failed",
          payload: new TextEncoder().encode(
            JSON.stringify({ documentId: "doc-1" }),
          ),
          context: jobContext,
          createdAt: "2026-01-01T00:00:00.000Z",
          updatedAt: "2026-01-01T00:01:00.000Z",
          tries: 3n,
          maxTries: 3n,
          lastError: "boom",
        },
        related: [],
        timeline: [],
      });
    },
  }, "job-1");

  deepEqual(calls, [{ method: "Jobs.Inspect", input: { id: "job-1" } }]);
  deepEqual(data.available, true);
  deepEqual(data.inspection?.job.id, "job-1");
});

Deno.test("loadJobDetailData treats declared NotFoundError as an empty detail", async () => {
  const data = await loadJobDetailData({
    inspect: () =>
      AsyncResult.err(
        new JobsNotFoundTestError("Job 'missing' not found"),
      ),
  }, "missing");

  deepEqual(data, { available: true });
});

Deno.test("cancelJob sends id-only action input", async () => {
  const calls: Array<{ method: string; input: unknown }> = [];
  await cancelJob({
    action: (input) => {
      calls.push({ method: "Jobs.Cancel", input });
      return AsyncResult.ok<apis.jobs.CancelOutput>({
        job: {
          id: "job-1",
          service: "documents",
          type: "document-process",
          state: "cancelled",
          payload: new Uint8Array(),
          context: jobContext,
          createdAt: "2026-01-01T00:00:00.000Z",
          updatedAt: "2026-01-01T00:01:00.000Z",
          tries: 0n,
          maxTries: 3n,
        },
      });
    },
  }, "job-1");

  deepEqual(calls, [{ method: "Jobs.Cancel", input: { id: "job-1" } }]);
});

Deno.test("retryJob sends id-only action input", async () => {
  const calls: Array<{ method: string; input: unknown }> = [];
  await retryJob({
    action: (input) => {
      calls.push({ method: "Jobs.Retry", input });
      return AsyncResult.ok<apis.jobs.RetryOutput>({
        job: {
          id: "job-1",
          service: "documents",
          type: "document-process",
          state: "retry",
          payload: new Uint8Array(),
          context: jobContext,
          createdAt: "2026-01-01T00:00:00.000Z",
          updatedAt: "2026-01-01T00:01:00.000Z",
          tries: 3n,
          maxTries: 3n,
        },
      });
    },
  }, "job-1");

  deepEqual(calls, [{ method: "Jobs.Retry", input: { id: "job-1" } }]);
});

Deno.test("replayDlqJob sends id-only action input", async () => {
  const calls: Array<{ method: string; input: unknown }> = [];
  await replayDlqJob({
    action: (input) => {
      calls.push({ method: "Jobs.ReplayDLQ", input });
      return AsyncResult.ok<apis.jobs.ReplayDLQOutput>({
        job: {
          id: "job-1",
          service: "documents",
          type: "document-process",
          state: "retry",
          payload: new Uint8Array(),
          context: jobContext,
          createdAt: "2026-01-01T00:00:00.000Z",
          updatedAt: "2026-01-01T00:01:00.000Z",
          tries: 3n,
          maxTries: 3n,
        },
      });
    },
  }, "job-1");

  deepEqual(calls, [{ method: "Jobs.ReplayDLQ", input: { id: "job-1" } }]);
});

Deno.test("dismissDlqJob sends id-only action input", async () => {
  const calls: Array<{ method: string; input: unknown }> = [];
  await dismissDlqJob({
    action: (input) => {
      calls.push({ method: "Jobs.DismissDLQ", input });
      return AsyncResult.ok<apis.jobs.DismissDLQOutput>({
        job: {
          id: "job-1",
          service: "documents",
          type: "document-process",
          state: "dismissed",
          payload: new Uint8Array(),
          context: jobContext,
          createdAt: "2026-01-01T00:00:00.000Z",
          updatedAt: "2026-01-01T00:01:00.000Z",
          tries: 3n,
          maxTries: 3n,
        },
      });
    },
  }, "job-1");

  deepEqual(calls, [{ method: "Jobs.DismissDLQ", input: { id: "job-1" } }]);
});
