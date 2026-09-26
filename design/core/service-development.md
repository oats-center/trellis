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
