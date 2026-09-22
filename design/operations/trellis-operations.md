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

Watch establishes the durable watch before reconciling current state, then emits
strictly later revisions. Signals are persisted before acceptance and may repeat
until handler acknowledgement. Cancellation is persisted; stale owners cannot
complete after losing their fence. Typed updates are live-only and never replace
durable progress.

Provider routing binds an API to a deployment. Queue groups derive only from the
actual subscription subject. Feed cancellation uses owner-specific authenticated
control routing, not the queued open subject.

Upload/download staging is platform-managed rather than mapped to a participant
Store. Committed staged bytes survive executor replacement. An interrupted
in-flight upload is invalidated and restarted from byte zero. Generated transfer
handles keep grant metadata outside application output types.
