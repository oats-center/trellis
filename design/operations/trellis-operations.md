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

Native provider provisioning creates the durable deployment-scoped framework KV
repository and staging object store through the platform's operational owner.
Startup verifies their physical ownership before committing framework binding
evidence. These stores are not participant-declared KV allocations or generated
resource handles. Ordinary provider connection opens them only when the installed
participant implements Operations. Broker storage rights are limited to the exact
committed stores of a current, accepted provider binding; caller capabilities do
not grant raw access to the workflow repository or staging store.

Acceptance persists one deployment-scoped record before reply. The caller
creates a stable invocation ULID. A digest binds Operation identity, canonical
typed input, and initiating principal/participant. Repeating the same ID/digest
within that deployment returns the existing operation; different content conflicts.
Deduplication does not span deployments. An uncertain start must not be
automatically retried through the canonical API queue: another deployment could
accept a second operation with the same invocation ID. Callers may admit
that same ID/input with `cancellationRequested: true`, requiring both Invoke and
Cancel authority before any durable write. Additional permission checks retain
the original verified Invoke opening for admission reply signing. An absent invocation persists
cancellation intent before owner admission; an existing matching nonterminal
invocation merges it monotonically through CAS. Intent is not part of the
business-input digest. Terminal replay is unchanged, and a late normal start
cannot clear intent. The returned reference acknowledges admission, not
completed cleanup. If normal admission wins the race, its single handler must
join owned work and complete cancellation through the ordinary cleanup path.

The record retains input, creator, state/revision, durable progress, terminal
output/error, cancellation request, ordered durable signals, executor lease
ownership, timestamps, and transfer reference. Storage revision is the CAS
token. Terminal states are completed, failed, or cancelled; owner crash is not a
business failure.
Replica control writes preserve the complete retained upload attempt, including
unfinished state, staging locator, grant, and transfer metadata. Only creation or
a fenced replacement may allocate another staging identity; accepting a signal
or cancellation must not reconstruct it.

Each process has an executor ULID. A 30-second lease renewed every 10 seconds
fences all owner mutations. After expiry a replica may resume nonterminal work
with persisted progress and unacknowledged signals. Side effects can repeat and
are not transactionally rolled back; handlers use operation ID/resume context to
make them idempotent.
If loss of authoritative state or the execution's receiving carrier interrupts
an execution before cancellation has been observed, any failed state read or
watcher retires its local fence before
returning the infrastructure error to handler code, and its heartbeat must stop
renewing that epoch. A handler cannot turn this interruption into a durable
business failure. Recovery waits for the existing durable lease to expire;
reconnecting transport alone does not revive the interrupted execution. This is
distinct from known cancellation cleanup, which retains its lease until cleanup
succeeds.
Observation owns its own healthy carrier; an executor's disconnected receiving
carrier must not prevent observing retained state while its lease awaits takeover.

Service registrations expose an owner-fenced local `control(operationId)` for
durable mutations. `reconcile(operationId)` instead routes to the current owner,
checks the exact route and owner epoch, and re-enters the registered handler
with stored input, verified caller, and persisted progress. A live lease is
never stolen; expired leases use ordinary claim/recovery. Reconciliation
serializes with that owner's execution and returns the latest durable snapshot.
A provider instance cannot use `control` to borrow another instance's fence.

Get and a watcher's initial snapshot read the persisted record, not the handler's
speculative execution snapshot. A committed write remains visible even when its
acknowledgement was lost. Reads do not refresh the active execution's mutable
record while its fenced write is in flight.

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
repeat until handler acknowledgement. Handlers read only persisted signals;
acceptance and acknowledgement serialize with state updates on the same local
operation frame queue. Cancellation is persisted; stale owners
cannot complete after losing their fence. Typed updates are live-only and never
replace durable progress. A cancellation request remains nonterminal while the
current handler cleans up; the owner continues renewing its lease and blocks
further progress or completion. The handler observes cancellation (a Rust
observer or TypeScript `AbortSignal`) and must cancel and join its
side-effecting children before returning. Only then may the fenced owner persist
`Cancelled` and return a terminal result to the caller. Stalled cleanup stays
nonterminal without a forced timeout. Cancellation-requested recovery invokes
the handler with Rust `Requested` or an aborted TypeScript signal already set to
retry cleanup without starting new business work. Cleanup failure remains
nonterminal; Rust `Ok(())` or a successful, non-deferred TypeScript return
acknowledges successful cleanup before fenced cancellation finalization. Owner
loss notifies the old handler, but epoch fencing cannot prevent overlap of
arbitrary external side effects after lease expiry. Cleanup duration is recorded
as operation telemetry.

The observer is independent of durable execution: its lifetime is bound to the
live session, its authority follows the current observer guard rather than the
opening digest, a local stop or observation signal is observation-only
cancellation that never dispatches a business cancel, and an executor may
continue or finish with zero observers.

Untargeted new starts use canonical generated API subjects and the logical API's shared
request queue, without caller selection of a service or deployment. Every
serialized Operation reference requires `id`, `service`, `operation`, and
`deploymentId`. The accepting provider supplies the durable record's deployment;
the SDK verifies it against the signed acceptance and certified provider identity.
Get, watch, wait, cancel, and signals route internally to that reference's
deployment. Restoration requires the complete reference, never just an ID. A handler reads
its own complete reference, for example to embed in a return URL.
This routing metadata is not authority: receivers still validate caller identity,
capability, and durable operation ownership. Instance IDs do not identify durable
owners; replicas within the owning deployment use the existing leases and CAS
fences for takeover. Live cancellation retains authenticated owner-specific
session control routing, not the queued open subject. No global routing directory
or persistent caller-to-provider selection is involved.

Canonical start subjects carry the generated API's major version. Established
owner-directed subjects retain the fixed `operation.v1` transport-family prefix
and encode the full API identity, including that API major. These versions are
independent. Implementation-only providers can subscribe to their own
deployment-scoped transient-update forwarding subject without selecting caller
Observe authority; that maintenance right grants no observation of another
deployment or caller authority.

Durable references and repository records use the canonical
`<version-qualified API identity>:operation:<action name>` in both SDKs. Generated
method aliases are only language ergonomics: a same-named action added to another
API cannot rename an existing Operation or merge its repository scope.

Both SDKs persist one framework-private record through the shared Rust/WASM
codec: TRKV envelope format 1, representation version 2. Descriptor-encoded JSON
input, progress, output, errors, and signal payloads remain JSON values without
a second binary encoding. Invocation identity, executor epoch/lease, original
cancellation timestamp, signal acknowledgement, and upload commitment have the
same meaning across replicas. Record revisions, fencing epochs, and update
sequences fit the exact JSON integer range; descriptor-owned uint64 values retain
their generated decimal-string codec.
Application-defined error fields survive repository reads and writes.

The owning deployment's CAS revision remains authoritative. Codec validation is
not authorization, an executor claim, or permission to replay application work.
Unsupported or corrupt persisted data fails visibly without becoming absence,
being rewritten, or causing empty storage recreation. This pre-release storage
cutover has no migration or dual-read path; retained installations and their old
records are never deleted or silently converted.

Upload/download staging is platform-managed rather than mapped to a participant
Store. Committed staged bytes survive executor replacement. An interrupted
in-flight upload is invalidated and restarted from byte zero. Generated transfer
handles keep grant metadata outside application output types.
Both SDKs return the original logical file metadata after takeover; the physical
staging locator remains private and is used only to open the staged object.

Operation upload uses Transfer v1's bounded one-way raw-byte stream,
independently of Live-based Operation Watch. Both SDKs issue upload grants from
one shared protocol policy: a grant lasts 15 minutes and admits at most 512 MiB.
Grant expiry ends an unfinished upload, so the lifetime bounds the whole
transfer, not only activation. Network credit follows
complete-frame consumption by staging storage, never durable Operation progress
persistence. One owned latest-value worker coalesces durable progress at 1 MiB
of newer consumption or 250 ms of pending progress; it does not enqueue one
repository mutation per frame. `transferredBytes` is monotonic, while
`chunkIndex` and `chunkBytes` describe the latest credited frame represented.
Watch consumers must not assume one durable event per frame.

Completion synchronizes the newest progress with the owner-fenced, nonterminal
durable transfer mutation: exact final progress, committed staged metadata, and
handler-runnable state must be persisted before the authoritative snapshot and
handler continuation. Client `committed` cannot precede that durable boundary.
Backend completion alone does not make an Operation upload successful. An
interrupted uncommitted attempt is invalidated and replaced from byte zero;
observers neither own nor cancel this execution lifecycle.
