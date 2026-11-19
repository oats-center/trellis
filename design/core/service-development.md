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

Native Store, Job, and Consumer declarations use the same deployment allocation
and revision fence as KV. Store startup preserves owned objects and refuses
retention reductions; desired capacities are planning hints, not enforced limits.
Jobs use the runtime-owned lifecycle/work streams and deployment-scoped keyed
coordination. Consumer allocation includes its original and targeted-replay
durables; reapplication preserves their delivery positions and rejects foreign
ownership or incompatible delivery policies.

Explicit administrative removal of detached Store, Consumer, and Job allocations
uses the resource catalog's existing revision check and removal lease. Store
removal verifies the allocation marker before deleting its bucket. Consumer
removal verifies and deletes only its original and replay durables, retaining the
shared event streams. Job removal deletes its owned work durable and purges only
its lifecycle/work subjects and queue-specific keyed coordination through broker
sequence cutoffs saved in the removal command. Other queues keep their data.

Native device State declarations are projected from accepted installed compiler
schemas into the State resource catalog, including accepted older representations.
Their bootstrap binding identifies the logical deployment allocation; the State
runtime owns physical storage. Device declarations do not register user schemas
or grant raw access to the shared State bucket. Reusing a representation version
with a different schema is rejected during materialization.

Shared State storage keys include the allocation incarnation. Device values add
their authenticated principal namespace beneath that prefix. Explicit detached
State removal purges only the selected allocation's device values (or the exact
user-State key); delayed old removals cannot erase a recreated allocation.

Use Operations for caller-visible durable workflows. They are restartable and at
least once. Use Jobs for private queued work. Consumer concurrency is local per
process; retry and Events DLQ behavior are runtime-managed. Live observations
are transient and owner-cancelled, not durable replay.

Expected domain failures use generated Result-style errors. Preserve request
correlation, cancellation, timeout, and orderly stop/wait behavior.

## Lifecycle, readiness and stop

Both SDKs give a service process the same lifecycle. Its phase is
`connecting` from connect until the serve call (`wait()`, `run()`) has every
registered RPC, Live, operation, job and consumer intake taking work, and again
while the logical connection has lost its transport; then `serving`;
`stopping` once a stop begins; `stopped` once closed. Job workers and durable
consumers registered before the serve call take no job or delivery until the
service serves. A serve call that fails to start, or whose intake never
attaches because the connection closed, ends in `stopped`, so a readiness
waiter learns the service never served. Approval and settings waits happen
inside connect and are reported as status text only. Each phase change is
published in a heartbeat at once.

With `NOTIFY_SOCKET` set (a path or an `@` abstract name), the SDK sends
`READY=1` the first time it serves, `STOPPING=1` when it stops, and `STATUS=`
text for every wait and phase. Rust writes the datagram itself; TypeScript
calls libc through `node:ffi`, so it needs Node 26 and otherwise warns once.
The optional readiness probe is a file present only while serving and/or an
HTTP listener (`/ready` 200 while serving, else 503; `/live` 200 until
stopped). A build retired by a newer installed revision stops with an error.

SIGTERM, SIGINT and the SDK stop call run one graceful stop: retire provider
intake on every generation at once (the queue groups route new work elsewhere),
stop job workers and durable consumers from taking work, end owned Live sessions
with `local_shutdown`, and wait for accepted requests, deliveries and jobs. The
stop timeout (25 s default) starts with the stop and bounds all of it, ending
Live sessions included. A job or delivery that a pull issued before the stop
returns afterwards is naked at once rather than run. At the deadline remaining
jobs are cancelled for shutdown and naked without delay, remaining deliveries
are naked without delay, and remaining requests are abandoned. The connection
then closes once it has written everything already sent, such as final replies
and those naks. A second
signal exits at once. Operations are not drained: their leases expire and
another instance recovers them.
Authorization refresh is owned SDK maintenance that preserves the logical
connection; genuine transport loss triggers the owned recovery lifecycle.
Authorization contexts and route credentials remain process-memory state.

Request status observation belongs to the exact physical transport that carries
the request. Share its status monitor and detach each request's listener when the
exchange settles; completed requests must not retain payloads until another
transport event arrives. Rollover must not redirect retained requests' status
observation to a newer attachment.

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
