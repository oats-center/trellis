---
title: Trellis Patterns
description: High-level Trellis architecture, platform boundaries, service categories, and communication patterns.
order: 10
---

# Design: Trellis Patterns

## Prerequisites

None.

## Context

Trellis is a distributed system for aggregating, processing, and distributing
organizational data. Connected Trellis contract calls use NATS. Applications can
also use external HTTP APIs, databases, or other systems; Trellis does not
prescribe their entire communication architecture.

This document establishes the top-level cross-cutting system patterns for:

- service boundaries
- communication patterns
- platform boundaries
- the relationship between subsystem-specific pattern docs

Detailed coding, storage, type-system, observability, frontend, and capability
guidance is split into companion documents.

## Architecture

### Service Categories

| Category       | Purpose                                | Examples           |
| -------------- | -------------------------------------- | ------------------ |
| Infrastructure | Platform capabilities for all services | Auth, Jobs         |
| Ingest         | Pull external data, emit domain events | Zendesk, FoodLogiQ |
| Repository     | Persist and query domain data          | Graph, Search      |
| Processing     | Transform, enrich, derive knowledge    | Classification     |
| Egress         | Push data to external systems          | Laserfiche         |

These are example application roles, not Trellis participant kinds or required
service boundaries. Any service may subscribe to events for cache invalidation
or local state.

### Platform Boundary

Trellis platform code and cloud/domain code are intentionally separate.

Rules:

- the Trellis platform repo owns protocol/runtime libraries, the `trellis`
  runtime service, jobs, Trellis-owned contracts, and contract tooling
- cloud repos own domain services, domain contracts, apps, and domain models
  unless a model is required by a Trellis-owned contract or shared Trellis
  runtime library
- `@qlever-llc/trellis` is a runtime library, not a central registry for every
  service API
- service APIs are defined with the service that owns them and consumed through
  native source-package dependencies and consumer-local generated SDKs

### Communication Patterns

#### Events

Events announce state changes. Publishers fire and forget.

Subject naming:

```text
events.v1.<A>.<Event>.<...tokens>
```

`A` is the unpadded base64url encoding of the event's qualified API identity
(`<package>.<api>@v<major>`). This keeps the wire subject aligned with the
generated API descriptor and its ACL identity.

Each parameter token is the unpadded base64url encoding of its canonical UTF-8
value. The signed event descriptor carries the qualified API ID, exact dotted
event name, and parameter count so overlapping textual subjects remain
unambiguous during authorization and projection.

Examples:

```text
events.v1.YWNtZS5wYXJ0bmVyQHYx.Changed.<origin>.<id>
events.v1.YWNtZS5pZGVudGl0eUB2MQ.Changed.<origin>.<id>
events.v1.YWNtZS5kb2N1bWVudEB2MQ.Uploaded.<contentType>.<partnerId>
```

Rules:

- add subject tokens only when consumers need selective subscription and the
  cardinality is bounded and stable
- token order matters; put the most-filtered tokens first
- event handlers must be idempotent because delivery is at-least-once
- direct event publish is the default; use a SQL outbox only when event
  publication must be coupled to service-local SQL state
- TypeScript services create a SQL outbox helper with
  `service.createSqlOutbox(...)`; the returned object is a plain dependency that
  handlers close over at registration rather than receiving it through handler
  arguments. The same SQL outbox also supports transactional job submission.
  Handlers receive a `job` facade alongside the `event` facade within
  `outbox.transaction(...)`.
- outbox dispatch MAY use a process-local wakeup helper to reduce latency, but
  the wakeup MUST happen after the outbox transaction commits; enqueueing a row
  inside a transaction must not directly publish work that can later roll back
- consumers should use an inbox only for handlers that are not naturally
  idempotent
- Trellis owns SQL outbox/inbox helper-table schema and versioned migration
  artifacts; services own database lifecycle, migration execution, table names,
  and transaction boundaries
- NATS KV inbox/outbox helpers provide durable dedupe/queue storage but are not
  transactional with unrelated database side effects
- process-local outbox wakeups are latency optimizations only; durable retry and
  recovery still depend on persisted outbox state and an explicit dispatch or
  recovery scan
- a prepared event transport record MUST NOT duplicate the publisher's contract
  id or contract digest
- event subscribe permissions are event-type gates, not per-entity ACLs; do not
  encode user-owned object lists into Trellis runtime permissions

#### Feeds

Feeds expose caller-visible live views that are authorized by the owning
service. They are request/reply streams: a caller requests a feed with typed
input, and the service emits typed frames to the caller's reply inbox.

Subject naming:

```text
feeds.v1.<Domain>.<LiveView>
```

Examples:

```text
feeds.v1.Device.Events
feeds.v1.Audit.Feed
feeds.v1.Inspection.Updates
```

Rules:

- use feeds when normal apps need reactive UI updates filtered by application
  authorization, such as devices visible to the logged-in user
- feed subjects are request subjects, not raw event subjects
- the service owns fine-grained authorization against the authenticated caller,
  feed input, and every emitted frame
- normal apps that use feeds do not receive raw `events.v1.*` subscribe
  permissions for the backing domain events
- feeds are live streams, not durable operations; use operations when the caller
  needs resumable workflow state or terminal completion

#### RPCs

RPCs query data or perform bounded synchronous operations. Caller-visible
long-running workflows use operations.

Subject naming is domain-based rather than service-based:

```text
rpc.v1.User.Find
rpc.v1.Partner.List
rpc.v1.Documents.Search
```

Rules:

- callers use method names, not raw subjects, in normal code
- the API schema maps methods to transport subjects
- implementation details may change without changing caller-visible method names

#### Operations

Operations are caller-visible asynchronous workflows with durable state,
explicit progress, and watchable completion.

Subject naming:

```text
operations.v1.<Domain>.<...tokens>
```

Rules:

- use operations when the caller must observe progress or wait across reconnects
- use RPCs for bounded synchronous work and jobs for service-private execution
  machinery
- operation control and watch semantics are defined in
  [../operations/trellis-operations.md](./../operations/trellis-operations.md)

#### Runtime subject spaces

Some subsystem-owned subject spaces are not `events.v1.*`, `rpc.v1.*`, or
`operations.v1.*`. They exist for infrastructure coordination, stream
projections, and service-private transport protocols.

Rules:

- public and cross-service boundaries must be modeled as contract-owned RPCs,
  operations, events, feeds, jobs, state, or resources rather than caller-
  authored raw subject declarations
- Trellis-owned runtime protocols may still use raw subjects behind a
  contract-owned public API; file transfer chunk subjects are an example of this
  pattern
- subsystem docs should define the semantics and naming rules for any runtime
  subject space they introduce
- examples include jobs work subjects and other platform-owned control surfaces
  described in companion docs

## Companion Documents

This document defines the high-level system style. Detailed companion docs are
split by concern:

- [platform-libraries.md](./platform-libraries.md) - package responsibilities
  and core runtime/library guidance
- [files-transfer-patterns.md](./files-transfer-patterns.md) - public files API
  and operation-native transfer patterns over NATS
- [kv-resource-patterns.md](./kv-resource-patterns.md) - KV naming, keys, TTLs,
  and projections
- [store-resource-patterns.md](./store-resource-patterns.md) - service-owned
  blob store resource patterns and runtime semantics
- [type-system-patterns.md](./type-system-patterns.md) - schemas, validation,
  `Result`, and errors
- [service-development.md](./service-development.md) - service layout,
  lifecycle, and jobs vs operations usage
- [testing-patterns.md](./testing-patterns.md) - smallest-real-boundary testing,
  live discovery, and unit-test boundaries
- [observability-patterns.md](./observability-patterns.md) - health, stats,
  docs, telemetry, and request correlation
- [frontend-svelte-patterns.md](./frontend-svelte-patterns.md) - Svelte frontend
  guidance
- [capability-patterns.md](./capability-patterns.md) - capability naming and
  deployment policy patterns
