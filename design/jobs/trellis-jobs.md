---
title: Trellis Job Management
description: Service-private durable job execution and operator lifecycle.
---

# Trellis Job Management

Jobs are service-private queued execution. Operations are the caller-visible
workflow abstraction; Jobs are not exposed as a public application API and are
not replaced by the Operation repository.

A `job` resource declares title, description, payload, optional result/update,
deadline, retry, and optional typed keyed-concurrency policy. Progress,
structured logs, retries, and dead-job lifecycle are always enabled. Authored
feature switches, heartbeat, ack-wait, max-delivery, and transport knobs do not
exist.

The default retry policy is five ordinary attempts with delays of 5s, 30s, 2m,
and 10m. Explicit retry has positive attempts and exactly attempts-minus-one
positive delays. Ordinary retryable handler failure uses delayed NAK and
consumes that execution budget. Broker delivery is unlimited: after the ordinary
budget is exhausted, the execution owner re-enters the handler with cancellation
already requested to reconcile earlier side effects. A creation-relative
deadline uses the same cleanup path for previously started work. Cleanup errors
leave work redeliverable; only successful cleanup permits terminal Dead or
Expired settlement and source acknowledgement. Never-started expired work
recovers a fresh fence and confirms Expired publication before releasing its
slot, without running the handler. Failed publication retains the reservation
for redelivery; confirmed terminal redelivery reconciles any remaining slot
before acknowledgement. The janitor does not settle deadlines or execute
application cleanup.

Progress ACK maintenance is automatic for accepted keyed and unkeyed deliveries
at one third of the effective broker acknowledgement wait. It covers preflight,
handler execution, confirmed lifecycle publication, and source disposition on
the receiving transport. Keyed lease renewal is a separate fence; failure
cancels local work and prevents stale completion. Job execution and side effects
are at least once.

A keyed delivery that cannot acquire its active slot remains blocked before the
handler starts. The worker maintains the broker delivery lease with progress
ACKs, so capacity waiting neither consumes another delivery attempt nor causes
ordinary-attempt exhaustion. Handler work and keyed lease renewal begin only
after the slot is acquired. Ordinary provisioned queues use the `block` stale
policy; stale-policy selection is not an authored option.

At the key-coordinator boundary, `fail-stale` takeover fences the displaced
attempt and atomically preserves its cleanup obligation in the key record. The
new job does not publish terminal Stale for the displaced job. On redelivery,
that job waits for capacity, acquires a fresh fence, and enters its handler with
`stale-attempt` cancellation already requested. Cleanup failure preserves the
obligation for another delivery. A never-started displaced reservation needs no
handler cleanup. Only its recovered owner can publish Stale after
reconciliation; confirmed terminal settlement then removes the obligation.
Release and renewal by an old token cannot mutate the replacement owner's slot.
Stale-completion diagnostics are nonterminal and do not authorize
acknowledgement; the displaced work redelivers until its cleanup obligation is
settled. A recovered never-started reservation may transition directly from
Pending to Stale with zero attempts, without manufacturing handler execution.

Workers fold accepted durable lifecycle transitions in broker stream-sequence
order to resolve execution state and attempts. `Created` initializes the
original run; republishing it cannot reopen or reset an existing job.
Diagnostics, heartbeats, logs, and progress cannot supersede settlement or reset
attempts. A valid `Retried` opens a new run under the same job ID without
deleting history. Work deliveries retain their original lifecycle sequence
through the broker's source metadata. Deliveries predating the accepted replay
are acknowledged as old work without removing reservations or cleanup
obligations belonging to the newer run. Deferred work rereads lifecycle state
before trying acquisition again.

Jobs retains stream-first durable lifecycle, stable IDs, queue/key coordination,
projection, janitor, cancellation, progress, logs, result/error, dead jobs,
replay/dismiss, and optional live typed updates. Live updates are transient; the
durable lifecycle remains authoritative. Confirmed JetStream publication is
required before acknowledging source work. Both Rust and TypeScript lifecycle
publishers await the broker acknowledgement; typed live updates use core NATS
without requiring a durable stream.

Key paths compile to typed scalar accessors. Optional or nonscalar leaves are
compile errors. Existing collision-safe key encoding and reject/coalesce/
replace-oldest queue behavior remain.

Operator Query returns finite cursor pages. Summary applies the same filters to
the full matching set and returns page-independent count, state statistics, and
optional groups; Metrics remains windowed time series. Cursor pagination
rechecks authority on each page and does not generally promise a snapshot.
