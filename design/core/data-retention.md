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

Retention is an age, a message count, or completion wherever that is what the
data needs. A stream gets a size bound only where the size itself is the
requirement: the webhook intake stream, which senders outside the installation
fill, and the Health stream when an operator sets `transport_max_bytes`.
JetStream reserves a stream's whole size bound against the account and the
server when the stream is created, whether or not it holds anything, and
refuses a stream whose reservation does not fit what is left. A broker on a
small disk computes its storage limit from the free space, about a gigabyte on
a small VM, so a large default bound would take that budget from every other
stream. Every runtime stream without a size bound reserves nothing; the intake
default is 64 MiB per deployment with endpoints.

The configured limits of a runtime-owned stream are its policy. Startup
converges an existing stream to them in place, in either direction: a shorter
age or smaller size takes effect at once and JetStream removes only what the
new policy no longer keeps. Startup still refuses, and never recreates, a stream
whose name, subjects, retention kind, discard policy, or sources differ.
JetStream cannot change storage type or retention kind (limits, work queue) in
place, so those differences need an operator decision, not convergence.

When JetStream refuses to create or resize a runtime stream for lack of room
(insufficient storage or memory, or the account's stream limit), startup waits
and tries again with the storage backoff, as it waits out a locked database.
Every subsystem needs its streams, so nothing is served meanwhile. The refusal
is logged once with what was asked for and the account's and server's storage
used, reserved, and limit (the server's when the process has system-account
credentials), then every ten minutes while it lasts, and the server starts as
soon as there is room.

A write a participant makes is refused the same way once there is no room:
KV, store, State, job and event writes return a typed resource-exhausted
error naming the resource (its kind, `kv`, `store`, `state`, `jobs` or
`events`, its name, and what JetStream ran out of) instead of a generic or
"temporarily unavailable" failure. The SDKs recognize the JetStream codes for
insufficient storage or memory (10047, 10023, 10002, 10028), the stream and
consumer limits (10027, 10026) and a full stream (10077), as the runtime does
for its own streams. Nothing was written, so retrying succeeds only once there
is room. A managed broker keeps the `max_file_store` and `max_memory_store`
the authored `nats.conf` sets in its `jetstream` block.

Stopping the server while JetStream has no room still exits 0. Releasing a
runtime owner lease is a write to the lease bucket; when JetStream refuses it
for lack of room, the server logs a warning naming the lease and leaves it to
lapse at the end of its term, so the next owner waits at most that long.

Removing a deployment is final: `Deployments.Remove` cannot be undone, and a
removed deployment cannot be enabled or applied again. The streams Trellis made
for it hold data nothing can read afterwards, and the intake stream holds its
size bound reserved, so Trellis deletes them:

- The webhook intake stream `TRELLIS_WEBHOOKS_<deployment>`. The webhook
  runtime stops the deployment's worker and deletes the stream when it reloads
  its endpoints after the removal, and again at every pass, once a minute and
  at startup, for any intake stream left for a removed deployment. A stream of
  that name whose subjects are not the deployment's is left alone. Requests
  still waiting for verification are recorded as expired. The request records
  themselves keep expiring under the deployment's record retention, and dedupe
  IDs under their window; neither reserves storage.
- The Operation records `KV_trellis_operations_<deployment>` and the upload
  staging store `OBJ_trellis_operation_staging_<deployment>`. The removal
  queues an Auth effect in the same transaction, which deletes both and is
  retried until it succeeds, across restarts. A bucket whose description is
  not the one Trellis wrote is left in place and the effect set aside.
  Enrollment prepares this storage after its own transaction: it checks that
  the deployment is active, creates the buckets, and checks again. A removal
  can commit between the checks, so while a preparation for the deployment is
  under way the deletion waits and is retried. A preparation that starts after
  the removal commits creates nothing, and one that finishes after it fails
  and leaves the buckets to the deletion. A server that stops mid-preparation
  has nothing under way after it starts again, so the deletion runs then.

A server that stops between the removal and the deletion deletes the streams
after it starts again, since both paths work from Auth's record of the removal,
not from anything held in memory. There is no grace period, because there is
nothing to recover a removed deployment into.

Resources the service declared (KV, Store, State, Consumers, Jobs, webhook
consumers) are not deleted with the deployment. They are detached, keep their
data, and go only when an administrator runs `trellis resources destroy` on
each, as with a resource a service update omits.

SQL history is removed by hourly loops in the subsystem that owns the database,
in transactions of at most 1,000 rows, so a backlog never holds the writer for
long. A failed pass is retried at the next tick and never stops projection.

The same holds for every background loop that writes a runtime database: the
Auth startup drain, enforcement, effect, event-publication, cleanup and
disconnect work, the Jobs and worker-presence projectors, the Events projector
and exhaustion advisories, and the Health projector, deadlines and outbox.

Every connection to a runtime database, the webhook records beside the Auth
database and the migration and backup connections included, waits for a lock
another connection holds for its subsystem's `[<subsystem>.storage]
busy_timeout_ms`, five seconds when unset. A failure is then one of three kinds:

- It can pass: the lock is held longer, the database is busy, an I/O was
  interrupted or failed, or the disk is full. The same work is tried again,
  indefinitely, with a warning each time: 250 milliseconds after the first
  failure, doubling up to 30 seconds, or for Health at its next tick (logged at
  most once a minute while it repeats) and a heartbeat redelivered after five
  seconds. Durable work stays queued and a projector retries the same batch.
  Startup waits the same way, logging each failed attempt with the work it was
  doing: the migrations, and Auth, Jobs, Events and Health startup each retry
  every step that writes their database, so a server started while any of its
  databases is locked, or one locked part way through startup, starts once the
  lock is released. A stop request ends the wait at once.
- It fails the same way on every attempt, such as a constraint violation or a
  value that cannot be decoded, for one item. That item is logged once at
  error with its ID and set aside, and the loop goes on with the next one. An
  Auth effect or event publication keeps its row with `failed_at` and the
  error for 30 days, is never claimed again, and no longer holds back the
  effects queued after it; Auth's five-minute cleanup pass then removes it. A
  Health status event is kept with `set_aside_at_ns` until retention removes
  it; a heartbeat or coverage gap is dropped. An Auth enforcement page that
  fails this way is deferred a minute rather than dropped, since its sessions
  still need enforcing, so the work queued after it proceeds. That work, the
  one the failed page selected even if other work has since moved ahead of it
  in the queue, is marked stuck: the error is logged when it first fails and
  then once an hour while it keeps failing, and Auth's `enforcement` health
  check reports degraded, naming the work, the time it first failed and the
  latest error, until a page of it succeeds. Startup work that fails this way, such as a
  migration whose checksum differs, stops startup with a message naming the
  work. A failure that is not tied to one item, such as a projector's batch
  bookkeeping, is retried and logged at error.
- The database file is corrupt or not a database. Its subsystem stops, and
  with it trellis-server, with a message naming that database.

`Health.Storage` also reports JetStream capacity: the Trellis account's storage
and memory used and reserved against its limits, the server's from the system
account's `JSZ` request when the process holds a system-account connection
(the platform role does), stream and consumer counts, and the largest streams
by bytes stored or reserved. In a cluster every server answers `JSZ`; storage
is reported from the server with the largest share of its storage committed
and memory from the one with the largest share of its memory committed, which
can be different servers, since each refuses first for its resource. When
JetStream does not list every stream, the largest streams are absent rather
than a partial list. It is `degraded`, with each problem and its numbers,
while used or reserved bytes, streams, or consumers reach 85% of a limit, and
while JetStream refuses a resource this process needs. A refusal is reported
until the resource is created or written, or until nothing needs it any more,
as when the last webhook endpoint of a deployment whose intake stream was
refused is removed. `trellis storage` and the Console Health page show it.

`Health.Storage` and the `trellis.storage.bytes`, `trellis.storage.records`, and
`trellis.storage.oldest.age` gauges (attribute `trellis.data_class`) report each
class's size, record count, and oldest retained record from JetStream stream
state and read-only SQLite queries. A stream the process's mode does not create,
or a database file not on its host, is absent rather than zero. Database size is
the file plus its write-ahead log; the recorded-events and dead-letter rows are
estimates of their stored bytes.

## Retention table

### JetStream streams

| Data class (`Health.Storage` name)        | Stream                          | Default policy                                                                                                           | Setting                                                     | Depends on it                                                                                                                                        |
| ----------------------------------------- | ------------------------------- | ------------------------------------------------------------------------------------------------------------------------ | ----------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------- |
| Event stream (`eventStream`)              | `trellis`                       | 7 days                                                                                                                   | fixed                                                       | Consumer delivery and `replay all`, the Events projector, the Health stream source, dead-letter creation (the original is copied when exhausted)     |
| Health reports (`healthStream`)           | `TRELLIS_HEALTH`                | 24 hours, no size bound                                                                                                  | `[health] transport_retention_hours`, `transport_max_bytes` | The Health projector                                                                                                                                 |
| Webhook intake                            | `TRELLIS_WEBHOOKS_<deployment>` | 7 days or 64 MiB, whichever is reached first; a full stream refuses new requests; deleted when the deployment is removed | deployment settings `intakeRetention`, `intakeMaxBytes`     | `async` endpoints' verification, the request record's stored body                                                                                    |
| Job history (`jobHistory`)                | `JOBS`                          | 30 days plus one                                                                                                         | `[jobs] history_retention_days`                             | Worker lifecycle folding, `Jobs.History`, the Jobs projector; the 2-minute publish deduplication window                                              |
| Job queue (`jobWork`)                     | `JOBS_WORK` (work queue)        | removed when acknowledged; unclaimed jobs expire after 30 days                                                           | `[jobs] history_retention_days`                             | Workers                                                                                                                                              |
| Job delivery notices (`jobAdvisories`)    | `JOBS_ADVISORIES`               | 7 days                                                                                                                   | fixed (the event stream's retention)                        | Consumer dead-letter creation                                                                                                                        |
| Failed delivery log (`deadLetterJournal`) | `trellis_consumer_dlq`          | no age or size limit; resolved and dismissed entries purged after 90 days                                                | `[events] dead_letter_retention_days`                       | Dead-letter projection rebuilds, replay (the revision-one record holds the only copy of the original), command idempotency, exhaustion deduplication |
| Delivery retries (`replayStream`)         | `trellis_consumer_replays`      | 90 days, one envelope per generation                                                                                     | `[events] dead_letter_retention_days`                       | Targeted replay delivery and once-per-generation dispatch                                                                                            |

### SQLite databases

| Data class (`Health.Storage` name)           | Database          | Default policy                                                                                                                                                                                                                                                                           | Setting                                                                | Depends on it                                                                                                                |
| -------------------------------------------- | ----------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------- |
| Event history (`recordedEvents`)             | `events.sqlite`   | 90 days after it was recorded                                                                                                                                                                                                                                                            | `[events] history_retention_days`                                      | `Events.Query`, `Inspect`, `Metrics`, `Diagnostics`                                                                          |
| Failed deliveries (`deadLetters`)            | `events.sqlite`   | open entries kept; resolved and dismissed entries removed after 90 days unchanged, with their journal records                                                                                                                                                                            | `[events] dead_letter_retention_days`                                  | `DeadLetters.*`, replay and dismiss commands                                                                                 |
| Jobs database (`jobsDatabase`)               | `jobs.sqlite`     | completed, cancelled, expired, skipped, stale, and dismissed jobs removed 30 days after their last change, with timeline, metadata, lineage, waits, and search entries; error groups and worker registrations unseen for 30 days                                                         | `[jobs] history_retention_days`                                        | Jobs admin queries; `Retry` and `ReplayDLQ` publish the payload from this row                                                |
| Health database (`healthDatabase`)           | `health.sqlite`   | 30 days of intervals, metric buckets, rejections, and published outbox rows; latest state while the participant is known                                                                                                                                                                 | `[health] history_retention_days`                                      | `Health.*`                                                                                                                   |
| Sign-in and access database (`authDatabase`) | `platform.sqlite` | expired OAuth artifacts, API reviews, and barriers removed every second; set-aside effects and event publications removed 30 days after they were set aside, in batches of 1,000 every five minutes; security audit, authorization contexts, revocations, and package evidence permanent | none                                                                   | Event verification and audit (authorization contexts are referenced by every recorded event)                                 |
| Retired and removed service instances        | `platform.sqlite` | the instance, its identity key, and its settings overrides deleted 30 days after it was retired or removed, in batches of 1,000 every five minutes; the principal and a key tombstone (fingerprint and outcome) permanent                                                                | `Auth.InstancePolicy` `retiredRecordDays` (Auth DB, not configuration) | A returning key-file host learns whether its key was removed or retired; audit and authorization contexts name the principal |

### Other runtime data

| Data                           | Store                                                 | Policy                                                                |
| ------------------------------ | ----------------------------------------------------- | --------------------------------------------------------------------- |
| Verification material          | KV `TRELLIS_AUTH_MATERIAL`                            | permanent, immutable                                                  |
| Session revocations            | KV `TRELLIS_AUTH_REVOCATIONS`                         | each entry expires with the last context it can affect                |
| Platform identity              | KV `TRELLIS_PLATFORM_IDENTITY`                        | one key                                                               |
| State values                   | KV `trellis_state`                                    | kept until the resource is deleted                                    |
| Runtime leases                 | KV `trellis_runtime_leases`                           | `[leases] ttl_ms`                                                     |
| Job key coordination           | KV `JOBS_KEYS_<namespace>`                            | current key state only                                                |
| Service KV and Store resources | `TRELLIS_RESOURCE_*`                                  | the contract's history and TTL                                        |
| Operation records              | KV `trellis_operations_<deployment>`                  | deleted with the deployment                                           |
| Operation staging              | object store `trellis_operation_staging_<deployment>` | deleted after settlement, and with the deployment; 512 MiB per upload |

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
survives, so a concurrent replay request is never erased. That request has
lost its original, though: the sweep removed the revision-one record with the
rest. Copying the original into every replay request would close that window
at the cost of storing each payload twice, so the race is settled instead. The
dispatcher records the replay as dead again, with an error saying retention
removed the original while the replay was being accepted, and a further replay
is refused as unreplayable; the operator dismisses it. The sweep drops the
original and the purged transitions from the projection row, and a projection
rebuilt from the purged journal starts the entry at the surviving request, so
both hold the same entry. A request that reads the entry before the purge but
appends after it is refused as not found. The retention cannot be shorter
than the event stream's 7 days: the
journal is what turns a second exhaustion of the same original into a
duplicate, and the original stays deliverable for that long. A replay envelope
keeps the same retention; one nobody consumed in that time leaves its entry in
replaying.

**Job history.** Replay and retry of a failed or dead job read the payload from
the Jobs projection, so those rows stay regardless of age, as does every
unsettled job. The lifecycle stream's age bounds `Jobs.History` and how far a
projection rebuild reaches. Workers fold a job's lifecycle from that stream, so
the default is far above the default retry schedule.

A job may be submitted without a deadline, and its workers may stay away.
Keeping its queued work longer than the retention would not help: the worker
that finally claims it folds a lifecycle that has aged out of `JOBS` with it,
and keeping that stream longer for one job keeps it for all. So the retention
is also how long a job waits for a worker. The hourly Jobs retention pass
publishes Expired, with an error saying no worker claimed it, for every job
still Pending since it was created or replayed more than the retention ago.
Both streams keep one day past the retention so that record lands while the
work and its lifecycle are still there: a worker that claims it then folds
Expired and runs nothing. If the Jobs subsystem was down longer than that day,
the next pass still records Expired after the broker dropped the work, so the
projection never shows a job Pending that no worker can receive. A job a
worker has started is settled by its execution owner, not by retention.

**Advisories.** A max-deliveries advisory only names an original event; once
that event is gone from the 7-day event stream, the advisory cannot produce
a dead letter, so it keeps the event stream's retention. An advisory that
expires unprocessed records missing history, as before.

## Not yet bounded

These grow with use and have no policy yet; `Health.Storage` shows their size:

- `trellis_operations_<deployment>` KV: terminal Operation records are deleted
  only with the deployment.
- Auth database: `auth_session_authorities` gains a permanent row per issued
  authorization context, and authorization sessions, login sessions, grants,
  idempotency results, and resource history have no removal.
- `events_consumer_samples` is declared but nothing writes it.
