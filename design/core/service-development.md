---
title: Service Development
description: Current service bootstrap, participant, resource, execution, and lifecycle rules.
---

# Service Development

Services author native source packages and connect through generated participant
facades. TypeScript services import `TrellisService` from
`@oatscenter/trellis/service`; Rust services use their generated crate. Supply
the provisioned identity and Trellis URL. Deployment assignment, package
evidence, exact grants, routes, and resource bindings are resolved server-side.

Register generated RPC, Operation, Live, Event Consumer, and Job handlers before
waiting on the service lifecycle. Use generated callers/publishers and the
connected `service.kv`, `service.store`, and `service.jobs` handles. Do not
fetch bindings, derive physical subjects, construct raw handles, or
generate/install packages during startup.

Every public TypeScript handler callback receives the same flat,
contract-selected `ServiceHandlerClient`; registration and lifecycle methods
remain on the connected service.

Native IDL `implements Api;` declares complete provider ownership.
`use Api {
... }` selects exact outbound interactions. Capability approval and
exact grants are separate: declarations define eligible behavior, the server
derives current authority, and temporary provider liveness remains an
availability concern.

State, KV, Store, Job, and Consumer resources are approved/provisioned before
use. Resource desires do not authorize expansion. Removal detaches access and
retains new-version data; compatible approved re-add reattaches. State is one
value, KV is typed history, and Store is raw bytes.

Use Operations for caller-visible durable workflows. They are restartable and at
least once. Use Jobs for private queued work. Consumer concurrency is local per
process; retry and Events DLQ behavior are runtime-managed. Live observations
are transient and owner-cancelled, not durable replay.

Expected domain failures use generated Result-style errors. Preserve request
correlation, cancellation, timeout, and orderly stop/wait behavior.
Authorization refresh is owned SDK maintenance that preserves the logical
connection; genuine transport loss triggers the owned recovery lifecycle.
Authorization contexts and route credentials remain process-memory state.

## Request admission and concurrency

RPC handlers execute concurrently; Trellis does not promise execution or
completion order. Use transactions, revision checks, or explicit application
sequencing when order matters. One connected service owns a shared, non-waiting
dispatch budget across endpoints and physical transport generations. Incoming
work is bounded by both unfinished-request count and retained incoming message
bytes. NATS buffers and decoded application objects are separate from this byte
accounting.

Verified excess requests receive `trellis.service.busy`: the request was not
executed. The SDK does not automatically retry that refusal. Verification itself
has a separate bounded allowance; traffic that cannot be safely verified and
answered is dropped, not reflected to an unverified reply destination. Built-in
Operation get/cancel and Live connection controls have reserved bounded
capacity. Application signals use ordinary capacity. Live control ordering is
preserved. An Operation start releases its dispatch reservation after the start
response; ongoing Operations retain independent lifecycle and limits. Accepted
requests retain their receiving generation through reply handoff and participate
in its existing retirement/drain lifecycle.

TypeScript services can opt into the experimental worker verifier with
`runtime.verificationWorkers: true`. One ordinary worker and one reserved worker
start with the same Rust/WASM request and Transfer proof verification. Ordinary
capacity grows one worker at a time when every ordinary worker is busy and the
oldest waiting proof has waited at least 20 ms continuously for 100 ms. These
are trial thresholds, not universal latency targets.
`runtime.maxVerificationWorkers` bounds ordinary workers (including workers
starting), defaulting to three; the reserved worker is additional. Starting
workers count toward this ceiling. Added workers retain capacity until shutdown;
there is no shrinking or cooldown. An ordinary worker failure disables further
growth for the pool; healthy workers continue without inline fallback. Only
queued, never attempted proofs move between ready ordinary workers; active or
failed attempts are never retried. Admission limits remain shared. Controls
precede refusal verification on the reserved worker; Live control sequence
ordering is unchanged. Pending verification remains inside admitted work, with
independently bounded scheduler occupancy. Exact payload bytes are copied and
transferred only when a worker becomes available; the incoming message remains
owned by its dispatch callback. This is not a throughput guarantee or a change
to defaults.

The authoritative main-thread cache retains revocation coverage and context
identity. Before dispatching a worker result, it rechecks the exact entry and
uses Rust/WASM to check context and proof freshness at the current time. Worker
failure fails verification closed, without fallback or automatic retry. Workers
retain their own context handles, release them when the cache releases the
corresponding entry, and terminate when the service releases its authorization
cache. Event and Live-frame proof verification remain on their existing paths.

Terminal service shutdown and generation disposal stop local request/control
subscriptions without waiting for a broker flush, then settle accepted callbacks
before releasing their authority and transport ownership. Planned generation
rollover still drains intake with broker acknowledgment; final local disposal
must be able to interrupt that acknowledgment wait when transport is
unavailable.

TypeScript configures local limits through `runtime.requestLimits` on
`TrellisService.connect`; Rust uses `ServiceConnectOptions::with_request_limits`
and `RequestLimits`. These are local runtime settings, not contract permissions
or queued-job limits. Production `trellis.service.admission.inflight`, `.bytes`,
and `.rejections` distinguish ordinary, control, and refusal-verification
capacity. In-flight values cover dispatch, not an Operation's entire execution.

## Minimal installable service example

```trellis
model EchoInput { value: string; }
model EchoOutput { value: string; }

api echo@v1 {
  title "Echo";
  description "Echo a value.";
  rpc Run { input EchoInput; output EchoOutput; }
  capabilities { public { allows { rpc Run; } } }
}

service EchoService { implements echo; }
```

Run `trellis install`, `trellis generate`, apply the exact package/participant
source with required capability/resource approvals, provision an instance, then
connect through the generated `EchoService` facade.
