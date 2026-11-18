---
title: "Administer Jobs"
description: "Query, cancel, replay, and monitor jobs across all services using the built-in Jobs API."
---

This guide covers querying, cancelling, replaying, and monitoring jobs across all services using the built-in `trellis.jobs@v1` API. The examples use the `orders-service` fulfillment jobs from [Jobs: TypeScript](/guides/using-jobs-ts).

> **Note for Service Authors:** Jobs are strict service-internal execution machinery. They run in the background and are completely invisible to callers. For caller-visible asynchronous workflows, you should implement **Operations** instead.

This guide is for operators. If you are a service author, see [Jobs: TypeScript](/guides/using-jobs-ts) or [Jobs: Rust](/guides/using-jobs-rust).

## What the Jobs admin runtime implements

`trellis.jobs@v1` is a standard built-in Trellis API. The optional Jobs admin
runtime is Trellis infrastructure that implements its admin behavior:

- **SQL projection** — a disposable query view of current job state derived from
  the `JOBS` JetStream source of truth
- **Global RPCs** — `Jobs.Query`, `Jobs.Summary`, `Jobs.Inspect`, `Jobs.GetKey`,
  `Jobs.History`, `Jobs.Metrics`, `Jobs.Cancel`, `Jobs.Retry`,
  `Jobs.ReplayDLQ`, `Jobs.DismissDLQ`, `Jobs.ListDLQ`, `Jobs.ListServices`
- **Workbench live** — `Jobs.Watch` invalidation frames for live query and
  inspection views
- **Retention** — an hourly pass that expires jobs no worker claimed and
  removes old settled jobs from the projection

The Jobs admin runtime is stateless and horizontally scalable. It does not
participate in job correctness — it is a derived observability layer. The
JetStream stream remains the source of truth.

## Querying jobs

Use `Jobs.Query` to query jobs. Any service or app can call the Jobs API once
its IDL selects the capabilities it needs in a `use jobs { ... }` block, for
example `capability jobs.query;`. The examples call the generated TypeScript
client's flat methods. A method's name is the action's name in camelCase
(`client.cancel` for `Jobs.Cancel`), prefixed with the API's alias only when
another selected API has an action of the same name: a caller that also uses
`trellis.health@v1` gets `client.jobsQuery`, `client.jobsInspect`, and
`client.jobsSummary`. Check the generated participant for the names yours
gets. Each method returns a `Result`; the examples unwrap it with `orThrow()`.

**List all pending fulfillment jobs for the orders service:**

```ts
const pending = await client
  .jobsQuery({
    service: "orders-service",
    type: "reserve-inventory",
    state: ["pending"],
    sort: { field: "updatedAt", direction: "desc" },
    page: { limit: 50 },
  })
  .orThrow();

for (const job of pending.items) {
  console.log(job.id, job.state);
}
```

`Jobs.Query` performs search, sort, grouping, and stats on the server for admin
workbenches. Full-text search covers safe fields only: ids, service/type/state,
request and trace context, queue key/policy text, progress/log text, and error
text. Payload and result bodies are not searched by default.

Use `Jobs.Summary` with the same filters when the UI needs aggregate data. It
computes `count`, state totals/statistics, and optional groups across the full
filtered result set, independent of the current `Jobs.Query` page or cursor.

**Inspect a specific job:**

```ts
const inspection = await client.jobsInspect({ id: "job_abc123" }).orThrow();

console.log(inspection.job.context.requestId, inspection.timeline.length);
```

Projected job records include `context.requestId`, `context.traceId`,
`context.traceparent`, and optional `context.tracestate` so operators can pivot
from a job to related RPC, event, operation, and service logs.

`Jobs.Inspect` returns the current job, lifecycle timeline, attempts, related
jobs, structured or string-derived errors, trigger, lineage, and current wait
edges. Error details include a stable fingerprint and aggregate occurrence count
when the same failure repeats.

Active jobs that are suspended on child work remain in the `active` state and add
`waitingOn` entries. Runtime-owned waits on Trellis jobs and operations are
recorded automatically. Service authors can use `job.waitFor(...)` /
`job.wait_for(...)` to describe long external waits such as vendor APIs,
databases, object stores, or polling loops.

## Inspecting keyed concurrency

Jobs that declare keyed concurrency expose their derived key in admin
projection. Use `Jobs.GetKey` to inspect why work is waiting or rejected for a
logical key:

```ts
const key = await client
  .getKey({
    service: "zendesk-sync",
    type: "sync-tickets",
    key: "zendesk:foo:tickets",
  })
  .orThrow();

console.log(
  key.active[0]?.heartbeatAgeMs,
  key.queuedDepth,
  key.latestPolicyReason,
);
```

Key status includes the active job identity, queued depth, heartbeat age, lease
expiry, stale takeover count, and the latest queue-policy reason such as
`active-limit`, `queue-depth`, `coalesced`, or `replaced`. A stale active job is
reported explicitly; it is not inferred from worker-presence heartbeats.

## Cancelling a job

`Jobs.Cancel` is valid for jobs in `pending`, `active`, or `retry` states.

```ts
await client
  .cancel({ id: "job_abc123", reason: "replacement run queued" })
  .orThrow();
```

**Important:** Cancellation is cooperative and eventual for `active` jobs. The
service worker continues running until it checks `job.cancelled` and responds.
Expect a short delay before the state transitions to `cancelled`. Jobs in
`pending` or `retry` state transition immediately. There is no hard kill or stop
API.

The optional `reason` on `Jobs.Cancel`, `Jobs.Retry`, `Jobs.ReplayDLQ`, and
`Jobs.DismissDLQ` is recorded in lifecycle event admin metadata and appears in
the inspection timeline.

## Failed vs dead: two different operations

The jobs system distinguishes `failed` from `dead`. They require different admin
actions.

### `failed` jobs

A job enters the `failed` state when its handler reports a non-retryable failure.
Use `Jobs.Retry` to re-enqueue it:

```ts
await client
  .retry({ id: "job_abc123", reason: "fixed provider credentials" })
  .orThrow();
```

This transitions the job back to `pending` for re-processing.

### `dead` jobs

A job enters the `dead` state when retryable handler failures exhaust its
declared retry attempts. Capacity blocking for a keyed queue does not spend
attempts or cause dead-lettering. Example: a `charge-payment` handler ran and
reported a retryable provider outage 5 consecutive times.

The `dead` state is **not terminal** — dead jobs can be either replayed or
dismissed.

**Replay** (when the underlying issue is fixed):

```ts
await client
  .replayDlq({ id: "job_abc123", reason: "provider recovered" })
  .orThrow();
```

This re-enqueues the job at `pending`.

**Dismiss** (when the work is no longer needed):

```ts
await client
  .dismissDlq({ id: "job_abc123", reason: "order was manually closed" })
  .orThrow();
```

This transitions the job to `dismissed`, a terminal state. Dismissed jobs are not
re-processed.

## Watching for changes

Use `Jobs.Watch` when a workbench or detail view needs live updates without
polling every row. The live emits `ready`, `jobChanged`,
`jobInspectChanged`, and `queryInvalidated` frames. Treat those frames as
invalidations: rerun `Jobs.Query` for a workbench view or `Jobs.Inspect` for
a detail view. The live does not provide a precise row-delta protocol.

## Worker presence

Services emit heartbeat messages per job type while the worker host is running.
Use these to confirm service/job-type presence, not as proof that every queue
consumer task is healthy or making progress.

The heartbeat data includes the service name, job type, instance identifier, and
a timestamp. Absence of recent heartbeats for a job type means either the service
is not running or the worker is not registered for that job type.

`Jobs.ListServices` lists each service that runs jobs, whether it is healthy,
and the workers it has heard from recently. The Console's Jobs page shows the
same list.

```ts
const services = await client.listServices({}).orThrow();

for (const service of services.items) {
  for (const worker of service.workers) {
    console.log(
      service.name,
      worker.jobType,
      worker.instanceId,
      worker.timestamp,
    );
  }
}
```

## Retention

Once an hour the Jobs admin runtime applies `[jobs] history_retention_days`
(default 30) from the server's `config.toml`:

- **Unclaimed jobs**: a job still `pending` more than the retention after it
  was created or replayed moves to `expired`, with an error saying no worker
  claimed it. The `JOBS` and `JOBS_WORK` streams keep one day past the
  retention, so the expiry lands while the job's work is still queued, and a
  worker that claims it afterwards runs nothing.
- **Settled jobs**: finished jobs older than the retention leave the
  projection. Failed and dead jobs stay however old they are, because a retry
  or replay needs their payload.

A job a worker has started is settled by that worker, not by retention. If a
service will be offline longer than the retention, raise the setting before
its jobs expire. See
[Upkeep](/guides/running-trellis/upkeep) for the other retention settings.
