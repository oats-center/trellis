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

The service runtime publishes baseline samples through the native Health
`Heartbeat` event with ordinary SessionEvent signatures. Its logging and telemetry are configurable;
application logging and tracing outside that runtime remain application-owned. A
service can declare its own health or statistics RPCs if useful, but those
domain surfaces are not automatically added to its API or required by Trellis.

Provisioned devices use the same event. Auth assigns the runtime-owned
`trellis.health.v1:health.report` capability through ordinary instance provisioning
and requires it during native credential selection. Contract authors need not
declare this internal runtime dependency. Users and application OAuth grants
cannot select this publishing right, even through administrative assignment.

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
- baseline heartbeats include runtime metadata, instance identity and publish
  interval; TypeScript heartbeats also list a built-in `nats` connectivity
  check, which Rust leaves out because an arriving heartbeat already proves the
  connection
- service heartbeats carry the lifecycle `phase` (`connecting`, `serving`,
  `stopping`); the SDK publishes a sample at once on every phase change, so a
  watcher sees an instance move without waiting for the interval
- `service.health.setInfo(...)` and `service.health.add(...)` (Rust
  `service.health().set_info(...)` and `.add(...)`) extend service heartbeat
  payloads at publish time using callback-based state snapshots; the same
  helper surface is also available on device connections
- each check reports `ok`, `degraded` (working with reduced capacity or
  quality) or `failed`. The participant's status follows from its checks by
  one rule in both SDKs: any `failed` check makes it `unhealthy`; otherwise any
  `degraded` check makes it `degraded`; otherwise it is `healthy`. Built-in
  checks count like any other, so one failing check reads the same in a Rust
  and a TypeScript service however many other checks pass. The heartbeat
  carries this as `reportedStatus`, and the Health projection's
  `effectiveStatus` is that status until the heartbeats stop, when it becomes
  `offline`. Across a deployment's instances the worst status wins: any
  unhealthy instance makes it unhealthy, and a degraded or offline instance
  degraded
- heartbeat samples are signed native events, not a raw monitoring channel;
  Console reads the Rust-owned health projection through `Health.Query`,
  `Health.Inspect`, and `Health.Metrics`, then uses `Health.Watch` as a
  post-commit invalidation live
- `Health.Storage` reports each runtime data class's disk use, record count,
  oldest record, and retention policy; see
  [data-retention.md](./data-retention.md)

### Live Observation Telemetry

The live-observation contract requires Live observations and Operation watchers to be
observed as live sessions through bounded instruments in both languages:
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

- publishers use the generated native Health `Heartbeat` descriptor and ordinary
  signed-event publication. The complete payload is integrity-bound.
- Auth grants only the exact Heartbeat publish action automatically, never
  arbitrary Health or domain actions. Catalog remapping of the automatic
  capability fails closed.
- the collector verifies whole-capability/catalog evidence, publisher signature,
  original broker chronology and signed retirement cutoff through the shared
  authorization owner. It accepts only provisioned service/device bindings and
  matches payload identity against authenticated deployment/instance identity.
  `trellis.nativeParticipant.v1` in the signed authority extensions retains the
  authoritative participant ID, installed revision and digest; payload claims
  do not establish contract identity.
- `TRELLIS_HEALTH` sources only the generated Heartbeat subject from the ordinary
  raw event stream, avoiding overlapping NATS subject capture. It uses file storage, limits
  retention, a default 24-hour maximum age, a default 1 GiB maximum size, and no
  inactive threshold on projector durables. Health-only mode owns the raw stream
  but does not require an Events SQL store or Events projector.
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
- Events stores full NATS-valid payloads
- Events keeps recorded events for `[events] history_retention_days`, default
  90 days, longer than the event stream's 7. Removal follows
  [data-retention.md](./data-retention.md): one stream-sequence prefix by
  recording time, and a removed sequence is never recorded again
- Events projector and retention loops are singleton runtime loops coordinated
  with NATS KV leases
- the projector tracks the highest stream sequence it has received separately
  from contiguous progress (every sequence up to it is projected or settled).
  An event it could not verify yet is negatively acknowledged and stays
  pending; projecting later events past it is not a gap, and its redelivery
  fills its place. `Events.Diagnostics` reports `catchingUp` while such an
  event waits or the broker holds undelivered events
- history is marked incomplete (`gapDetected`, with `completeSince`) only on
  broker evidence: an unprojected sequence the stream no longer holds because
  it expired, was purged, or was deleted. A sequence the stream still holds is
  pending, and one holding a subject outside `events.v1.>` is skipped. The
  marker is durable and never cleared by later projection. History removed by
  the retention policy is not missing: `prunedBefore` reports it, and once the
  removed prefix covers every missing sequence `gapDetected` is false

Stats example, for an RPC named `Graph.Stats`:

```ts
await service.handleGraphStats(async () => {
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
- An unexpected failure's cause stays in the service's log. Replies and failed
  Operation snapshots carry only the error `id`, `traceId` and
  `context.requestId`, which the log line repeats; see Failure Log Lines.
- if a runtime has not installed an OpenTelemetry tracer provider, RPC error
  responses should still attach `traceId` from a valid inbound `traceparent`
  header before the error leaves the server span boundary

## Failure Log Lines

Every failed request, Operation, job attempt, event delivery and webhook
request is logged once, in one structured line, by the process that failed.
The error ID is generated once (a ULID in both SDKs) and put into both the
caller's error and that line, so the reference a caller shows finds the line.

Both SDKs and `trellis-server` use the same field names, so one query works
across languages:

| Field              | Content                                                                 |
| ------------------ | ----------------------------------------------------------------------- |
| `error_id`         | The `id` of the error the caller received                               |
| `request_id`       | The request's `request-id`, or a job's inherited request ID             |
| `trace_id`         | Inbound W3C trace ID, else the handling span's when traced              |
| `route`            | RPC method, Operation, Live, event or job type                          |
| `service`          | Service name of the failing runtime                                     |
| `caller_principal` | Authenticated caller's principal ID, when known                         |
| `caller_session`   | Authenticated caller's authorization session ID, when known             |
| `error_type`       | Wire error type the caller received                                     |
| `code`             | Declared error code, when the error has one                             |
| `error`            | The error's message                                                     |
| `cause`            | Cause chain (TypeScript: list of `{type, message}`, plus `cause_stack`) |

Surface-specific references are logged as extra fields: `operation_id`,
`job_id`, `event_id` (the event's `header.id`), `delivery_id` and
`attempt_id` for webhooks. Credentials, proofs and session keys are never
logged.

Levels: a failure the service caused (`UnexpectedError`, including a
contract's own `...::UnexpectedError`) logs at `error`; an authorization
refusal at `warn`; a declared outcome or invalid input at `info`. A Rust
`DeclaredRpcError` that stands in for an internal failure carries its cause
with `with_cause`, which only the log line repeats.

Console shows an unexpected failure as `Something went wrong on the server.
Reference: <error_id>`; the CLI and Rust callers end the error text with
`Reference: <error_id>`.

HTTP requests to `trellis-server` (OAuth, the Portal, service and device
enrollment, and the other routes of the Trellis listener) each get a ULID when
they arrive. A handler that fails the request on the server's side records
what it could not do and why, and answers with that ID as the reference; once
the response is ready, one line is logged at `error` with `error_id` and
`request_id` both set to the ID, `trace_id` from an inbound `traceparent`,
`route` (`<method> <path>`, never the query), `service` = `trellis`, `status`,
`error_type` = `UnexpectedError`, `code` (the code the caller got), `error` and
`cause`. Refusals and invalid requests are not server failures and log no such
line. The caller sees the reference without changing a standard error code:

- OAuth `server_error` responses put `Something went wrong on the server.
Reference: <id>` in `error_description`.
- Enrollment and identity-status errors carry it as `error.reference` in the
  `{"error": {"code": ...}}` envelope, which the Rust and TypeScript clients
  end their error text with.
- Portal pages that fail by redirect add `reference=<id>` to the page URL, and
  an OIDC sign-in Trellis could not start or finish stores it with the refusal
  the sign-in page shows.
- Every such response has the `Trellis-Reference: <id>` header.

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
