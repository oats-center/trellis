---
title: Data Retention
description: Every data class the Trellis runtime keeps, its retention policy and defaults, and what depends on it.
order: 65
---

# Design: Data Retention

## Prerequisites

- [observability-patterns.md](./observability-patterns.md) - Health and Events
  subsystems, projection completeness
- [../jobs/trellis-jobs.md](../jobs/trellis-jobs.md) - job lifecycle stream and
  projection

## Rules

Every data class the runtime writes has an explicit policy: a configured age or
size, removal when its work completes, or deliberate permanence. A policy never
removes data that replay, deduplication, or an open workflow still needs. Where
those needs set a floor, the floor is stated next to the class.

The configured limits of a runtime-owned stream are its policy. Startup
converges an existing stream to them in place, in either direction: a shorter
age or smaller size takes effect at once and JetStream removes only what the
new policy no longer keeps. Startup still refuses, and never recreates, a stream
whose name, subjects, retention kind, discard policy, or sources differ.
JetStream cannot change storage type or retention kind (limits, work queue) in
place, so those differences need an operator decision, not convergence.

SQL history is removed by hourly loops in the subsystem that owns the database,
in transactions of at most 1,000 rows, so a backlog never holds the writer for
long. A failed pass is retried at the next tick and never stops projection.

`Health.Storage` and the `trellis.storage.bytes`, `trellis.storage.records`, and
`trellis.storage.oldest.age` gauges (attribute `trellis.data_class`) report each
class's size, record count, and oldest retained record from JetStream stream
state and read-only SQLite queries. A stream the process's mode does not create,
or a database file not on its host, is absent rather than zero. Database size is
the file plus its write-ahead log; the recorded-events and dead-letter rows are
estimates of their stored bytes.

## Retention table

### JetStream streams

| Data class (`Health.Storage` name)        | Stream                     | Default policy                                                            | Setting                                                     | Depends on it                                                                                                                                        |
| ----------------------------------------- | -------------------------- | ------------------------------------------------------------------------- | ----------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------- |
| Event stream (`eventStream`)              | `trellis`                  | 7 days                                                                    | fixed                                                       | Consumer delivery and `replay all`, the Events projector, the Health stream source, dead-letter creation (the original is copied when exhausted)     |
| Health reports (`healthStream`)           | `TRELLIS_HEALTH`           | 24 hours or 1 GiB                                                         | `[health] transport_retention_hours`, `transport_max_bytes` | The Health projector                                                                                                                                 |
| Job history (`jobHistory`)                | `JOBS`                     | 30 days                                                                   | `[jobs] history_retention_days`                             | Worker lifecycle folding, `Jobs.History`, the Jobs projector; the 2-minute publish deduplication window                                              |
| Job queue (`jobWork`)                     | `JOBS_WORK` (work queue)   | removed when acknowledged; at most 30 days                                | `[jobs] history_retention_days`                             | Workers                                                                                                                                              |
| Job delivery notices (`jobAdvisories`)    | `JOBS_ADVISORIES`          | 7 days                                                                    | fixed (the event stream's retention)                        | Consumer dead-letter creation                                                                                                                        |
| Failed delivery log (`deadLetterJournal`) | `trellis_consumer_dlq`     | no age or size limit; resolved and dismissed entries purged after 90 days | `[events] dead_letter_retention_days`                       | Dead-letter projection rebuilds, replay (the revision-one record holds the only copy of the original), command idempotency, exhaustion deduplication |
| Delivery retries (`replayStream`)         | `trellis_consumer_replays` | 90 days, one envelope per generation                                      | `[events] dead_letter_retention_days`                       | Targeted replay delivery and once-per-generation dispatch                                                                                            |

### SQLite databases

| Data class (`Health.Storage` name)           | Database          | Default policy                                                                                                                                                                                                                   | Setting                               | Depends on it                                                                                |
| -------------------------------------------- | ----------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------- | -------------------------------------------------------------------------------------------- |
| Event history (`recordedEvents`)             | `events.sqlite`   | 90 days after it was recorded                                                                                                                                                                                                    | `[events] history_retention_days`     | `Events.Query`, `Inspect`, `Metrics`, `Diagnostics`                                          |
| Failed deliveries (`deadLetters`)            | `events.sqlite`   | open entries kept; resolved and dismissed entries removed after 90 days unchanged, with their journal records                                                                                                                    | `[events] dead_letter_retention_days` | `DeadLetters.*`, replay and dismiss commands                                                 |
| Jobs database (`jobsDatabase`)               | `jobs.sqlite`     | completed, cancelled, expired, skipped, stale, and dismissed jobs removed 30 days after their last change, with timeline, metadata, lineage, waits, and search entries; error groups and worker registrations unseen for 30 days | `[jobs] history_retention_days`       | Jobs admin queries; `Retry` and `ReplayDLQ` publish the payload from this row                |
| Health database (`healthDatabase`)           | `health.sqlite`   | 30 days of intervals, metric buckets, rejections, and published outbox rows; latest state while the participant is known                                                                                                         | `[health] history_retention_days`     | `Health.*`                                                                                   |
| Sign-in and access database (`authDatabase`) | `platform.sqlite` | expired OAuth artifacts, API reviews, and barriers removed every second; security audit, authorization contexts, revocations, and package evidence permanent                                                                     | none                                  | Event verification and audit (authorization contexts are referenced by every recorded event) |

### Other runtime data

| Data                           | Store                                                 | Policy                                                 |
| ------------------------------ | ----------------------------------------------------- | ------------------------------------------------------ |
| Verification material          | KV `TRELLIS_AUTH_MATERIAL`                            | permanent, immutable                                   |
| Session revocations            | KV `TRELLIS_AUTH_REVOCATIONS`                         | each entry expires with the last context it can affect |
| Platform identity              | KV `TRELLIS_PLATFORM_IDENTITY`                        | one key                                                |
| State values                   | KV `trellis_state`                                    | kept until the resource is deleted                     |
| Runtime leases                 | KV `trellis_runtime_leases`                           | `[leases] ttl_ms`                                      |
| Job key coordination           | KV `JOBS_KEYS_<namespace>`                            | current key state only                                 |
| Service KV and Store resources | `TRELLIS_RESOURCE_*`                                  | the contract's history and TTL                         |
| Operation staging              | object store `trellis_operation_staging_<deployment>` | deleted after settlement; 512 MiB per upload           |

## Reasoning per class

**Recorded events.** The event stream keeps 7 days; the projection keeps its own
copy for 90 by default so history outlives the broker. Removal walks stream
order from the oldest row and stops at the first row recorded within the
window or at the first sequence not yet projected, so the removed history is
always one sequence prefix. Retention is by recording time, not the publisher's
signed event time, so a publisher clock cannot keep rows forever or expire them
early. A removed sequence counts as recorded: a late redelivery is not stored
again. Gap tracking distinguishes the two kinds of absent history:

- missing: the broker discarded an event before it was recorded.
  `gapDetected` and `completeSince` report it.
- removed by policy: `prunedBefore` reports the cutoff of the last removal.
  Once the removed prefix covers every missing sequence, retained history has no
  hole and `gapDetected` turns false; missing history found later is reported
  again.

**Dead letters.** An open entry (dead, replay pending, replaying) is kept
indefinitely: the journal's revision-one record is the only copy of the
original event, and the source stream drops it after 7 days. Only resolved and
dismissed entries are removed. The journal records are purged first, through
the journal sequence the projection observed; a transition appended after it
survives, so a concurrent replay request is never erased. A replay whose
original was removed this way settles as unreplayable instead of stopping the
dispatcher. The retention cannot be shorter than the event stream's 7 days: the
journal is what turns a second exhaustion of the same original into a
duplicate, and the original stays deliverable for that long. A replay envelope
keeps the same retention; one nobody consumed in that time leaves its entry in
replaying.

**Job history.** Replay and retry of a failed or dead job read the payload from
the Jobs projection, so those rows stay regardless of age, as does every
unsettled job. The lifecycle stream's age bounds `Jobs.History` and how far a
projection rebuild reaches. Workers fold a job's lifecycle from that stream, so
the default is far above the default retry schedule; work older than the
lifecycle it needs is dropped from the work queue with it.

**Advisories.** A max-deliveries advisory only names an original event; once
that event is gone from the 7-day event stream, the advisory cannot produce
a dead letter, so it keeps the event stream's retention. An advisory that
expires unprocessed records missing history, as before.

## Not yet bounded

These grow with use and have no policy yet; `Health.Storage` shows their size:

- `trellis_operations_<deployment>` KV: terminal Operation records are never
  deleted.
- Auth database: `auth_session_authorities` gains a permanent row per issued
  authorization context, and authorization sessions, login sessions, grants,
  idempotency results, and resource history have no removal.
- `events_consumer_samples` is declared but nothing writes it.
