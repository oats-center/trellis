---
title: Trellis Operations
description: Durable caller-visible asynchronous workflow semantics.
---

# Trellis Operations

Operations are caller-visible durable, restartable, at-least-once workflows.
Jobs remain service-private execution. Selecting an Operation selects its whole
lifecycle—including get/watch/wait, cancel, and declared signals—subject to
capability approval and exact derived authority. There is no action
sub-selection.

Acceptance persists one deployment-scoped record before reply. The caller
creates a stable invocation ULID. A digest binds Operation identity, canonical
typed input, and initiating principal/participant. Repeating the same ID/digest
returns the existing operation; different content conflicts.

The record retains input, creator, state/revision, durable progress, terminal
output/error, cancellation request, ordered durable signals, executor lease
ownership, timestamps, and transfer reference. Storage revision is the CAS
token. Terminal states are completed, failed, or cancelled; owner crash is not a
business failure.

Each process has an executor ULID. A 30-second lease renewed every 10 seconds
fences all owner mutations. After expiry a replica may resume nonterminal work
with persisted progress and unacknowledged signals. Side effects can repeat and
are not transactionally rolled back; handlers use operation ID/resume context to
make them idempotent.

Service registrations expose an owner-fenced local `control(operationId)` for
durable mutations. `reconcile(operationId)` instead routes to the current owner,
checks the exact route and owner epoch, and re-enters the registered handler
with stored input, verified caller, and persisted progress. A live lease is
never stolen; expired leases use ordinary claim/recovery. Reconciliation
serializes with that owner's execution and returns the latest durable snapshot.
A provider instance cannot use `control` to borrow another instance's fence.

Watch installs its owned durable-watch and optional executor-update
subscriptions, then rereads the authoritative durable record so
watch-before-read readiness is established before the first emitted snapshot. A
record that is already terminal emits its terminal snapshot followed by the
normal observation END. Both sources then merge through one observer arbiter: a
waiting snapshot is conflated to the latest authoritative value while admitted
transient updates are never silently dropped, and the bounded per-observer
buffer fails only that observer with a slow-consumer outcome. Lease-only writes
do not invent business progress. When a terminal snapshot is observed, update
admission freezes, already admitted updates are emitted, then the terminal
snapshot and normal END follow. Signals are persisted before acceptance and may
repeat until handler acknowledgement. Cancellation is persisted; stale owners
cannot complete after losing their fence. Typed updates are live-only and never
replace durable progress. A cancellation request remains nonterminal while the
current handler cleans up; the owner continues renewing its lease and blocks
further progress or completion. The handler observes cancellation (a Rust
observer or TypeScript `AbortSignal`) and must cancel and join its
side-effecting children before returning. Only then may the fenced owner persist
`Cancelled` and return a terminal result to the caller. Stalled cleanup stays
nonterminal without a forced timeout; interrupted cancellation is finalized on
recovery without re-entering business logic. Owner loss notifies the old
handler, but epoch fencing cannot prevent overlap of arbitrary external side
effects after lease expiry. Cleanup duration is recorded as operation telemetry.

The observer is independent of durable execution: its lifetime is bound to the
live session, its authority follows the current observer guard rather than the
opening digest, a local stop or observation signal is observation-only
cancellation that never dispatches a business cancel, and an executor may
continue or finish with zero observers.

Provider routing binds an API to a deployment; the selected provider deployment
comes from the installed binding, and routine admission compiles delivery
permissions from the participant evidence rather than from context atoms. Queue
groups derive only from the actual subscription subject. Live cancellation uses
owner-specific authenticated control routing, not the queued open subject.

Upload/download staging is platform-managed rather than mapped to a participant
Store. Committed staged bytes survive executor replacement. An interrupted
in-flight upload is invalidated and restarted from byte zero. Generated transfer
handles keep grant metadata outside application output types.
