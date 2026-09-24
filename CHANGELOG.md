# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Added the publishable `trellis-test` Rust crate: an out-of-process harness
  that starts the released `trellis`/`trellis-server` executables in a private
  sandbox, selects its loopback ports automatically, and exposes real
  bootstrap/login/consent/provisioning helpers for external service and
  app/agent integration tests. It depends only on the published `trellis-rs`
  facade plus a projected copy of the generated administration API.
- Added an optional `[http] bind_address` runtime configuration so the HTTP
  listener can bind a specific address; omitting it preserves the existing
  unspecified IPv4 default.
- Added `AgentLoginChallenge::complete_session`, a storage-free login completion
  that binds a session without writing the default session store and does not
  require administrator privilege. Existing `complete` retains its persistence
  and administrator checks.
- Added `TrellisTestRuntimeBuilder::extra_origin`/`extra_origins` and the
  `trellis init config --extra-origin` flag so a browser app served from another
  origin (for example an operator console on `http://localhost:5174`) can
  complete a portal login against a harness runtime.
- Added `TrellisTestRuntime::admin_username`/`admin_password` accessors and
  `TrellisTestRuntimeBuilder::admin_username`/`admin_password` pinning so
  browser- and portal-driven tests can log in, plus `trellis_url()` accessors on
  `TestServiceIdentity` and `TestClientIdentity`.
- `trellis generate` now emits an explicitly configured `[generate.rust]` or
  `[generate.typescript]` target even when the project root has no matching
  toolchain file, so a TypeScript app participant can emit a Rust projection for
  a Rust test harness (and vice versa). With no target configured, a single
  detected language still defaults to `trellis`, and multiple targets still
  require explicit outputs.
- Added `trellis_test::remove_retained_workdirs` to reclaim stale harness-owned
  sandboxes left by aborted or killed runs. It removes only directories carrying
  this harness's ownership marker, and an age gate leaves currently running
  sandboxes alone.

### Fixed

- Quoted host-local NATS configuration paths so JetStream store and JWT resolver
  directories containing spaces, quotes, or backslashes render safely.
- `trellis-test` retries runtime startup within its bounded attempt budget when
  the server exits with a transient Auth Callout denial (`invalid_auth_token` or
  `authority_unavailable`) while a built-in live provider bootstraps, instead of
  failing the run. A persistent denial still surfaces the original `ServerStart`
  error with the captured server output.

### Documentation

- Documented the external Rust live-testing workflow and the intentional set of
  publishable Trellis crates (`trellis-rs`, `trellis-protocol`, `trellis-test`).

## [0.100.0-rc.1] - 2026-09-23

### Added

- Prepared Trellis 2.0 for its first `0.100.x` release under OATS Center
  stewardship, with native Trellis IDL authoring guidance and joint OATS/Qlever
  contribution credit.
- Added deployment-owned authorization trust initialization and direct-key
  issuer rotation, strict startup validation, generation rollback floors, and
  immutable NATS KV manifest/context history.
- Added snapshot-bound, issuer-signed authorization-context issuance and refresh
  to browser, client, service, and device bootstrap. Rust, TypeScript, WASM, and
  Svelte clients verify/cache contexts and refresh before expiry.
- Added context-bound NATS admission. Auth Callout selects a published context
  by digest, requires NKey/session-key equality, compiles transport permissions
  directly from signed grants and current bindings, and bounds transport JWTs by
  context expiry.
- Added additive V1003 SQLite trust/context state, transactional revocation
  coupling for authorization-relevant mutations, durable revocation publication,
  startup repair, expiry/cleanup janitor, and `trellis-server check` validation
  of config, connectivity, migrations, trust, and registries.
- Added durable complete client trust floors with atomic browser and native
  persistence, Rust/WASM-authoritative TypeScript verification, deterministic
  client-computed refresh scheduling, and registry publication visibility
  checks.
- Added the dedicated `trellis-server` process with explicit user/system
  profiles and opt-in managed local NATS from `PATH`, an exact binary path, or
  the pinned checksum-verified download. Managed NATS ports can be selected for
  parallel local instances. Plain startup uses configured external NATS; the
  `trellis` CLI no longer owns server lifecycle.
- Added trusted login-portal authority policy keyed by portal and participant,
  including recursive capability-group macros, verified OIDC role mappings,
  trusted local registration, atomic provenance, automatic authority
  convergence, and live multi-connection session recovery after policy
  reduction, revocation, restoration, or expansion.
- Added native Trellis source packages for the platform-owned Auth, Core,
  Events, Health, Jobs, and State APIs, with generated TypeScript and Rust
  clients, provider descriptors, semantic permissions, and cursor paging.
- Added Core-owned State, KV, Store, Consumer, and Job Queue resource
  reconciliation, generated client handles, durable file transfer, and
  approval-bound device companion activation.
- Added repository-backed Operations with restart and replica recovery, plus
  Events-owned signed history, Consumer delivery reporting, dead-letter replay,
  and Jobs-owned retry and exhaustion handling.

### Changed

- Made the Rust workspace the repository root and moved current source,
  generated packages, container builds, and release publication to
  `OATS-Center/trellis` and `@oatscenter/*` identities.
- Replaced repository prepare/watch and producer-published generated SDK
  workflows with project-local `trellis.toml`, committed `trellis.lock`,
  lock-stable `trellis install`, consumer-local generated TypeScript and Rust
  packages, and canonical API-only OCI publication.
- Replaced the obsolete TypeScript Trellis runtime image with the Rust
  `trellis-server` image and removed the withdrawn TypeScript runtime source and
  release path.
- Replaced `eventlog-runtime` and handwritten platform contract surfaces with
  `events-runtime` and generated native built-in API packages.

- Local bootstrap now creates an offline authorization root and online issuer;
  runtime configuration never references the root seed.
- Ordinary request and event validation now resolves signed contexts locally by
  digest and uses generated receiver-owned permission metadata.
- Authorization-context restore, install, clear, trust reset, and floor advance
  now mutate durable and process-local state through one coherent update gate.
- Clarified that the deny-all bootstrap JWT is route-selection material bounded
  by session/authority/delegation/context state and a configurable lifetime cap;
  proof-bound context refresh renews and atomically installs it.
- Made public Trellis wire DTO and generated client decoding additively tolerant
  by default while retaining strict signed canonical objects, runtime config,
  authored source artifacts, and intentional internal persistence boundaries.
- Increased local-bootstrap NATS authorization timeout to 30 seconds and apply
  Trellis connection timeouts to initial client dials so the full fail-closed
  admission pipeline remains reliable under concurrent live validation.
- Aligned generated local and runtime-owner lease defaults to 30 seconds with
  5-second renewals so concurrent builds do not evict a healthy server.
- Derived runtime convergence and `trellis-server check` requirements from the
  selected runtime mode. Removed obsolete public infrastructure apply/check
  commands; `trellis infra` now contains only offline trust artifact tooling.
- Kept TypeScript cryptography Rust/WASM-authoritative while supporting
  own-context persistence/refresh/reconnect and the same minimal connected
  registry watch state for TypeScript service providers.
- Require Platform, Jobs, Health, and Event Log to use separate SQLite files;
  runtime config rejects shared subsystem paths and local bootstrap provisions
  one file per subsystem.
- Made the normal `Check` workflow the sole correctness gate, including
  generated freshness and the shared live matrix. Release now verifies only
  metadata, packages, archives, images, and publication inputs from a green
  `main` base.
- The final self-hosted `Check` baseline is approximately 26 minutes. The
  prebuilt live bundle takes 3m57s and Auth is the remaining critical live slice
  at 19m31s; every other semantic live slice completes in under 8 minutes.
- Replaced positional Rust user-session, proof, event, generator, listener, and
  test-fixture APIs with cohesive inputs; Jobs event constructors now live under
  `jobs::events`.
- Removed the obsolete generic `cargo xtask release lane` and `release verify`
  commands in favor of direct package-specific release checks.
- TypeScript provider caches now retain bounded opaque Rust/WASM
  verified-context handles, avoiding repeated trust-chain JSON parsing and
  verification on request and event hot paths.
- Live integration test harnesses no longer require Podman or Docker; they spawn
  the pinned nats-server binary directly. Trellis OCI images bake the pinned
  nats-server during image build instead of downloading it at container runtime.

### Fixed

- Included installed protocol-WASM output in the JSR package.
- Made trust startup monotonic across configured files, SQLite, immutable NATS
  KV history, and the CAS-protected current pointer, including atomic
  removed-issuer context revocation and `trellis-server check` reporting for
  every required store, stream, bucket, trust floor, and credential.
- Added proof-bound nullable context recovery, midpoint server-clock correction,
  renewed route-JWT expiry tracking, atomic client persistence, same-digest
  rescheduling, transactional two-context overlap, and strict revocation
  records.
- Made the Rust validator cache establish watches and consume their initial
  state before Auth Callout starts, recreate them after connection/watch
  failure, resolve exact immutable trust/context records lazily, expose cache
  health, and record admitted context digest in connection presence.
- Defined context size limits uniformly as canonical complete signed-context
  JSON UTF-8 bytes and restored semantic subsystem-parallel TypeScript and Rust
  integration gates.
- Made authorization-context revocation observation monotonic and reconnect
  recovery adopt refreshed dynamic NATS credentials without browser
  reauthentication.

## [0.11.0] - 2026-07-23

### Public Breaking Changes

- Replaced contract-manifest authority at runtime with canonical
  `trellis.api.v1` and `trellis.participant.v1` artifacts, exact participant
  bindings, protocol-derived subjects, and materialized `GrantSet` permissions.
- Moved browser auth, service and device bootstrap, session lifecycle,
  authorization reconciliation, NATS Auth Callout, Auth RPC, and connection
  revocation into the Rust platform runtime. Removed the TypeScript auth owner,
  sentinel credentials, mutable capability groups, grant overrides, and
  `Trellis.Bindings.Get` compatibility surfaces.
- Replaced legacy concatenated bootstrap signatures with purpose-separated,
  canonical session proofs and nonce-bound NATS connection authentication.
  Service, browser, device, CLI, TypeScript, Rust, and WASM clients now use one
  session-key proof protocol.

### Added

- Added protocol-owned API and participant artifact parsing, semantic digests,
  compatibility checks, participant resolution, permission atoms, authority
  needs, proposals, and cross-language contract-to-protocol compilation.
- Added Rust authorization storage and reconciliation with versioned desired and
  materialized authority, entity-owned runtime evidence, effective expiry,
  dependency and resource evidence, deterministic outbox actions, and matching
  in-memory and SQLite repository conformance.
- Added the Rust auth HTTP surface for first-administrator bootstrap, local and
  OIDC browser flows, service and device provisioning, activation review,
  password and identity workflows, portal policy, and unified session creation.
- Added NATS Auth Callout admission with XKey transport, deny-all bootstrap
  credentials, nonce and session-proof verification, replay protection,
  deterministic transport-permission compilation, bounded response authority,
  connection presence, and durable kick intents.
- Added source-owned Auth API, runtime participant, and administration
  participant artifacts; generated TypeScript and Rust SDKs; shared Rust/WASM/
  TypeScript session-proof vectors; and an embedded reproducible Svelte login
  portal.

### Changed

- Made deployment-authority planning one stable lineage per deployment and
  participant, with pending-only semantic deduplication, preserved terminal
  history, deterministic expiry, and initial-plan visibility before acceptance.
- Made server-owned browser consent immutable across flow transitions.
- Updated TypeScript and Rust clients, CLI commands, test harnesses, deployment
  examples, local bootstrap, generated package metadata, documentation, and LLM
  guidance for the Rust-owned, sentinel-free TOML runtime configuration.
- Updated integration infrastructure to provision isolated NATS resources,
  compile protocol artifacts in both languages, run supported suites in
  parallel, and retain explicit deferred coverage for State and Jobs
  administration owners.

### Fixed

- Preserved the accepted Milestone 7 migration byte-for-byte and moved M8 schema
  evolution into a populated, repeatable V1002 upgrade. Added exact
  grant-derived resource/request/event authorization and stable redacted public
  errors.
- Made browser consent server-owned, bound OIDC callbacks to browser possession
  and current portal registration policy, stabilized first-admin restart and
  federated completion, and added explicit first-admin token rotation.
- Unified service/device artifact presentation with compatibility-aware semantic
  authority proposals, preserved optional authority, and made bootstrap replay
  revalidate current issuance before signing fresh credentials.
- Fixed cross-language artifact digest normalization, generated action source
  portability, operation-transfer metadata, feed and reply-inbox subjects,
  JetStream discovery permissions, resource evidence projection, durable
  operation reconnect identity, and operation watch readiness.
- Fixed native and browser NATS nonce-signature encoding interoperability,
  bootstrap proof hashing and clock correction, transient session-validation
  retries, request-proof replay rejection, device activation gating, authority
  expiry staleness, and session-bound reconnect behavior.

## [0.11.0-rc.8] - 2026-07-13

### Fixed

- Made the published control-plane package consume built-in contract manifests
  through the Trellis SDK package exports so JSR can build its self-contained
  module graph.
- Deferred `trellis-svelte` JSR validation until its matching Trellis runtime
  version has been published, preserving ordered clean-break publication.

## [0.11.0-rc.7] - 2026-07-13

### Public Breaking Changes

- Replaced TypeScript owner SDK runtime packages with vocabulary-only generated
  packages and participant-contract-derived caller/provider facades. Removed the
  old generated `api`, `client`, and `contract` surfaces, implicit contract
  uses, low-level connection constructors, and direct binding/resource
  bootstrap.
- Changed TypeScript participant contracts to select explicit RPC, event,
  operation, feed, state, KV, store, and Jobs descriptors. Generated action
  names, handler registration types, and connected runtime access now derive
  only from those selections.
- Replaced runnable Rust owner SDKs and generic runtime clients with
  vocabulary-only owner SDK crates plus materialized, contract-filtered
  participant facades for services, apps, devices, and agents.
- Removed public raw-transport and compatibility surfaces from normal service
  code, including Rust `TrellisClient`, raw NATS/binding access, owner
  `connect_service(...)`, and the `trellis-auth`, `trellis-client`,
  `trellis-service`, and `trellis-service-runtime` compatibility crates.
- Split event consumption into typed live caller subscriptions and generated,
  Trellis-provisioned durable service groups. Service code no longer chooses
  durable consumer names or combines live and durable options in one API.
- Changed public contract object schemas to be open by default. Contracts must
  now set `additionalProperties: false` explicitly when unknown fields are a
  security-sensitive validation error.

### Added

- Added signed event proofs, `Auth.Events.Validate`, and retained publisher
  session history so consumers can verify historical provenance after ordinary
  session expiry or closure.
- Added the optional Rust Event Log service, generated TypeScript and Rust Event
  Log SDKs, and a Console Events workspace for persisted envelopes, publisher
  verification, integrity exceptions, metrics, and durable-consumer health.
- Added a Rust-owned Health heartbeat projection with latest instance state,
  status intervals, five-minute metrics, replay recovery, snapshot/watch APIs,
  and runtime failure supervision.
- Added typed live updates for Jobs, operations, feeds, and event listeners,
  including ordered operation signals and durable operation resume behavior.
- Added Jobs trigger, parent/root lineage, active wait evidence, worker-presence
  projection, job-type metrics, error fingerprints, and related-job inspection.
- Added generated Rust service resource, state, Jobs, event-consumer, operation
  provider, typed publisher, prepared-event, and transfer facades backed by
  bootstrap-resolved bindings and an opaque generated-code ABI.
- Added typed declared RPC and operation errors, provider/caller wire-schema
  validation, safe templated-event subject resolution, nullable tri-state
  support, tagged object unions, and deterministic generation diagnostics.
- Added portable prepare-time Rust owner/participant crates with relative Cargo
  dependencies, embedded contract metadata, compile fixtures for every
  participant kind and enforced Rustdoc coverage.

### Changed

- Updated the Console Jobs workspace around a job-type health matrix, scoped
  throughput and latency charts, execution-story timelines, attempt details,
  lineage, wait edges, related jobs, worker state, and structured Trellis
  errors.
- Updated deployment authority acceptance to commit desired state and return
  while physical reconciliation continues in the background. The Console plan
  workspace now surfaces structured schema/capability/resource breakage and
  keeps the decision action visible during long reviews.
- Updated Health, Jobs, and Event Log ownership so their server-side projection
  and RPC/feed implementations run in Rust behind purpose-scoped runtime ports.
- Updated first-party Rust services, tests, CLI code, and demos to consume
  generated participant facades and generated types directly. Generated service
  runtimes now supervise authenticated handlers, durable listeners, and Jobs
  workers as one lifecycle.
- Moved Jobs and Event Log raw transport needs behind purpose-scoped Trellis
  runtimes; production services no longer borrow NATS from authenticated
  sessions.

### Fixed

- Fixed keyed Jobs queues so processing failures request redelivery, release
  active slots best-effort, keep worker loops alive, and initialize distributed
  key coordination through generated service facades.
- Fixed event subject templates to resolve payload pointers consistently across
  TypeScript and Rust, including zero values, safe-integer enforcement, and
  canonical token escaping.
- Fixed generated event, feed, RPC, operation, and signal payload validation and
  preserved typed declared error data at caller/provider boundaries.
- Fixed deployment authority planning so contributed provided surfaces and safe
  resource changes reconcile materialized permissions, while new grants and
  strict migrations still require approval.
- Fixed Rust service bootstrap to wait for pending deployment authority by
  default, matching TypeScript while retaining an optional overall deadline.
- Fixed generated Rust worker heartbeat identity so Jobs worker presence
  projects correctly.
- Fixed untagged generated Rust object unions so more-specific branches decode
  before open broad branches and do not discard fields.
- Fixed physical resource reconciliation so deleting already-absent streams,
  stores, buckets, or consumers is idempotent.

## [0.11.0-rc.6] - 2026-07-08

### Public Breaking Changes

- Removed the service-hosted Health RPC surfaces from TypeScript and Rust
  service facades. Health remains a Trellis-owned heartbeat/event surface rather
  than a caller-invoked service RPC surface.

### Added

- Added `Jobs.Metrics` and Console job-type diagnostics with health matrix rows,
  scoped throughput/latency charts, queue wait/runtime aggregates, worker
  counts, and error fingerprints.
- Added schema-aware deployment authority plan diffs, structured breaking-change
  records, superseded plan state, and Console rendering for capability, schema,
  resource, surface, docs, and contract-use changes.
- Added browser smoke coverage for login portal session refresh, signed logout,
  consent denial, credential failures, account-link/password flows, and device
  activation outcomes.

### Changed

- Updated deployment authority approval policy so safe updates auto-apply during
  service bootstrap, new capability grants and resource aliases require pending
  approval, strict same-contract migrations require approval, and `mutable-dev`
  records and auto-accepts migrations.
- Updated Jobs worker identity and presence handling so runtime worker subjects
  use the logical service identifier consistently across TypeScript and Rust.
- Updated the Console job detail view with denser operational status, timeline,
  attempt, lineage, related-job, and JSON payload inspection surfaces.
- Updated service-author LLM guidance and design docs for the current Jobs,
  authority, resources, and service-runtime behavior.

### Fixed

- Fixed Console authority-plan state typing to recognize superseded plans.
- Fixed Jobs admin projections so each independent Rust `service-jobs`
  projection uses its own durable consumer instead of sharing stream delivery
  with other projections.
- Fixed Rust Jobs admin cancellation so cancelling an already-terminal job
  returns the current terminal job instead of surfacing an invalid error
  payload.
- Fixed TypeScript jobs so retryable handler failures publish `dead` once the
  job reaches `maxDeliver`.
- Fixed Rust jobs workers so per-message processing or terminal cleanup errors
  request redelivery, best-effort release keyed active slots, and keep the queue
  worker running.
- Fixed device activation UI state so pending operation progress is loaded from
  the current operation snapshot before watching for future events.

## [0.11.0-rc.5] - 2026-07-04

### Public Breaking Changes

- Replaced the Jobs admin list/detail RPCs with the workbench-oriented
  `Jobs.Query` and `Jobs.Inspect` RPCs. Generated TypeScript and Rust Jobs SDKs
  no longer expose `Jobs.List` or `Jobs.Get`.

### Added

- Added the Jobs admin workbench API with filtered/sorted/grouped job queries,
  single-job inspection, keyed-concurrency lookup, live invalidation feed,
  structured error details, trigger/lineage metadata, related jobs, and admin
  action reasons.
- Added Jobs admin Console pages backed by `Jobs.Query`, `Jobs.Inspect`, and
  `Jobs.Watch`, including queue stats, grouping, detail timeline, related jobs,
  and cancel/retry/DLQ actions.
- Added live TypeScript and Rust coverage for the new Jobs admin workbench API,
  terminal job inspection, keyed queue policies, retry-to-dead projection,
  worker presence, and control-plane Jobs admin registration.
- Added live release-gate coverage for account-flow OAuth callbacks, operation
  callback/resume behavior, state conflict and corrupt-entry handling, catalog
  resource binding projection, and login portal browser smoke behavior.

### Fixed

- Fixed state admin deletion so malformed backing state entries can be removed
  without first deserializing the corrupt value.
- Fixed local HTTP OAuth/OIDC account-flow callback handling for provider
  errors, provider mismatch, and successful account linking.

## [0.11.0-rc.4] - 2026-07-03

### Public Breaking Changes

- Removed raw transport escape hatches from TypeScript and Rust runtime,
  service, and client facades so service authors use generated clients,
  `TrellisService.connect(...)` handles, and resource APIs instead of hand-built
  NATS clients, subjects, or envelopes.

### Added

- Added the `Auth.Users.Resolve` RPC, generated TypeScript and Rust client
  surfaces, and control-plane handling so callers can resolve explicit user IDs
  to display labels without browsing the full user directory.
- Added Jobs retry, cancellation, queue-full, dead-letter, stale-worker, and
  max-deliveries advisory coverage across the shared test matrix, TypeScript
  integration suite, Rust integration suite, and Rust Jobs admin service tests.
- Added live TypeScript and Rust acceptance coverage for auth grant overrides,
  identity-grant revocation, portal route and built-in portal guardrails,
  capability groups and last-admin checks, user/identity admin pages, authority
  plan acceptance validation, device activation proof and review scoping, state
  migration-required responses, TTL expiry, and unsafe service-deployment
  removal.

### Changed

- Updated service-author guidance templates to use `<version-tag>` with the
  leading `v`, current `v0.11.0-rc.3` examples, and transport-neutral Rust
  guidance.
- Updated the integration runner to support explicit case selection and matrix
  coverage used by the new JS Jobs and auth/device/state/control-plane cases.

### Fixed

- Fixed local Console login recovery so stale or missing browser session data
  redirects through a fresh auth request instead of leaving users stranded on
  the login page.
- Fixed deployment-authority migration acceptance so plans can remove obsolete
  dependency or provided-surface authority state, and fixed the Console
  authority plans page to use the current `count` response and pending default
  filter.
- Fixed device-review authorization so deployment-scoped
  `trellis.auth::device.review.*` capabilities satisfy review RPCs, delegated
  sessions preserve those scoped capabilities, and connect-info failures return
  stable HTTP status/reason mappings.
- Fixed deployment-grant sessions so reconnect and principal refresh do not drop
  sessions that were intentionally bound without direct user capabilities.
- Fixed auth start-request signature payload canonicalization for JSON contract
  and context presentations.
- Fixed Jobs admin advisory projection parsing and exposed the small parser and
  mapping helpers needed by Rust/TypeScript parity tests.

## [0.11.0-rc.3] - 2026-06-30

### Fixed

- Fixed Trellis SQL outbox migrations so 0.10 SQLite/Postgres outbox tables with
  `event` columns upgrade to the 0.11 `kind`/`name`/`outcome` schema without
  downstream hand patches.

## [0.11.0-rc.2] - 2026-06-29

### Fixed

- Fixed the bootstrap invalid-signature integration test so it corrupts signed
  bytes deterministically instead of changing unused base64url padding bits.
- Fixed Cargo publishing for the public `trellis-contracts` and `trellis-rs`
  crates.

## [0.11.0-rc.1] - 2026-06-28

### Public Breaking Changes

- Split Trellis event runtime metadata from event bodies. Generated SDK event
  body types are now body-only, TypeScript handlers read event id/time from
  listener context or `TrellisEventMessage`, and Rust prepared events use
  `headers`, `event_id`, and `event_time` instead of the old message/header
  helpers.
- Changed caller-owned SQL outbox/inbox storage schemas for the event metadata
  split. Outbox tables now store `headers`, `event_id`, and `event_time`, and
  inbox storage tracks `trellis_inbox_events(event_id)` instead of
  `trellis_inbox_messages(message_id)`.
- Changed Jobs runtime and generated SDK APIs for keyed concurrency. `JobQueue`
  implementations now need `submit(...)`, job results can report not-enqueued
  outcomes, job states and event/process enums include keyed-concurrency
  terminal states, and job/job-binding records include concurrency and
  queue-policy metadata.
- Changed the Rust `trellis-jobs` crate from a thin `trellis-rs::jobs::*`
  re-export into an owning jobs crate, so downstream code that mixed nominal
  `trellis_jobs::*` and `trellis_rs::jobs::*` types may need to standardize on
  one import path.
- Changed the public deployment authority protocol so proposal and desired-state
  `needs` are grouped by `contracts`, `surfaces`, `capabilities`, and
  `resources`, and materialized authority `grants` are grouped by
  `capabilities`, `surfaces`, and `nats`. The TypeScript
  `DeploymentAuthorityNeed` union was replaced by `DeploymentAuthorityNeeds` and
  family-specific need types.
- Renamed the runnable Trellis control-plane service JSR package from
  `@oatscenter/trellis-service-trellis` to `@oatscenter/trellis-control-plane`
  and now publishes it directly from `js/services/trellis` instead of a
  generated staged package tree.
- Changed `TrellisTestRuntime.start(...)` so callers must provide an explicit
  `trellis.command`; removed the default Trellis runtime package resolution and
  the `trellis.binary` option.
- Changed TypeScript SQL outbox service APIs from manual repository/dispatcher
  wiring to `service.withSqlOutbox(...)` and handler-injected
  `outbox.transaction(...)`.
- Moved durable SQL event enqueue from direct
  `SqlOutboxRepository.enqueue(prepared)` service-author usage to
  transaction-scoped typed `event.*.*.enqueue(...)` inside
  `outbox.transaction(...)`.
- Changed Trellis SQL outbox/inbox helper-table schema ownership. Trellis now
  owns versioned migration artifacts; services own migration execution, database
  lifecycle, table names, and transaction boundaries.
- Changed Drizzle outbox guidance. `createDrizzleSqlExecutor(...)` remains an
  advanced helper, but the main service-author flow is generic
  `withSqlOutbox(...)`; direct `withSqlOutbox({ drizzle })` sugar is deferred
  and not exposed.
- TypeScript extracted service handlers now use concrete handler aliases from
  the generated SDK after running `trellis install`.
- Removed handler dependency injection from Trellis runtime internals and
  generated service surfaces in favor of concrete generated handler aliases and
  runtime-owned handler context.

### Added

- Added keyed Jobs concurrency and queue-depth policy support for contract job
  queues, including TypeScript and Rust runtime APIs, generated SDK metadata,
  and the `Jobs.GetKey` admin RPC for key-specific job projection lookup.
- Added public TypeScript event metadata types, including `TrellisEventHeader`
  and `TrellisEventMessage`, for metadata-aware event handling after event
  bodies stopped carrying runtime headers.
- Added the TypeScript `@oatscenter/trellis/service/drizzle` helper for Drizzle
  SQL-backed outbox/inbox tables.
- Added the Deno-first `@oatscenter/trellis-test` JSR package for service
  boundary integration tests that need an isolated NATS/JetStream environment
  and a spawned Trellis control-plane process.
- Added the internal `@oatscenter/trellis/host/control-plane` export used by the
  Trellis control-plane service package to access runtime host primitives
  without repo-relative package imports.
- Added `service.withSqlOutbox(...)` for SQL outbox-backed service wrappers.
- Added handler-injected `outbox` for RPC, feed, operation, event-listener, and
  job handlers.
- Added transaction-scoped typed `event.*.*.enqueue(...)` for durable SQL event
  enqueue.
- Added `getSqlOutboxMigrations(...)` for Trellis-owned SQL outbox/inbox
  migration artifacts.
- Added Drizzle SQL transaction helper types and functions:
  `DrizzleSqlTransactionRunner`, `DrizzleSqlOutboxOptions`, and
  `runDrizzleSqlTransaction(...)`.
- Added Rust and TypeScript typed operation error handling, including generated
  operation error payload surfaces and preserved source operation descriptor
  types.
- Added JSON Schema runtime validation parity in Rust and
  `SchemaValidationError` UX metadata for schema validation failures.
- Added OpenTelemetry duration metrics for Trellis RPC, jobs, operations,
  events, feeds, transfers, auth, service lifecycle, and runtime helper paths.
- Added the Rust `trellis-runtime` and `trellis-bootstrap` crates, including the
  `trellis-server` runtime entrypoint, bootstrap config generation, runtime
  storage migrations, and subsystem scaffolds.
- Added generated Jobs admin service registration plus live projection RPC
  handlers for listing, reading, and cancelling jobs.
- Added live decoded event capture to `@oatscenter/trellis-test` through
  `TrellisTestRuntime.captureEvents(...)` and `TrellisTestEventCapture`, so
  integration tests can subscribe to selected contract events with normal
  Trellis authority and generated event facades.
- Added `@oatscenter/trellis-test` assertion helpers for RPC results, eventual
  RPC success, captured event presence and context, no-event windows, and
  terminal job and operation completion.
- Added expanded JS and Rust integration coverage for granular matrix cases,
  operations cancellation/signalling, event consumers, control-plane jobs admin,
  prepared events, outbox flows, state admin, and authority planning.
- Added service-repo integration helper surfaces and documentation updates for
  Trellis test harness workflows.
- Added live TypeScript and Rust auth integration coverage for bootstrap client
  branches, generated Auth session RPCs, request validation, session and
  connection metadata, and revoke/logout cleanup paths, replacing fake runtime
  unit coverage.
- Added narrow `trellis-test` live-runtime helpers for one-shot logout kick
  failures and raw auth connection-presence seeding.

### Changed

- Changed generated TypeScript service handler aliases into concrete generated
  SDK function aliases with an optional `TDeps` generic. Generated `client.ts`
  service registration surfaces consume those aliases directly, inline handlers
  still infer from `service.handle...`, and handler-side source
  `defineError(...)` instances are accepted when their serialized data matches
  the declared generated error data.
- Changed release verification and publishing so the Trellis control-plane
  service, `@oatscenter/trellis-test`, and direct JSR packages run through the
  normal release package set.
- Changed release retry and publish workflows so manual existing-tag retries can
  publish after successful release gates, publish jobs still run when unrelated
  dependencies are skipped, and internal JSR dependencies are rewritten to the
  release candidate version during release preparation.
- Updated release CI and image builds to Deno 2.8.2 so the Trellis image release
  gate uses the current runtime for Vite/Tailwind portal builds.
- Scoped release artifact matrix jobs to tagged release runs and fixed Rust
  integration test execution in release verification.
- Reworked JS integration execution around a shared test matrix and granular
  fixture selection, replacing the old client test matrix file and shared
  runtime protocol helpers.
- Changed TypeScript internal Trellis runtime hooks to thread bootstrap-resolved
  durable event consumer bindings without module-level mutable state.
- Changed contract digest normalization so TypeScript preserves operation error
  declarations consistently with Rust and the shared conformance vector.
- Preserved deployment compatibility mode while authority and runtime validation
  paths moved to the new live integration coverage.

### Fixed

- Fixed stale prior service offers during bootstrap and repeated browser auth
  handoffs/flow-id redirects.
- Fixed authority query and path handling bugs discovered after `0.10.22`.
- Fixed Rust TLS provider setup for reqwest/rustls and the Jobs service.
- Fixed release-managed versions back to `0.11.0` before cutting this release
  candidate.
- Fixed release bootstrap from clean checkouts so Rust workspace metadata no
  longer requires generated Cargo SDK packages before prepare runs.
- Fixed release gates for Trellis CLI artifact packaging, demo prepare locks,
  package test fakes, npm SDK smoke tests, Rust admin bootstrap integration, and
  publishable control-plane service imports and JSR dry-runs.
- Fixed duplicate workspace dependency metadata.
- Fixed generated operation handler typing, source operation descriptor typing,
  and generated SDK capability types.
- Fixed durable Rust service event consumers so handler errors NAK the delivered
  message and keep the shared pull loop alive for later redelivery.
- Fixed Jobs admin projection lookup to use an id index instead of scanning all
  projected jobs for `Jobs.Get` and `Jobs.Cancel`.
- Fixed Jobs admin `since` filtering and sorting to compare parsed timestamps
  instead of lexicographic date strings.
- Fixed Rust workspace lint configuration to use the supported `missing_docs`
  lint and keep new Rust runtime/bootstrap surfaces documented.
- Fixed bootstrap CLI reporting and `trellis init config --format json` output,
  while preserving structured `BootstrapError` diagnostics.
- Fixed `Auth.Sessions.Logout` cleanup ordering so durable session and
  connection records are removed before runtime access is kicked.
- Fixed `@oatscenter/trellis-test` assertion helpers so generated event captures
  from `TrellisTestRuntime.captureEvents(...)` and generated service job refs
  can be passed directly to `assertEventCaptured`, `assertEventsCaptured`, and
  `assertJobCompleted` without downstream casts, wrappers, or local adapters.
- Fixed `@oatscenter/trellis-test` live integration helpers so service approval
  and generated-client connection flows can run against the release candidate.

### Removed

- Removed the generated `js/services/trellis/jsr` service-package staging path
  and its `prepare:jsr` workflow.

## [0.10.22] - 2026-06-25

### Changed

- Split service-author AI guidance into shared, TypeScript-specific, and
  Rust-specific AGENTS templates so service repos can copy the smallest relevant
  guidance set.

## [0.10.21] - 2026-06-25

### Fixed

- Rebuilt and republished `@oatscenter/trellis` so the npm
  `@oatscenter/trellis/auth/browser` subpath includes `completeSessionLogout`,
  and added npm smoke coverage for that export.
- Fixed the Trellis service image build by freezing the login portal static
  build to `js/deno.lock` and using Deno 2.8.3 for the portal build stage.

## [0.10.20] - 2026-06-24

### Added

- Added the exported `CursorPageResponseSchema<TItem>` TypeBox return type for
  `CursorPageSchema(...)` so service contracts can name cursor-page response
  schemas cleanly.
- Added public API regression coverage for root contract helpers and retained
  auth, browser auth, device, service, and generated SDK subpath exports.

### Changed

- Clarified TypeScript package guidance so ordinary `define*Contract(...)`
  imports use the browser-safe `@oatscenter/trellis` root, with advanced
  contract tooling and runtime-specific helpers kept on explicit subpaths.
- Updated Trellis service, console, and demo TypeScript imports to use the
  browser-safe root for normal contract helpers and shared JSON/schema helpers.
- Updated demo workspace import maps and Vite aliases for the Trellis browser,
  device, service, and error subpaths used by the local examples.

## [0.10.19] - 2026-06-24

### Added

- Added signed HTTP browser logout for Trellis user sessions, including
  session-key logout proofs, validated post-logout return targets, optional
  OIDC/Auth0 provider logout redirects, browser logout helpers, Console sign-out
  integration, and service-author guidance updates.

### Changed

- Changed `Auth.Sessions.Logout` into terminal Trellis session revocation only;
  browser apps should use the signed HTTP logout helpers for provider logout
  instead of calling the active app connection RPC.

## [0.10.18] - 2026-06-22

### Changed

- Changed the public deployment authority protocol so proposal and desired-state
  `needs` are grouped by `contracts`, `surfaces`, `capabilities`, and
  `resources`, and materialized authority `grants` are grouped by
  `capabilities`, `surfaces`, and `nats`. The TypeScript
  `DeploymentAuthorityNeed` union was replaced by `DeploymentAuthorityNeeds` and
  family-specific need types.
- Added browser auth recovery classification helpers and Svelte provider hooks
  so browser apps can handle stale sessions, expired auth flows, and
  auth-required reconnects by restarting sign-in instead of showing terminal
  connection errors.

### Fixed

- Repaired stale or obsolete persisted materialized-authority projections
  through storage upgrade and reconciliation while preserving the rule that
  runtime permissions require current materialization.
- Fixed stale Trellis browser sessions and expired browser login flows across
  the Console, login portal, and Svelte provider surfaces so recoverable auth
  failures clear local session state, return users to sign-in, and avoid brief
  intermediate loading-card flicker during callback handoff.
- Fixed OAuth browser callbacks so flows with already-satisfied approval and
  capabilities redirect directly back to the app instead of forcing another
  login-portal approval step.
- Fixed login portal route management so disabled selectors can be reclaimed by
  a different portal, and Console route edits can update selector fields by
  replacing the old selector after the new route is saved.
- Fixed capability and authority displays by de-duplicating capability catalog
  entries, counting grouped materialized grants correctly in Console, and hiding
  raw capability keys from the login portal's primary insufficient-access copy
  when no capability metadata is available.

## [0.10.18-rc.1] - 2026-06-13

### Changed

- **Breaking:** Changed the public deployment authority protocol so proposal and
  desired-state `needs` are grouped by `contracts`, `surfaces`, `capabilities`,
  and `resources`, and materialized authority `grants` are grouped by
  `capabilities`, `surfaces`, and `nats`. The TypeScript
  `DeploymentAuthorityNeed` union was replaced by `DeploymentAuthorityNeeds` and
  family-specific need types.

### Fixed

- Repaired stale or obsolete persisted materialized-authority projections
  through storage upgrade and reconciliation while preserving the rule that
  runtime permissions require current materialization.

## [0.10.17] - 2026-06-12

### Fixed

- Fixed the `@oatscenter/trellis` npm root export so browser bundlers select the
  browser-safe entrypoint without app-level Vite aliases, preventing DNT and
  Node/Deno builtin shims from leaking into browser client bundles.
- Fixed Trellis contract catalog startup and lookup behavior so invalid cached
  manifests are pruned from the SQLite `contracts` cache, stale derived
  projections no longer block manifest hydration, active implementation offers
  remain untouched when their cached manifest is missing, and presenting a full
  valid manifest repairs a corrupt same-digest cache row.

## [0.10.16] - 2026-06-12

### Fixed

- Fixed Trellis browser client and Svelte provider startup under strict Content
  Security Policy by removing `new Function` and eval-like dynamic import probes
  from browser-reachable runtime detection, transport loading, and telemetry
  helpers, keeping browser clients on the websocket transport path without
  requiring `unsafe-eval`.
- Fixed Rust generated SDK compile checks against current registry resolution by
  pinning the `time` dependency below the patch that conflicts with the
  `async-nats` JetStream error type blanket implementation.

## [0.10.15] - 2026-06-11

### Changed

- Changed generated TypeScript package import maps for out-of-tree SDK shells to
  depend on the JSR Trellis runtime package instead of the npm package.
- Exposed service event publishing on bound TypeScript service clients so
  generated service event surfaces can publish prepared Trellis events.

### Fixed

- Fixed release JSR publish availability checks to probe exact immutable version
  metadata instead of the lagging package index used by `deno info`.
- Fixed the trellis-svelte JSR dry-run artifact to use a compatible published
  Trellis runtime range instead of assuming the previous patch exists on JSR.

## [0.10.14] - 2026-06-11

### Added

- Added grouped event consumer declarations so service contracts can declare
  dependency event subscriptions by use alias with `uses` and consume their own
  events with `self`, with manifest validation, resource planning, contract
  proposal analysis, design docs, and service-author AI guidance updated for the
  grouped form.
- Added package artifact smoke coverage for the Trellis browser graph and the
  `@oatscenter/trellis-svelte` package output, including declaration files,
  public export declarations, and JSR publish targets.
- Added Auth0 organization support to OIDC provider configuration and login
  routing.

### Changed

- Changed the Trellis browser package entrypoint to export an explicit
  browser-safe public surface instead of re-exporting the full root package.
- Hid raw runtime transport handles from public TypeScript and Rust service,
  client, device, jobs, event, transfer, and package declaration surfaces,
  keeping low-level NATS access behind internal APIs and using curated
  connection/status APIs for public consumers.
- Changed release publishing to dry-run and publish the prepared
  `@oatscenter/trellis-svelte` JSR package artifact alongside the existing
  staged JSR packages.
- Updated the release guide to preserve release marker branches after
  publication.
- Refreshed built-in capability definitions and admin capability projections so
  deployment authority planning and Console capability-group editing use the
  current materialized capability metadata.

### Fixed

- Fixed browser npm artifacts so the browser graph excludes DNT polyfills and
  Node-only shims, including environment detection paths that need to remain
  safe in bundled browser builds.
- Fixed `@oatscenter/trellis-svelte` package builds to emit declaration files,
  compiled JavaScript component output for JSR, self-type directives, rewritten
  Svelte component imports, and the runtime dependency metadata needed by
  consumers.
- Ignored generated package declaration outputs from local Trellis package
  builds so release and package smoke checks do not leave untracked `.d.ts`
  files behind.
- Fixed expired browser auth callbacks so stale callback state is reported
  cleanly instead of reusing an invalid browser flow.

## [0.10.13] - 2026-06-10

### Added

- Added language-specific LLM guidance files for TypeScript services, Rust
  services, and Svelte browser apps that consume Trellis.

### Changed

- Updated service-development AI guidance and the service `AGENTS.md` template
  to point agents at focused Trellis service, browser-app, and verification
  rules.

### Fixed

- Fixed Trellis contract event subject parameter validation for union payload
  schemas so `anyOf` and `oneOf` event variants are accepted only when every
  branch exposes the routed JSON Pointer as a string, number, or integer token.

## [0.10.12] - 2026-06-07

### Added

- Added Trellis OpenTelemetry error metrics, including the `trellis.errors`
  counter, low-cardinality error attribute sanitization, public telemetry metric
  helpers, and runtime instrumentation for RPCs, jobs, operations, events,
  feeds, transfers, auth failures, and service lifecycle failures.
- Added optional metrics export from the TypeScript telemetry runtime through
  OTLP endpoints or `TRELLIS_METRICS_CONSOLE`, with metric export interval
  support from `OTEL_METRIC_EXPORT_INTERVAL`.

### Changed

- Changed release publishing to use `release/v*` branch markers so GitHub
  Actions creates the release tag only after the release gate passes and
  publishes from the same verified artifacts.

### Fixed

- Fixed release verification so broad Deno tests run after npm package artifacts
  are built, and made the Jobs integration fixture tolerate transient `Jobs.Get`
  projection misses while the Jobs projector catches up.

## [0.10.11] - 2026-06-07

### Added

- Added `release pretag-check` and `release local-verify` xtask commands so
  release operators can run repeatable local verification and fail-closed
  pre-tag GitHub Release workflow dry-runs from the CLI.
- Added prepared Deno test partitions for faster focused package, service,
  UI/tooling, and packaging verification loops after a single prepare step.

### Changed

- Updated the Release workflow with Deno dependency caching, broader Rust tool
  cache coverage, and timing diagnostics for release verification phases.
- Expanded the release guide with exact Git review commands, targeted
  integration fixture guidance, corrected Rust formatter checks, and staged JSR
  publish verification.

### Fixed

- Annotated TypeScript and Rust handler-boundary errors with service, contract,
  surface, request, and trace metadata while preserving declared business error
  types and omitting internal NATS subjects from serialized error contexts.
- Fixed release verification type checks for cross-runtime timer handles and
  Node stack-trace capture, and restored JS demo contract parity resolution with
  local Trellis import mappings.

## [0.10.10] - 2026-06-06

### Added

- Added deps-aware extracted TypeScript service handler aliases for RPC, event,
  feed, job, operation, and health handlers so standalone handlers can preserve
  `service.with(deps)` typing without local argument/result aliases.

### Fixed

- Made Trellis generator output formatting part of artifact generation so
  `prepare` no longer leaves generated TypeScript, npm package, or embedded Rust
  SDK files needing release-time formatter cleanup.

## [0.10.9] - 2026-06-06

### Added

- Added reusable TypeScript cursor pagination helpers for service contracts and
  handlers, including `CursorQuerySchema`, `CursorPageSchema`,
  `normalizeCursorQuery`, and `buildCursorPage` for stable ID/keyset pages.

### Changed

- Extended `TrellisService.with(deps)` so service health checks and health info
  callbacks can receive application-owned dependencies through `args.deps`.
- Documented offset and cursor pagination helper choices in the design docs,
  guide docs, and out-of-tree service `AGENTS.md` template.

### Fixed

- Restored public root and contract-support exports for offset pagination
  handler helpers `normalizePageQuery` and `buildPageResponse`.

## [0.10.8] - 2026-06-06

### Added

- Added authority-owned capability definition storage and API projections so
  accepted deployment authority, not the active catalog, owns runtime capability
  metadata for admin capability listing and capability-group validation.
- Added typed materialized NATS grants for runtime authority, including service
  transfer endpoints, Jobs runtime internals, platform service calls, and
  owned/used surface grants.
- Added Console Creates/Given authority views for service and device deployment
  capabilities.

### Changed

- Runtime authorization now issues service, device, and delegated user NATS
  permissions from accepted materialized authority instead of deriving allow
  lists from active or known contract manifests at reconnect time.
- Service authority planning now keeps requested needs separate from provided
  surfaces, records accepted offers before materialization refreshes, and uses
  latest accepted implementation offers for same-contract digest updates.
- Updated runtime-authority, capability, and auth API design docs to describe
  contracts as planning inputs and materialized authority as the runtime source
  of truth.

### Fixed

- Fixed service reconnects during accepted-offer stale grace windows and
  restored service operation-store KV grants for operation runtimes.
- Fixed user reauth races where stale same-session-key reconnects could
  overwrite a newer successful bind to another approved contract digest.
- Fixed materialized authority reconstruction so provider-owned surfaces remain
  provided surfaces instead of being copied into requested needs.

## [0.10.7] - 2026-06-06

### Added

- Added `TrellisService.with(deps)` for TypeScript services so service-owned
  RPC, feed, operation, job, and event listener handlers can receive
  application-owned dependencies as `args.deps` without mixing them into Trellis
  runtime context or resource bindings.
- Added service event listener context for TypeScript handlers, including event
  id, time, subject, mode, group, and sequence metadata while preserving
  existing payload-first listener usage.

### Changed

- Updated generated TypeScript SDK service typings, docs, and demos to use bound
  service dependency wrappers where handlers need application dependencies.

### Fixed

- Fixed npm release packaging by bumping hardcoded internal package dependency
  ranges in npm build scripts and teaching `release bump` and
  `release check-versions` to manage those specs for future releases.

## [0.10.6] - 2026-06-06

### Added

- Added `TrellisService.with(deps)` for TypeScript services so service-owned
  RPC, feed, operation, job, and event listener handlers can receive
  application-owned dependencies as `args.deps` without mixing them into Trellis
  runtime context or resource bindings.
- Added service event listener context for TypeScript handlers, including event
  id, time, subject, mode, group, and sequence metadata while preserving
  existing payload-first listener usage.

### Changed

- Updated generated TypeScript SDK service typings, docs, and demos to use bound
  service dependency wrappers where handlers need application dependencies.

## [0.10.5] - 2026-06-05

### Changed

- Trellis now treats omitted `nats.jetstream.replicas` as automatic: the runtime
  probes NATS JetStream topology through the system account and uses `3` only
  when enough current metadata peers are visible, otherwise falling back to `1`.
- Trellis authority resource provisioning and reconciliation now pass the
  resolved JetStream replica count into KV buckets, object stores, and built-in
  Jobs streams instead of relying on hardcoded resource defaults.

### Fixed

- Fixed Trellis-created contract resources and Jobs infrastructure so explicit
  `nats.jetstream.replicas` values in `config.jsonc` are honored consistently.

## [0.10.4] - 2026-06-01

### Fixed

- Fixed the JSR `@oatscenter/trellis/generate` wrapper so `deno task prepare`
  can read package metadata when the wrapper is loaded from a remote module URL,
  avoiding Deno's file-URL-only read path before the release binary starts.

## [0.10.3] - 2026-05-30

### Fixed

- Promoted the `0.10.3` release after fixing staged JSR publishing and npm
  artifact smoke failures for the public Trellis packages.

## [0.10.3-rc.4] - 2026-05-29

### Fixed

- Fixed npm package smoke failures by rewriting bundled generated SDK imports to
  public `@oatscenter/trellis` package subpaths and correcting npm `generate`
  manifest discovery for Deno `npm:` execution.

## [0.10.3-rc.3] - 2026-05-29

### Fixed

- Fixed JSR publishing for `@oatscenter/trellis` by replacing published
  same-package `@oatscenter/trellis/*` imports with relative imports, including
  the embedded generated SDKs that JSR analyzes without the workspace import
  map.

## [0.10.3-rc.2] - 2026-05-29

### Fixed

- Restored JSR `--allow-dirty` publishing for release-prepared workspaces, which
  intentionally rewrite manifests and generated artifacts before package
  dry-runs and publishing.

## [0.10.3-rc.1] - 2026-05-29

### Fixed

- Fixed JSR release publishing in GitHub Actions by removing the invalid
  `deno eval --allow-read` invocation while keeping slow-type publishing enabled
  for the current generated SDK surface.
- Removed unnecessary dirty-worktree allowances from JSR release dry-runs and
  publishing.

## [0.10.2] - 2026-05-29

### Fixed

- Enabled GitHub Actions JSR publishing for the staged `@oatscenter/result` and
  `@oatscenter/trellis` packages.
- Fixed the `@oatscenter/trellis` JSR package layout so first-party generated
  SDK exports publish from package-local generated files instead of private
  workspace aliases.

## [0.10.1] - 2026-05-29

### Changed

- Renamed the public Rust facade package to `trellis-rs` (crate name
  `trellis_rs`) for crates.io, replacing the previous `trellis` package name
  which conflicted with an unrelated crate.
- Removed generated Trellis-owned SDK crates (`trellis-sdk-auth`,
  `trellis-sdk-core`) from crates.io release publishing; they remain embedded in
  the public Rust facade.
- Updated all Rust code generation to emit the `trellis-rs` dependency key and
  `trellis_rs` import paths consistently across generated SDKs and participant
  facades.

### Fixed

- Fixed crates.io release publishing to publish only public Rust crates and skip
  private workspace crates marked `publish = false`.
- Fixed release preparation for Rust builds by rewriting internal dependencies
  on the root `trellis` crate to the correct version.
- Fixed release npm smoke checks by installing Node type definitions in the
  generated consumer project.
- Fixed release generator tests for current TypeScript client method output and
  Rust SDK dependency names.
- Fixed Rust-authored demo service contract parity with the TypeScript source
  contract.
- Fixed GitHub Pages release-site builds to tolerate unavailable or incomplete
  release worktrees and fall back to current docs or console sources.
- Updated docs examples and guide snippets to use `trellis_rs` import paths.

## [0.10.0] - 2026-05-29

### Added

- Added deployment authority planning, acceptance, rejection, reconciliation,
  storage, admin RPCs, CLI flows, and Console review pages for service and
  device contract authority changes.
- Added implementation-offer backed service authority, so non-builtin service
  runtime availability is derived from accepted offers instead of repair-state
  catalog evidence.
- Added prepared event outbox/inbox support, event consumer groups, and expanded
  runtime and integration coverage for event publish/subscribe behavior.
- Added first-class Rust `trellis` facade modules for auth, client, service,
  jobs, generated SDKs, events, transfer, resources, and service runtime APIs.
- Added richer generated TypeScript and Rust contract support for digest-stable
  manifests, schema pointers, resource declarations, and generated SDK metadata.
- Added integration-harness fixtures and scenarios for authority, events, feeds,
  jobs, operations, resources, RPC, state, transfer, and public API guards.

### Changed

- Replaced deployment envelopes and envelope-expansion admin surfaces with the
  deployment authority model. Existing envelope RPCs, Console pages, design
  docs, migrations, and helper names were removed or renamed to authority
  terminology.
- Changed service bootstrap, runtime auth, auth-callout, catalog runtime, and
  reconnect checks to use materialized deployment authority and accepted
  implementation offers.
- Changed service APIs to prefer `TrellisService.connect(...)` and returned
  resource handles instead of constructing lower-level runtime objects or
  passing raw binding payloads through service bootstrap code.
- Renamed operation observation/control surfaces and simplified capability key
  handling across contracts, SDKs, tests, and docs.
- Reworked the documentation site under `docs/`, split and expanded the guides,
  added deployment-authority concepts, Rust and TypeScript library pages, and
  refreshed `llms.txt` / `llms-full.txt`.
- Consolidated Rust runtime APIs under the top-level `trellis` crate facade and
  updated demos to use current authority, event, and resource flows.

### Fixed

- Fixed service bootstrap dependency polling, dependency repair handling, and
  default workspace build behavior for clean clone prepare flows.
- Fixed jobs service bootstrap by provisioning jobs consumers before runtime
  use.
- Fixed legacy service-session pruning during storage upgrades and refreshed the
  Trellis service SQLite baseline around deployment authority history.
- Fixed Console contract dependency displays and service contract authority
  summaries for the new authority model.

## [0.9.0-rc.10] - 2026-05-22

### Fixed

- Removed the brittle generated SDK path guard from CLI release builds while
  keeping forced SDK regeneration so each runner rewrites local paths before
  compiling.

## [0.9.0-rc.9] - 2026-05-22

### Fixed

- Fixed macOS CLI release builds to force-refresh generated Rust SDK manifests
  before building, preventing Linux runner absolute paths from leaking into
  Darwin builds.
- Opted release and Pages workflows into Node.js 24 for JavaScript actions to
  avoid the GitHub Actions Node 20 deprecation warning.

## [0.9.0-rc.8] - 2026-05-22

### Fixed

- Fixed GitHub Pages release-site generation fallback cleanup so missing or
  delayed `trellis-generate` release assets no longer leave a stale `RETURN`
  trap that fails under `set -u`.

## [0.9.0-rc.7] - 2026-05-22

### Fixed

- Fixed CLI release builds to install Deno on each build runner before
  refreshing generated Rust SDK paths.

## [0.9.0-rc.6] - 2026-05-22

### Fixed

- Fixed prerelease npm dry-run validation to pass the `rc` dist-tag for
  prerelease package versions.
- Fixed macOS CLI release builds to refresh generated Rust SDK local dependency
  paths on each runner instead of reusing Linux absolute paths from the prepared
  release artifact.

## [0.9.0-rc.5] - 2026-05-22

### Added

- Added Forced Contract Update handling for active contract digest conflicts,
  including operator review, keep/accept resolution paths, and service runtime
  waiting while updates are pending.
- Added Console review and confirmation surfaces for Forced Contract Updates,
  including current/proposed contract comparison details.
- Added cleanup for pending service-originated envelope expansion requests when
  the requesting service disconnects.

### Changed

- Consolidated the GitHub release pipelines for release preparation, CLI assets,
  npm, crates, images, and JSR dry-runs into the unified `Release` workflow.
- Changed active catalog semantics so one `contractId` has at most one current
  active digest; resolving a Forced Contract Update now deletes non-selected
  evidence instead of retaining ignored or quarantined evidence.
- Changed service bootstrap and runtime handling for same-contract digest
  conflicts to report `contract_catalog_issue` and retry while the catalog issue
  is unresolved.
- Renamed Console and admin wording from repair-oriented catalog language to
  Forced Contract Update language.
- Changed generated auth identifiers to use ULID-based user, browser-flow,
  device-flow, and device activation review IDs.

### Fixed

- Fixed local self-registration username conflicts to return `username_taken`
  and present a user-facing "That username is already in use" message.
- Fixed `AuthError` serialization and deserialization to preserve explicit
  human-readable messages.
- Fixed legacy ignored contract-evidence metadata handling during reconnects so
  it is cleared under the Forced Contract Update model.
- Fixed GitHub Pages builds to prefer cached or published `trellis-generate`
  release assets while falling back to building from source when assets are not
  available yet.

## [0.9.0-rc.4] - 2026-05-21

### Added

- Added durable deployment-envelope, contract-evidence, portal, user, session,
  and resource-binding storage so Trellis authority no longer depends on
  long-lived in-memory contract state.
- Added first-class deployment grant overrides, capability groups, envelope
  expansion review surfaces, and active catalog repair support for operator-led
  rollout recovery.
- Added login portal account flows for local password setup/reset, identity
  linking, and admin bootstrap.
- Added durable user administration, linked identities, local password
  credentials, login-attempt tracking, and account-flow session APIs.
- Added first-class feed declarations plus TypeScript and Rust runtime APIs for
  authenticated request/stream subscriptions with typed feed events.
- Added SQLite-backed jobs query storage and paged Jobs admin queries, replacing
  broader KV scans for job, resource, and state projections.
- Added `trellis local init` and `trellis infra apply/check` workflows for local
  NATS/Trellis bootstrap files and shared infrastructure verification.
- Added Rust client support for device connect info, service bootstrap with
  contract evidence and envelope approval waits, feed subscriptions, event
  replay/ack controls, and typed RPC error payloads.
- Added a Rust demo workspace and a shared demo app layout so TypeScript and
  Rust service/device examples exercise the same inspection workflow.
- Added the Rust integration harness plus `trellis-local-bootstrap` and
  `trellis-generate-runner` workspace crates for local bootstrap, release, and
  end-to-end verification workflows.

### Changed

- Reworked auth around deployment envelopes, identity envelopes, auth-owned
  login portals, deployment-owned device routes, user accounts, and
  resource-first admin RPC names.
- Reworked the Trellis CLI command tree around top-level login/logout/whoami,
  users, portals, grants, `svc`/`dev`, local, infra, init, keys, and upgrade
  commands; update scripts and operator runbooks that call old subcommands.
- Reworked the Console admin surface around envelopes, grants, capability
  groups, consolidated devices, service repair, jobs detail, portal routing, and
  destructive-action confirmations.
- Reworked the guides site with split concept pages, a multi-page TypeScript
  service tutorial, updated install/start/local-development flows, and improved
  API-doc navigation.
- Removed the TypeScript runtime `authBypassMethods` option; unauthenticated RPC
  handlers must now opt out per handler with `authRequired: false` instead of
  using process-wide method bypass lists.
- Changed Trellis service storage to a squashed `0.9` SQLite baseline with new
  user identity, local credential, envelope, portal, grant override, resource
  binding, and catalog evidence tables; existing `0.8.x` service databases
  should be treated as incompatible unless an explicit migration path is added.
- Changed RPC, feed, and transfer request proofs to include issued-at and
  request-id headers, renamed the validation RPC to `Auth.Requests.Validate`,
  and rejected transfer proofs that omit those fields.
- Changed store, auth, state, and jobs list-style APIs to require bounded
  standard Trellis pagination or explicit limits, replacing unbounded scans with
  targeted storage queries and `nextOffset` response cursors.
- Changed contract manifests and SDK generation to support grouped
  required/optional uses, feeds, state accepted versions, contract-declared
  capabilities, operation control capabilities/signals, and shared
  `PageRequest`/`PageResponse` pagination models.
- Changed contract digest and catalog handling to support forward-compatible
  schemas, normalized manifests, feeds, declared capabilities, deployment
  evidence quarantine/ignore state, active catalog repair, durable resource
  bindings, and rejection of unsupported v1 subject and jobs/stream fields.
- Changed operation runtime APIs to support service-side control handles, named
  operation signals, durable signal history, and control/cancel capability
  metadata.
- Changed Trellis service configuration to require system NATS credentials,
  resolve credential paths relative to the config file, enable SQLite
  WAL/busy-timeout handling, and reject non-loopback HTTP/WS public origins
  unless listed in `web.allowInsecureOrigins`.
- Renamed the Rust service runtime crate surface from server-oriented naming to
  `trellis-service`, added Rust client state/store support, and expanded service
  resource, transfer, operation, and jobs runtime coverage.
- Moved release tooling into Rust xtask commands and prepared release-managed
  Rust crate, generated SDK, npm package, image, and workflow metadata for the
  `0.9.0-rc.1` release.

### Fixed

- Fixed expired browser auth/account flows to redirect back to the app login
  callback with a `flow_expired` error instead of leaving users stranded in the
  portal flow.
- Fixed Console grant override loading and removal grouping so override lists
  and revoke actions target the correct deployment and identity groups.
- Fixed local password setup/reset/change flows to return clearer policy and
  flow-state errors.
- Fixed release and publishing bootstrap paths so clean checkouts generate the
  required SDK artifacts before release, package, and image verification.
- Fixed generated Rust SDK formatting so prepared generated crates pass the
  workspace formatter checks used by release verification.
- Fixed service runtime RPC subscriptions so multiple instances share requests
  through queue groups instead of each instance handling the same request.
- Fixed service runtime first-connect retry behavior when Trellis is temporarily
  unavailable during bootstrap.
- Fixed heartbeat liveness status and expanded integration coverage for auth,
  catalog repair, events, feeds, jobs, operations, resources, state, transfer,
  portal, and runtime flows.
- Fixed generator TypeScript compiler discovery from repository-root workflows
  that use the JavaScript workspace `node_modules` directory.
- Fixed npm package export normalization so the `@oatscenter/trellis/generate`
  subpath remains available in freshly built publish artifacts.
- Fixed the published npm `@oatscenter/trellis/generate` subpath so it reads
  package metadata from the packed package instead of requiring a source
  `deno.json` next to the generated JavaScript entrypoint.
- Fixed prerelease npm smoke validation to invoke the packed Trellis CLI by its
  exact prerelease version when Deno resolves manual `node_modules` packages.
- Fixed Rust crate prerelease publishing order so registry-verified crates are
  published only after their internal Trellis dependencies are visible in the
  crates.io index.
- Fixed the Rust auth agent-flow polling test timeout so slower CI runners do
  not fail before the mocked redirect status is observed.

## [0.8.4] - 2026-05-07

### Fixed

- Fixed the Trellis npm generator entrypoint and release packaging so generated
  packages can launch the version-pinned generator through npm bin wrappers.

## [0.8.3] - 2026-05-07

### Added

- Added version-pinned generator launchers for shell-first contract generation.

### Fixed

- Fixed shell-first contract generation and serialized auth session store tests
  that were racing in release verification.

## [0.8.2] - 2026-05-01

### Fixed

- Fixed the `@oatscenter/trellis-svelte` npm package to publish built runtime
  JavaScript under `dist/` so Vite can optimize Svelte 5 rune modules without
  parsing raw `.svelte.ts` source from `node_modules`.

## [0.8.0-rc.8] - 2026-05-01

### Fixed

- Fixed the Trellis npm package build by removing the stale activity SDK export
  after the built-in activity contract was removed from generated SDK outputs.

## [0.8.0-rc.7] - 2026-05-01

### Changed

- Changed generated TypeScript SDK packages to expose only their root export,
  export the contract module consistently as `sdk`, and require explicit
  `use(...)` selections instead of `useDefaults()` helpers.

### Fixed

- Fixed GitHub Pages guide builds to prepare generated Trellis SDK artifacts
  before generating TypeScript API docs.
- Fixed CLI release publishing to build and attach `trellis-generate` archives
  alongside `trellis`, added `trellis-generate self check/update`, and hardened
  self-upgrade asset selection for both binaries.

## [0.8.0-rc.6] - 2026-04-30

### Changed

- Normalized formatting in the `trellis-generate` Rust command and planning
  code.

### Added

- Added unauthenticated `GET /version` on the Trellis service to report public
  build version and revision metadata for deployed containers.
- Added `trellis-generate prepare --out <path>` to let callers choose the output
  root for generated manifests and SDKs while scanning contracts from a separate
  source path.

### Fixed

- Fixed the built-in Trellis portal to resolve the runtime origin from the
  browser by default so published service images work behind deployment-specific
  hostnames without rebuilding.
- Fixed npm package error construction and Deno service transport loading so
  locally linked npm artifacts can construct Trellis errors and connect service
  runtimes without relying on missing dnt or Deno transport package shims.
- Fixed hand-built `trellis` and `trellis-generate` binaries to include local
  git metadata in `--version` output while keeping official GitHub Actions
  release builds on the clean package version.
- Fixed local app aliases and Trellis client SDK loading so generated Trellis
  SDK imports resolve through linked packages instead of app-private
  `#trellis-generated-sdk/*` aliases, and added a regression test for
  out-of-tree SDK generation defaulting to the `@trellis-sdk/` scope.
- Fixed generated TypeScript SDK dependency metadata to use the Trellis runtime
  version bundled with `trellis-generate`, emit npm runtime imports, and publish
  npm SDKs with Trellis as a peer and development dependency instead of a nested
  runtime dependency.
- Fixed checked-in release version bumps to include the standalone
  `trellis-generate` Cargo package version.

## [0.8.0-rc.5] - 2026-04-30

### Changed

- Changed generated TypeScript SDK package names for non-Trellis-owned contracts
  to default to the `@trellis-sdk/` scope, added `--prefix` for custom generated
  SDK package prefixes, and updated demos/docs to consume generated SDKs as
  linked packages instead of import-map aliases.

### Fixed

- Fixed npm package smoke validation to pack and install the release artifacts
  locally, verify public ESM/CJS/TypeScript consumer imports, and reject private
  generated SDK build-path references in published packages.

## [0.8.0-rc.4] - 2026-04-30

### Fixed

- Fixed export in public packages

## [0.8.0-rc.3] - 2026-04-30

### Fixed

- Moved release metadata validation to the start of all publish workflows so a
  missing release changelog section fails before crates, npm packages, CLI
  binaries, or container images are built.
- Built the console ARM64 image from the amd64 GitHub runner with QEMU so the
  static console build uses the known-good amd64 Vite/Rolldown/Tailwind path
  while still producing an ARM64 nginx runtime image.

## [0.8.0] - 2026-04-30

### Changed

- Raised the minimum supported `nats-server` version to 2.10.0 and changed
  jobs-derived worker permissions to use only the newer filtered JetStream
  consumer-create API subject.
- Documented the Trellis service v1 follow-up cleanup across design docs,
  guides, and portal notes: active subject collision checks now use the
  effective wildcard subject for templated events, omitted store `maxTotalBytes`
  reconciles object-store streams back to the unlimited runtime default, and
  portal selection records are keyed directly by browser app contract id or
  device deployment id.
- Documented the final Trellis service v1 architecture cleanup across design
  docs, guides, the built-in portal, and demos: active catalog and approval
  planning now fail closed on inactive dependencies, embedded schemas reject all
  `$ref`, event template params are schema-checked, operation handlers use
  `op.defer()` for external completion, service resources are exact-digest
  install input, missing sessions return `session_not_found`, and malformed
  internal State or connection storage is treated as runtime noise/corruption
  rather than caller data.
- Redesigned the JavaScript browser demo as the Field Inspection Desk demo
  client, with a Trellis-powered product identity, mostly-light Executive
  Systems theme, full-height left navigation shell, integrated workflow pages,
  and clearer live-versus-fixture data handling.
- Documented and surfaced the final v1 activation/state cleanup across design
  docs, guides, the built-in portal, and console: device activation review
  decisions now complete the original `Auth.DeviceUserAuthorities.Resolve`
  operation durably, and State rejects unstamped pre-v1 entries instead of
  inferring current or accepted-version metadata.
- Documented and surfaced the final Trellis service hardening pass across design
  docs, guides, the built-in portal, and console: active-catalog dry-run
  validation now gates staged apply/bootstrap state, service apply/unapply roll
  back failed refreshes instead of exposing partial mutations, and service NATS
  reconnect requires the exact current digest still allowed by the deployment.
- Aligned Trellis service v1 docs and app surfaces with the latest cleanup:
  optional payload-field removal is documented as compatible when absence
  remains valid, State no longer infers accepted versions for unversioned
  entries, State list pagination documents the current NATS KV scan/sort
  trade-off, auth callout unexpected failures return a stable `internal_error`,
  auth HTTP rate limiting avoids client-controlled forwarding headers, device
  runtime cleanup removes durable device sessions by public identity key, and
  store bindings no longer advertise unenforced per-object limits.
- Aligned the Trellis service v1 deployment model so required resources are
  provisioned from deployment envelopes before runtime binding, service
  bootstrap consumes deployment-owned resource bindings, physical resource names
  remain deployment scoped, app/agent contracts are treated as approved-session
  contracts rather than active catalog entries, and baseline
  `Auth.Requests.Validate` may be granted by resolved envelopes.
- Changed same-lineage active digest projection to verify duplicate RPC,
  operation, event, and job schema refs by resolved schema compatibility instead
  of ref-name equality: canonically equal schemas and optional additive fields
  on open objects are accepted, while closed-object property-set divergence and
  unproven non-identical schema constructs fail closed.
- Changed Rust CLI agent login and reauth to use the same normalized contract
  identity digest as the TypeScript catalog, so display-only metadata changes no
  longer cause `contract_changed` reconnect denials, and generic NATS
  authorization denials no longer clear the saved local CLI session unless auth
  explicitly reports the session as missing, revoked, or rejected.
- Browser Trellis clients now treat revoked or missing sessions
  (`session_not_found`) as auth-required, allowing Svelte apps such as the
  console to redirect back through their login page with the current return
  path.
- Removed raw subject declarations and service-declared stream resource requests
  from the documented v1 contract surface, aligned guides/portal docs/console
  copy with app-based portal contracts, and clarified that unversioned Trellis
  State entries remain readable when the current schema accepts them.
- Documented and surfaced the Trellis service v1 cleanup across design docs,
  guides, login portal docs, and console admin views, including canonical device
  connect-info, deployment-derived device digests, optional KV/store resources,
  operation analysis metadata, fail-closed catalog refresh, and explicit
  transfer-derived permissions.
- Aligned outward-facing auth, catalog, app, and guide docs with the Trellis v1
  runtime/control-plane cleanup and active-compatible-digest `uses` validation.
- Documented fail-closed active catalog refresh, explicit auth-callout denial,
  read/cancel-only operation-control grants, and strict device allowed-digest
  validation across design docs, guides, the built-in portal README, and demos.
- Tightened Trellis service v1 behavior by validating contract `uses` before
  persistence, scoping operation control permissions to read/cancel grants,
  keeping jobs projection buckets runtime-owned, requiring NATS for jobs
  provisioning, accepting boolean State JSON schemas, and making startup and
  shutdown cleanup attempt every registered path.
- Changed contract identity to hash a normalized runtime/interface digest
  projection instead of human-facing manifest metadata, and removed
  service-declared stream resources from the v1 contract/resource model.
- Reworked service and device rollout management around explicit deployments:
  auth protocol, storage, runtime bootstrap, CLI, portals, docs, guides, and
  demos now use `ServiceDeployment` / `DeviceDeployment`, `deploymentId`, and
  `trellis deploy` instead of service/device profile APIs.
- Changed Trellis State to store author-owned state versions and internal writer
  digest provenance per entry, while keeping durable namespaces scoped by
  contract id lineage. Older declared `acceptedVersions` now surface
  migration-required read/list/conditional-put responses for app/device-side
  migration instead of digest-keyed author APIs.
- Tightened the Trellis control-plane service for v1 by removing display-name
  based Trellis-owned contract implementation, keeping user sessions out of the
  deployment-active catalog, validating State accepted-version schemas at
  contract validation time, removing hidden stream replica downgrades, and
  reducing bound resource runtime grants to use-oriented subjects.
- Changed Svelte app docs and examples to derive app-local client types from
  `createTrellisApp` contracts instead of importing generated `client.ts`
  facades from local `generated/js/sdks/...` paths.
- Changed local SvelteKit app aliasing so each app owns explicit `kit.alias`
  mappings, with Vite relying on SvelteKit-provided aliases and the old shared
  frontend workspace alias helper removed.
- Changed `@oatscenter/trellis-svelte` so `createTrellisApp(...)` owns both the
  contract and Trellis URL and `TrellisProvider` takes a single `trellisApp`
  prop instead of separate app and `trellisUrl` props.
- Changed TypeScript contract authoring so manifest `exports` are declared in
  the `define*Contract(...)` callback body rather than the first-argument local
  registry, with registry-side `exports` now rejected at type and runtime
  boundaries.
- Changed Trellis control-plane storage so durable auth, catalog, service,
  device, portal, and session records are SQLite-backed with ULID row IDs while
  KV remains for OAuth/pending/browser scratch, connection presence, and the
  public State API; updated CLI bootstrap and docs for the SQL/KV boundary.
- Changed TypeScript activated-device startup so root
  `TrellisDevice.connect(...)` is runtime-only, Deno devices use
  `checkDeviceActivation(...)` to learn whether activation is ready or still
  required, hidden Deno activation-state persistence is scoped by deployment
  origin, device identity, and contract digest, and the JS device demos, design
  docs, and device guide now follow the `checkDeviceActivation(...)` then
  `connect(...)` flow.
- Redesigned `@oatscenter/trellis-svelte` around app-owned separate contexts:
  `createTrellisProviderContexts<TContract>()` now bundles Trellis, auth, and
  connection-state contexts for `TrellisProvider`, the old runtime-bag design is
  gone, and the design docs, SvelteKit guide, and browser demo app now show the
  `contexts`-based integration path.
- Changed service-owned KV from an opened-at-startup helper pattern to a
  schema-backed contract surface: `resources.kv.<alias>` now requires `schema`,
  `service.kv.<alias>` and handler `trellis.kv.<alias>` are directly typed
  stores, and public service-author guidance now leads only with
  `TrellisService.connect(...)` rather than exposing Trellis-internal bootstrap
  helpers.
- Made the JavaScript service jobs lifecycle service-owned by removing public
  `jobs.startWorkers()`, making `jobs.<queue>.handle(...)` synchronous, and
  starting and stopping registered job workers through `service.wait()` /
  `service.stop()` instead.
- Changed TypeScript contract discovery and authoring guidance so
  single-contract projects may use a top-level `contract.ts` or `contract.js`,
  updated design and guide docs to describe that layout, and migrated the JS
  demos from one-file `contracts/` folders to root `contract.ts` modules.
- Renamed the TypeScript service runtime package from
  `@oatscenter/trellis/host*` to `@oatscenter/trellis/service*`, aligned the
  extracted service handler types to `RpcHandler`, `JobHandler`, and
  `OperationHandler`, and updated design docs and demo examples to show the
  canonical single-object handler callback shape with the narrow injected
  service `trellis` facade.
- Changed `trellis auth login` to require a positional Trellis URL, renamed the
  persisted admin-session URL field to `trellis_url`, and updated the related
  design and guide examples.
- Changed TypeScript `prepare` so service and app contracts generate concrete
  consumer `client.ts` facade types, SvelteKit-style `src/lib/contract.ts`
  contracts are discovered, and app contracts produce TypeScript SDKs without
  Rust SDK crates.
- Updated Svelte app integration so `createTrellisApp` derives app-local client
  helper types from the supplied contract without app-local casts or handwritten
  overloads.
- Simplified portal auth by removing the portal contract kind and portal
  `appContractId`, keeping custom portals as routing config, and moving
  authenticated device activation to a single
  `Auth.DeviceUserAuthorities.Resolve` operation.
- Made the TypeScript service runtime surface v1-clean by removing the legacy
  `TrellisServer` public name, making `@oatscenter/trellis/service*` explicit
  service-author entrypoints, hiding raw runtime and NATS transport internals
  from root and generated client facades, and using `TrellisConnection` for
  lifecycle control.
- Changed TypeScript contract authoring so baseline app, agent, device, and
  top-level state Trellis-owned dependencies are derived automatically, while
  non-baseline Auth surfaces use explicit `auth.use(...)` declarations.
- Simplified the Trellis control-plane service for pre-v1 by removing old auth
  reconnect compatibility paths, making `sessionKey` the durable session storage
  identity, validating operation subjects before contract persistence, scoping
  State entries by contract id lineage, and replacing raw SQLite bootstrap with
  a Drizzle baseline migration named `00000_baseline`. Existing pre-baseline
  development databases must be deleted or recreated.
- Removed undocumented pre-v1 Trellis auth compatibility paths, including the
  query-init `/auth/login` flow, public `/auth/bind` token bind endpoint,
  `TRELLIS_AUTH_CONFIG`, contract `sessionKey` records, and single-active digest
  assumptions, while keeping flow-owned bind and multi-active compatible
  contract rollout support.

### Added

- Added `trellis-generate prepare --watch`, repo-local
  `cargo xtask prepare-watch`, and JS/demo `prepare:watch` tasks so contract and
  SDK artifacts can stay fresh during active service and app development. Watch
  mode now prepares only affected contract entries when safe, ignores
  non-TypeScript, non-JavaScript, and non-Rust file changes except recognized
  project/discovery inputs, falls back to full prepare for project manifests and
  discovery-shape changes, asks for a watch restart after generator/tooling
  changes, and prints the event paths plus the watch decision and reason with
  `--changes`.
- Added a reusable Svelte `DeviceActivationController` for custom and built-in
  authenticated device portal flows.
- Made service and activated-device runtime NATS lifecycle logging explicit so
  disconnects, reconnect attempts, reconnect success, stale connections, and
  connection errors produce distinct operator-facing messages.
- Moved contract-manifest job queue declarations to canonical top-level `jobs`
  in both the JavaScript and Rust contract layers, and aligned bootstrap and
  contract-get views with that shape.

### Fixed

- Fixed browser portal approval submissions so the built-in login portal and
  shared portal helpers send the auth endpoint's canonical `approved: boolean`
  request body, preventing console login approval from failing with HTTP 400.
- Fixed console auth-required redirects to restart the configured console login
  flow with a session-ended message instead of reusing a stale provider URL.
- Fixed Trellis local watched restarts to exit cleanly after shutdown, bounded
  HTTP listener drain during Trellis control-plane shutdown, and aligned
  service-author docs and JS demo shutdown examples with that deterministic exit
  pattern.
- Fixed schema-backed KV validation so invalid stored values now surface read or
  watch errors instead of being auto-deleted, and delayed service heartbeat
  publishing until required KV bootstrap succeeds.
- Fixed `trellis-generate` top-level contract discovery to reject ambiguous
  duplicate layouts while ignoring helper modules named `contract.ts` or
  `contract.js` that do not default export a contract, while also skipping
  `.worktrees/` during contract discovery.
- Granted KV-backed services JetStream info access so operation handlers can
  open their durable operation store without `$JS.API.INFO` permission errors.
- Fixed jobs worker permission grants for cancellation subscriptions and made
  server shutdown idempotent while NATS draining is already in progress.
- Corrected demo workspace generated-SDK resolution during contract prepare,
  wrote local TypeScript SDKs into the owning nested JS workspace, and switched
  local generated Trellis imports to repo-relative runtime paths.
- Fixed `TrellisClient.connect(...)` and `TrellisDevice.connect(...)` so
  contract-driven RPC request typing is inferred from the passed contract rather
  than widening typed responses like `Auth.Sessions.Me` to `unknown`.
- Fixed activated-device state flows by preserving top-level contract `state`
  metadata, refreshing device reconnect permissions from the presented digest,
  and encoding state KV keys safely so the JavaScript state demo runs
  end-to-end.
- Fixed standalone login portal builds by defaulting the portal Trellis URL to
  `http://localhost:3000` when `PUBLIC_TRELLIS_URL` is not set.

## [0.8.0-rc.1] - 2026-04-19

### Added

- Made TypeScript jobs and named stores first-class client surfaces.
- Split the JavaScript demo into more focused inspection surfaces.

### Changed

- Switched public async APIs to `AsyncResult` and hardened operation startup and
  transfer flows.
- Simplified runtime auth wiring by removing binding tokens and advertising
  native client NATS endpoints.

### Fixed

- Aligned detached agent login and revocation behavior.
- Stabilized console profile loading across reconnects, supported optional
  portal app contracts, and trimmed login portal files from the runtime image.

[Unreleased]: https://github.com/OATS-Center/trellis/compare/v0.100.0-rc.1...HEAD
[0.100.0-rc.1]: https://github.com/OATS-Center/trellis/compare/eb659cabba5a8847e096159ebd641a70bbf252e6...v0.100.0-rc.1
[0.11.0]: https://github.com/OATS-Center/trellis/compare/v0.11.0-rc.8...v0.11.0
[0.11.0-rc.8]: https://github.com/OATS-Center/trellis/compare/v0.11.0-rc.7...v0.11.0-rc.8
[0.11.0-rc.7]: https://github.com/OATS-Center/trellis/compare/v0.11.0-rc.6...v0.11.0-rc.7
[0.11.0-rc.6]: https://github.com/OATS-Center/trellis/compare/v0.11.0-rc.5...v0.11.0-rc.6
[0.11.0-rc.5]: https://github.com/OATS-Center/trellis/compare/v0.11.0-rc.4...v0.11.0-rc.5
[0.11.0-rc.4]: https://github.com/OATS-Center/trellis/compare/v0.11.0-rc.3...v0.11.0-rc.4
[0.11.0-rc.3]: https://github.com/OATS-Center/trellis/compare/v0.11.0-rc.2...v0.11.0-rc.3
[0.11.0-rc.2]: https://github.com/OATS-Center/trellis/compare/v0.11.0-rc.1...v0.11.0-rc.2
[0.11.0-rc.1]: https://github.com/OATS-Center/trellis/compare/v0.10.22...v0.11.0-rc.1
[0.10.22]: https://github.com/OATS-Center/trellis/compare/v0.10.21...v0.10.22
[0.10.21]: https://github.com/OATS-Center/trellis/compare/v0.10.20...v0.10.21
[0.10.20]: https://github.com/OATS-Center/trellis/compare/v0.10.19...v0.10.20
[0.10.19]: https://github.com/OATS-Center/trellis/compare/v0.10.18...v0.10.19
[0.10.18]: https://github.com/OATS-Center/trellis/compare/v0.10.18-rc.1...v0.10.18
[0.10.18-rc.1]: https://github.com/OATS-Center/trellis/compare/v0.10.17...v0.10.18-rc.1
[0.10.17]: https://github.com/OATS-Center/trellis/compare/v0.10.16...v0.10.17
[0.10.16]: https://github.com/OATS-Center/trellis/compare/v0.10.15...v0.10.16
[0.10.15]: https://github.com/OATS-Center/trellis/compare/v0.10.14...v0.10.15
[0.10.14]: https://github.com/OATS-Center/trellis/compare/v0.10.13...v0.10.14
[0.10.13]: https://github.com/OATS-Center/trellis/compare/v0.10.12...v0.10.13
[0.10.12]: https://github.com/OATS-Center/trellis/compare/v0.10.11...v0.10.12
[0.10.11]: https://github.com/OATS-Center/trellis/compare/v0.10.10...v0.10.11
[0.10.10]: https://github.com/OATS-Center/trellis/compare/v0.10.9...v0.10.10
[0.10.9]: https://github.com/OATS-Center/trellis/compare/v0.10.8...v0.10.9
[0.10.8]: https://github.com/OATS-Center/trellis/compare/v0.10.7...v0.10.8
[0.10.7]: https://github.com/OATS-Center/trellis/compare/v0.10.6...v0.10.7
[0.10.6]: https://github.com/OATS-Center/trellis/compare/v0.10.5...v0.10.6
[0.10.5]: https://github.com/OATS-Center/trellis/compare/v0.10.4...v0.10.5
[0.10.4]: https://github.com/OATS-Center/trellis/compare/v0.10.3...v0.10.4
[0.10.3]: https://github.com/OATS-Center/trellis/compare/v0.10.3-rc.4...v0.10.3
[0.10.3-rc.4]: https://github.com/OATS-Center/trellis/compare/v0.10.3-rc.3...v0.10.3-rc.4
[0.10.3-rc.3]: https://github.com/OATS-Center/trellis/compare/v0.10.3-rc.2...v0.10.3-rc.3
[0.10.3-rc.2]: https://github.com/OATS-Center/trellis/compare/v0.10.3-rc.1...v0.10.3-rc.2
[0.10.3-rc.1]: https://github.com/OATS-Center/trellis/compare/v0.10.2...v0.10.3-rc.1
[0.10.2]: https://github.com/OATS-Center/trellis/compare/v0.10.1...v0.10.2
[0.10.1]: https://github.com/OATS-Center/trellis/compare/v0.10.0...v0.10.1
[0.10.0]: https://github.com/OATS-Center/trellis/compare/v0.9.0...v0.10.0
[0.9.0]: https://github.com/OATS-Center/trellis/compare/v0.8.4...v0.9.0
[0.8.4]: https://github.com/OATS-Center/trellis/compare/v0.8.3...v0.8.4
[0.8.3]: https://github.com/OATS-Center/trellis/compare/v0.8.2...v0.8.3
[0.8.2]: https://github.com/OATS-Center/trellis/compare/v0.8.1...v0.8.2
[0.8.0]: https://github.com/OATS-Center/trellis/compare/v0.7.0...v0.8.0
[0.8.0-rc.1]: https://github.com/OATS-Center/trellis/compare/v0.7.0...v0.8.0-rc.1
