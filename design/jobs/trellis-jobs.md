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

The default retry policy is five total deliveries with delays of 5s, 30s, 2m,
and 10m. Explicit retry has positive attempts and exactly attempts-minus-one
positive delays. Ordinary retryable handler failure uses delayed NAK and
consumes that delivery budget. Final exhaustion is durably recorded by Jobs
Runtime before source acknowledgement.

Progress ACK maintenance is automatic for keyed and unkeyed handlers at one
third of the effective broker acknowledgement wait. Keyed lease renewal is a
separate fence; failure cancels local work and prevents stale completion. Job
execution and side effects are at least once.

A keyed delivery that cannot acquire its active slot remains blocked before the
handler starts. The worker maintains the broker delivery lease with progress
ACKs, so capacity waiting neither consumes another delivery attempt nor causes
max-delivery exhaustion. Handler work and keyed lease renewal begin only after
the slot is acquired.

Jobs retains stream-first durable lifecycle, stable IDs, queue/key coordination,
projection, janitor, cancellation, progress, logs, result/error, dead jobs,
replay/dismiss, and optional live typed updates. Live updates are transient; the
durable lifecycle remains authoritative. Confirmed JetStream publication is
required before acknowledging source work.

Key paths compile to typed scalar accessors. Optional or nonscalar leaves are
compile errors. Existing collision-safe key encoding and reject/coalesce/
replace-oldest queue behavior remain.

Operator Query returns finite cursor pages. Summary applies the same filters to
the full matching set and returns page-independent count, state statistics, and
optional groups; Metrics remains windowed time series. Cursor pagination
rechecks authority on each page and does not generally promise a snapshot.
