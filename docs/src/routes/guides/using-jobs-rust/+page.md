---
title: "Jobs: Rust"
description: "Add background job processing to a Rust service with retry, progress tracking, and dead-letter handling."
---

# Use jobs from Rust

Jobs are service-private durable execution. Declare a `job` resource with
payload, optional result/update/deadline/retry, and optional keyed-concurrency
policy. Progress, structured logs, and dead-job lifecycle are always available;
there are no feature switches or authored ack-wait/max-delivery knobs.

Native IDL uses the same job declaration for Rust generation:

```trellis
job reconcile {
  title "Reconcile order";
  description "Repairs one order projection.";
  payload ReconcileInput;
  result ReconcileOutput;
  retry { attempts 3; backoff [5s, 30s]; }
  key_concurrency { path orderId; policy queue; }
}
```

Use generated handles to register workers and submit work. The default policy is
five ordinary attempts with delays of 5s, 30s, 2m, and 10m. Explicit retry uses
attempts plus exactly attempts-minus-one positive delays.

Generated provider methods accept an `ActiveJob` handler whose future returns
`Result<JobResult, JobProcessError<E>>`:

```rust
use trellis_rs::jobs::JobProcessError;

provider
    .register_reconcile(|job| async move {
        reconcile(job.payload())
            .await
            .map_err(JobProcessError::retryable)
    })
    .await?;

let job = provider.create_reconcile(input).await?;
```

Use `JobProcessError::failed(error)` for a non-retryable handler failure.

`create_reconcile` returns the job's `JobRef`, and fails with
`JobsError::NotEnqueued` when the key policy refuses the job.
`submit_reconcile` returns the policy's decision instead: a `JobSubmitOutcome`
of `Accepted`, `Rejected`, `Coalesced` (an existing job already covers the key)
or `Replaced`.

## Submit a job from a database transaction

To submit a job only if your own database transaction commits, store it in an
outbox inside that transaction and dispatch it afterwards. `prepare_<job>`
fixes the job's ID, payload and creation time without submitting anything:

```rust
use trellis_rs::client::{dispatch_outbox_once, OutboxStore, SqliteOutboxStore};

let jobs = provider.jobs_client();
let tx = connection.transaction()?;
save_order(&tx, &order)?;
let job = jobs.prepare_reconcile(ReconcileInput { order_id: order.id.clone() })?;
SqliteOutboxStore::new(&tx)
    .enqueue(&format!("reconcile-{}", order.id), &job)
    .await?;
tx.commit()?;

// A dispatcher loop, in any task:
let dispatcher = jobs.outbox_dispatcher();
let mut outbox = SqliteOutboxStore::new(&connection);
dispatch_outbox_once(&mut outbox, &dispatcher).await?;
```

A dispatched job reports `JobDispatched` with the queue's `OutboxJobOutcome`.
A key policy's refusal (`Rejected` or `Coalesced`) is final, and the prepared
job is recorded as skipped so anything watching its ID sees it end. Errors
leave the record pending for another attempt. Dispatching the same job again
after a crash does not create a second job. The same outbox also carries
prepared events. Its SQL table has a `kind` column; add
`kind TEXT NOT NULL DEFAULT 'event'` to an existing `trellis_outbox_events`
table.

## Progress and live updates

A handler reports durable progress with `job.progress(JobProgress { .. })` and
logs with `job.log(..)`. Both become part of the job's state. Wrap a wait on
something outside the job in `job.wait_for(edge, future)` so the job's state
shows what it is waiting for.

A queue that declares `update ReconcileUpdate;` also streams live-only updates.
The handler publishes them with `job.emit_update(update)`, and anyone in the
service subscribes with `job.updates()` on a `JobRef`, or by ID with
`provider.reconcile_updates(&job_id)`:

```rust
let mut updates = job.updates().await?;
while let Some(update) = updates.next().await {
    let update = update?;
    println!("attempt {} step {}: {:?}", update.attempt, update.sequence, update.update);
}
```

Each update is checked against the declared schema. The stream yields updates in
order and ends when the job reaches a terminal state. Updates are not stored, so
a subscriber only receives the ones published after it subscribed. A queue
without a declared update has no update type, and calling `emit_update` on it
does not compile.

Handlers and spawned tasks create and observe jobs through
`provider.jobs_client()`, a cloneable `Jobs` handle with the same `create_`,
`submit_` and `_updates` methods as the provider.

Jobs are private to their service, so a browser or another service can't
subscribe to them. To show a job's progress in a web app, start the job from an
operation handler and forward each update from `job.updates()` with the
operation's `emit_update`. The app then watches the operation.

Delivery progress acknowledgement is automatic for keyed and unkeyed work.
Applications do not heartbeat merely to retain delivery. Keyed lease renewal is
separate and fences/cancels work after ownership loss.

For keyed work, the handler starts only after the active key slot is acquired.
Capacity waits maintain broker delivery progress without consuming an attempt or
causing max-delivery exhaustion. `JobProcessError::retryable(error)` from
handler execution consumes the configured attempts and backoff schedule.

Broker redelivery is unlimited so previously started work can finish cleanup
after its creation-relative deadline or ordinary retry budget is exhausted.
Before business work, inspect `job.cancellation_token().reason()`: an
already-cancelled invocation must reconcile earlier side effects and return
success only when cleanup is complete. Cleanup errors keep the job redeliverable
rather than terminalizing it. Never-started expired jobs confirm terminal
publication before releasing their recovered key slot, without running the
handler. Publication failure preserves the reservation for redelivery; terminal
redelivery reconciles any remaining slot.

Native lifecycle publication awaits JetStream acknowledgement. Typed live
updates remain transient and do not require a durable stream.

Ordinary queues use the Block stale policy; stale-policy selection is not an
authored option. At the coordinator boundary, fail-stale takeover only fences
the old attempt. Its recovered owner must obtain a fresh slot and reconcile with
`JobCancellationReason::StaleAttempt` before publishing terminal Stale. The new
owner's fence is unaffected by the old token. An ignored stale completion is a
nonterminal diagnostic, not final acknowledgement. The work redelivers until
cleanup settles. A never-started displaced reservation settles directly from
Pending to Stale without running its handler.

Jobs are at least once. Keep side effects idempotent. Typed updates are
live-only; durable progress, logs, result/error, and dead lifecycle remain
authoritative.
