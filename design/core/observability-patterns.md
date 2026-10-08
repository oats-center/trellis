---
title: Observability Patterns
description: Health, stats, documentation, tracing, and request-correlation patterns for Trellis services.
order: 60
---

# Design: Observability Patterns

## Prerequisites

- [trellis-patterns.md](./trellis-patterns.md) - Trellis architecture and
  communication model
- [type-system-patterns.md](./type-system-patterns.md) - Result and error
  conventions

## Scope

This document defines Trellis runtime observability and contributor conventions
for Trellis-owned code. Documentation style and general logging/tracing choices
are not requirements for downstream applications. Trellis logger adapters and
wire correlation fields still follow their public interfaces when used.

Public API documentation explains behavior that signatures cannot: ownership,
validation and commit points, cancellation and drop semantics, backpressure, and
failure cleanup. Do not add lint-satisfying prose that only repeats an item or
field name. An obvious field may use a short semantic phrase; a public operation
must document the guarantees callers need to use it safely.

## Service Observability

The service runtime publishes baseline heartbeat samples through the private
Trellis health transport. Its logging and telemetry are configurable;
application logging and tracing outside that runtime remain application-owned. A
service can declare its own health or statistics RPCs if useful, but those
domain surfaces are not automatically added to its API or required by Trellis.

Provider admission has three production metric families:

| Metric                                 | Meaning                                                                                                                                            |
| -------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| `trellis.service.admission.inflight`   | Occupied dispatch slots, labeled by `trellis.kind`: `request`, `control`, or `verification`.                                                       |
| `trellis.service.admission.bytes`      | Retained inbound message bytes for those pools; excludes decoded heap and NATS buffers.                                                            |
| `trellis.service.admission.rejections` | Refusals/drop decisions, labeled by original pool kind and limiting reason: `requests`, `bytes`, `verification_requests`, or `verification_bytes`. |

These meanings are shared by TypeScript and Rust. A refusal count does not imply
a busy reply was sent: reply proof and authorization still have to pass. The
existing `trellis.rpc.server.inflight` follows unary dispatch instrumentation,
which may include refusal validation; it is not an application-only concurrency
count, all shared reservations, or transport queue occupancy.

The opt-in TypeScript worker trial additionally exposes
`trellis.auth.worker.pending` (queued plus executing verification attempts) and
`trellis.auth.worker.payload_bytes` (copied payload bytes held for executing
worker attempts), labeled by `trellis.kind`: `ordinary`, `control`, or
`refusal`. These do not replace admission metrics or measure total heap, WASM
memory, or broker buffers. Only executing work has an additional payload copy.

`trellis.auth.worker.active` counts ready, healthy workers by `trellis.kind`
(`ordinary` or `reserved`). `trellis.auth.worker.failures` counts worker
failures by that kind and startup/execution phase; intentional shutdown is not a
failure. `worker.start` in the verification duration family records successful
worker startup latency. Adaptive growth retains these production metrics rather
than adding test-only counters.

Request verification also records phase durations in
`trellis.auth.verification.duration`, labeled by the same `trellis.kind` and
`trellis.phase`. `worker.queue` measures admitted scheduler wait; `worker.copy`
measures dispatch preparation and the exact payload copy; `worker.execute`
measures worker-local processing and contains `worker.verify` (the complete WASM
proof-verification call). `worker.boundary` is the main-thread send-to-result
duration minus worker-local processing: it includes serialization, delivery, and
event-loop scheduling, not just an IPC syscall. Durations use monotonic clocks
within each thread; no absolute cross-thread clocks are compared.
`worker.postcheck` measures successful main-cache and Rust freshness checks
after the result; `inline.verify` and `inline.postcheck` measure the
corresponding inline path. Execution and verification overlap and must not be
added together. Postcheck samples currently describe successful dispatch
verification only.

Activated devices publish through the same private health transport. Heartbeat
publishing is a runtime protocol grant, not a contract dependency or event
surface, so contract authors do not declare it.

Example payload for an application-declared health RPC:

```ts
const service = await TrellisService.connect({
  trellisUrl: config.trellisUrl,
  participant: participants.graph.participant,
  seed: config.seed,
  name: "graph",
}).orThrow();

service.health.setInfo({
  version: build.version,
  info: { region: config.region },
});

service.health.add("db", async () => ({
  status: (await db.ping()) ? "ok" : "failed",
  info: { engine: "postgres" },
}));
```

Heartbeat behavior:

- `TrellisService.connect(...)`, Rust `TrellisClient::connect_service(...)`,
  `TrellisDevice.connect(...)`, and Rust `TrellisClient::connect_device(...)`
  publish baseline samples automatically after authenticated bootstrap
- baseline heartbeats include runtime metadata, instance identity, publish
  interval, and a built-in NATS connectivity check
- `service.health.setInfo(...)` and `service.health.add(...)` extend service
  heartbeat payloads at publish time using callback-based state snapshots; the
  same helper surface is also available on device connections
- heartbeat samples are not Trellis events and are not exposed as a public live
  live; Console reads the Rust-owned health projection through `Health.Query`,
  `Health.Inspect`, and `Health.Metrics`, then uses `Health.Watch` as a
  post-commit invalidation live

### Live Observation Telemetry

The live-observation contract requires Live observations and Operation watchers
to be observed as live sessions through bounded instruments in both languages:
`trellis.live.sessions` (phase
`prepared`/`activating`/`active`/`draining`/`closing`), `trellis.live.ends`,
`trellis.live.handshake.duration`, `trellis.live.buffered.bytes`,
`trellis.live.frames`, `trellis.live.rejections`, and
`trellis.live.cleanup.pending`. They must move on the manager's real lifecycle
transitions, not telemetry-only callbacks: a provider becomes active when
activation commits and a consumer when its first pulse acknowledgement verifies;
prepared or failed offers are not active executions; and a session ends exactly
once per committed terminal outcome. A running Operation with zero observers
retains one unchanged execution lifetime, and opening or closing a watcher never
changes Operation execution counts. `trellis.live.cleanup.pending` decrements
only on actual source termination, so a zero active-session gauge with nonzero
pending is still a visible leak. All seven families are emitted by the same
endpoint record that owns the local state, and each session reports its
`trellis.kind` as `standalone` or `operation`.

Labels stay bounded: `trellis.kind` (`live`/`operation_watch`), `trellis.side`
(`consumer`/`provider`), `trellis.phase`, fixed `reason` and rejection codes,
and `send`/`receive` direction. Raw subjects, session and Operation IDs,
principal or deployment identities, digests, inputs, and free-form error strings
are never metric dimensions. Telemetry never keeps a session alive and never
closes a healthy one.

### Runtime Health And Events Views

The Rust runtime has first-class `health` and `events` subsystems. In all-in-one
mode both run with the platform and jobs subsystems. In split mode, operators
run `trellis-server health` for health projection and may omit
`trellis-server events` when projected event capture is not wanted.

Health subsystem rules:

- publishers send samples to
  `health.v1.heartbeat.<kind>.<contract>.<digest>.<deployment>.<instance>.<session>`;
  identity components other than kind and session are unpadded base64url UTF-8
  tokens
- Auth grants each authenticated service or device exactly one matching publish
  subject. The projector treats this subject identity as authoritative and
  rejects payload identity mismatches.
- `TRELLIS_HEALTH` captures `health.v1.heartbeat.>` with file storage, limits
  retention, a default 24-hour maximum age, a default 1 GiB maximum size, and no
  inactive threshold on projector durables
- JetStream ingress time is canonical for freshness. Publisher sample time is
  retained only as diagnostic data. A participant becomes offline at
  `observedAt + 2 * publishIntervalMs`.
- the health store retains only latest instance state, status intervals,
  five-minute metric buckets, bounded rejection diagnostics, and a transition
  outbox; it does not retain one SQL row per raw sample
- health projection is independent from Events storage; it must not depend on an
  Events store to answer latest or freshness queries
- health stores bounded history according to runtime config, with a default of
  30 days when not overridden
- health projector and retention loops are singleton runtime loops coordinated
  with NATS KV leases
- every committed projection change increments a monotonic revision and
  publishes cross-process invalidation; RPC responses include that revision and
  projection completeness diagnostics
- only meaningful effective-status transitions publish the durable
  `Health.StatusChanged` event on the normal event stream

Events subsystem rules:

- Events captures Trellis-owned event subjects under `events.v1.>` and stores
  queryable metadata plus raw payloads for those events
- jobs lifecycle and worker-presence subjects are jobs subsystem stream traffic,
  not initial Events input
- Events stores full NATS-valid payloads unless a later explicit storage or
  retention policy defines a different bound
- Events stores bounded history according to runtime config, with a default of 7
  days when not overridden
- Events projector and retention loops are singleton runtime loops coordinated
  with NATS KV leases

Stats example:

```ts
await service.handle.rpc.graph.stats(async () => {
  return Result.ok({
    users: { count: await db.countUsers() },
    partners: { count: await db.countPartners() },
  });
});
```

## Documentation

Exported functions, classes, and methods require JSDoc.

Required fields:

- brief purpose description
- `@param` for each parameter
- `@returns` description
- `@throws` or `@errors` for error conditions
- `@example` for complex usage

Skip JSDoc for private helpers when the code is self-evident and for tests.

## Tracing

`TrellisService.connect()` initializes OpenTelemetry automatically using the
service name.

Span naming:

- RPC client: `rpc.client.<MethodName>`
- RPC server: `rpc.server.<MethodName>`
- Event publish: `event.publish.<Domain>.<Action>`
- Event handle: `event.handle.<Domain>.<Action>`
- Job handle: `job.handle.<service>.<queue>`

Required attributes:

- `rpc.system`
- `rpc.method`
- `messaging.destination`

Library support rule:

- libraries performing I/O must accept trace context, create child spans, and
  propagate context
- `TrellisError` subclasses should include `traceId` when tracing is active
- if a runtime has not installed an OpenTelemetry tracer provider, RPC error
  responses should still attach `traceId` from a valid inbound `traceparent`
  header before the error leaves the server span boundary

## Request Correlation

RPCs and jobs include a `requestId` for correlation and audit. Domain events
carry their own `header.id` and trace context; they do not currently emit a
separate `request-id` NATS header unless they are job lifecycle events.

Rules:

- the client supplies a unique `request-id` for signed RPCs; auth includes it in
  the RPC proof and replay-cache key
- after auth validation, the server may use the request id as correlation
  context but must still treat logs/traces as observability data, not as a
  source of authorization policy
- request IDs propagate across downstream RPC and job flows
- logs and traces include `requestId`

Propagation:

| Context                        | `request-id` value                           |
| ------------------------------ | -------------------------------------------- |
| RPC handler                    | generated on receipt                         |
| RPC response                   | echoed from handler                          |
| Domain event                   | not set; use event `header.id` and trace     |
| Job created from RPC/event/job | inherited when available; otherwise new ULID |
| Job lifecycle event            | copied from `job.context.requestId`          |
| Scheduled or cron job/event    | new ULID for jobs; event `header.id` only    |

Job correlation:

- job creation records `job.context.requestId`, `job.context.traceId`,
  `job.context.traceparent`, and optional `job.context.tracestate`
- if no active trace exists when a job is created, the runtime creates a fresh
  W3C trace context rather than leaving the job untraced
- every job lifecycle publish includes matching `request-id`, `traceparent`, and
  `tracestate` NATS headers when present
- workers expose immutable job context to handlers and create job handling spans
  from that context where the language runtime supports tracing

Auth/admin control-plane correlation:

- built-in auth/admin RPCs follow the same inbound `traceparent` extraction as
  application RPCs
- traced admin errors include the request trace ID in serialized Trellis error
  data so operators can correlate failed control-plane calls with logs and spans
- the integration harness covers both a successful traced `Auth.Sessions.Me`
  call and a traced failing `Auth.Users.Get` call through live NATS/auth-callout

Event deduplication:

- domain events include `Nats-Msg-Id: <event.header.id>`
- JetStream deduplicates within its configured window
- this protects against duplicate publication on retries and reconnects
