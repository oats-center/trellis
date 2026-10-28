---
title: "Operations: Rust"
description: "Expose caller-visible async workflows from a Rust service with typed progress and cancellation."
---

# Use operations from Rust

Use Operations for caller-visible durable workflows and Jobs for private
service execution. Author the Operation and top-level types in native IDL. A
caller selects the Operation once, including its lifecycle and signals.

Generated facades expose typed start, get/watch/wait, signal, and cancel methods.
Production services use Trellis's deployment-scoped durable Operation store;
they do not choose a Store mapping or use an in-memory runtime.

Acceptance is persisted before reply. Within the accepting deployment, the same
invocation identity/input returns the same record; different input conflicts.
Progress, signals, cancellation, and terminal results are durable; typed updates
are live-only.

New starts default to the canonical API without selecting a deployment. Serialize and
retain the complete accepted reference, including `deploymentId`, to restore a
handle. The SDK verifies that deployment against the certified signed acceptance
and routes subsequent get, watch, wait, cancel, and signals internally to it.
An instance can disappear; another replica in the same deployment can take over
using the existing executor lease. The reference supplies routing, not authority.

Do not automatically retry an uncertain start: canonical routing may deliver the
retry to another deployment, whose independent record store can accept the same
invocation ID again. Trellis does not promise cross-deployment deduplication.

For explicit replay after a lost acceptance, persist the target deployment,
invocation ULID, and frozen input before the first request. Generated invokers
expose `start_on_deployment_with_invocation_id(deployment_id, invocation_id,
&input)` and `start_cancelled_on_deployment_with_invocation_id(deployment_id,
invocation_id, &input)`. The latter requires Invoke and Cancel; a later normal
replay cannot clear cancellation intent. The unavailable target never falls
back to another deployment, and choosing a target after an uncertain canonical
start does not recover its unknown owner. Accepted references and controls are
unchanged, including same-deployment replica takeover.

Rust validates operation inputs, signals, progress, outputs, and declared updates
against their descriptor schemas. Schema-validation issue paths use JSON Pointer:
an empty path addresses the root, `/` addresses an empty-name property, and names
escape `~` as `~0` and `/` as `~1`, including missing required fields.

Execution is restartable and at least once. Executor leases fence writes, and a
replica may resume after owner loss. Arbitrary business side effects are not
exactly once. Interrupted uploads restart from byte zero; committed staged bytes
survive executor replacement.

Rust operation handlers can clone `control.cancellation()` and observe its
`Requested`, `OwnershipLost`, or `ServiceStopping` reason with `reason()`,
`is_cancelled()`, or `cancelled().await`. On request, cancel and join owned
side-effecting work—including `spawn_blocking` closures—before the handler
returns. Dropping an async join handle does not stop an already-running
blocking closure. The owner keeps renewing its lease and leaves the durable
snapshot nonterminal while cleanup runs; a caller's `cancel()` returns terminal
`Cancelled` only after cleanup and the fenced terminal write. An interrupted
requested cancellation is finalized on recovery without running the business
handler again. Epoch fencing cannot stop arbitrary external work after owner
loss; use idempotency or external fencing where overlap matters.
