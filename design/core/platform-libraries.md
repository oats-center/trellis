---
title: Platform Libraries
description: Responsibilities and boundaries for the core Trellis runtime, auth, contracts, jobs, and telemetry libraries.
order: 20
---

# Design: Platform Libraries

## Prerequisites

- [trellis-patterns.md](./trellis-patterns.md) - Trellis architecture and
  platform boundaries
- [type-system-patterns.md](./type-system-patterns.md) - Result, schema, and
  error conventions
- [../auth/trellis-auth.md](./../auth/trellis-auth.md) - auth subsystem design

## Scope

This document defines the responsibilities of the core Trellis platform
libraries: which package owns which platform surface, which surfaces are public
or service-only, and which low-level details must stay behind runtime facades.

It is not a language-library usage guide. Ordinary app and service examples,
connection walkthroughs, and exact public signatures belong in:

- `/guides/libraries/typescript`
- `/guides/libraries/rust`
- `/api` for generated TypeScript API reference and Rustdoc links

## Core Libraries

| Library                             | Purpose                                                                                                                  | Use when                            |
| ----------------------------------- | ------------------------------------------------------------------------------------------------------------------------ | ----------------------------------- |
| `@oats-center/trellis`              | Canonical core Trellis runtime package: client/device helpers, Result helpers, transfers, and generated-contract support | Frontend apps, services, CLI tools  |
| `@oats-center/trellis/service`      | Service-side runtime facade, extracted handler types, and service-only helpers                                           | Backend services                    |
| `@oats-center/trellis/service`      | Shared Node-compatible native service runtime                                                                            | Node and Deno services              |
| `@oats-center/trellis/auth`         | Full auth helper and auth protocol surface, including browser bind helpers                                               | Apps, services, docs, tests         |
| `@oats-center/trellis/auth/browser` | Browser-only auth and portal-flow helper facade                                                                          | Browser apps, custom portals        |
| `@oats-center/trellis/participant`  | Participant metadata and canonical runtime types                                                                         | Generated code and tooling          |
| Configured local generated package  | One ordinary ESM package with `apis` and `participants` namespaces                                                       | Apps and services that consume APIs |
| `@oats-center/trellis/telemetry`    | Specialized Trellis telemetry facade for tracing, propagation, and metrics                                               | Runtime libraries and services      |
| `@oats-center/trellis-svelte`       | Svelte-specific Trellis browser integration with a Trellis-only public surface                                           | Svelte applications                 |

## Library Rules

- `@oats-center/trellis` is the canonical app and service package for Trellis
  TypeScript development
- service APIs are defined with the service that owns them and are consumed
  through contract packages
- server helpers live on explicit Trellis subpaths
- APIs and participant surfaces live in the configured local generated package,
  with the source package version and the CLI-matched published runtime
  dependency
- health contract types come from the generated Health API module
- framework adapters such as `@oats-center/trellis-svelte` remain separate
  packages
- platform packages should expose stable ergonomic surfaces and hide
  transport/bootstrap details
- browser-safe public runtime APIs and generated descriptor support belong on
  `@oats-center/trellis`
- the root package should expose Trellis-owned lifecycle handles such as
  `TrellisConnection`, not raw transport handles such as `NatsConnection`
- browser-only login and portal-flow helpers belong on
  `@oats-center/trellis/auth` and the narrower
  `@oats-center/trellis/auth/browser` facade
- service-only resource handles and bootstrap helpers belong on
  `@oats-center/trellis/service*`
- `@oats-center/trellis/service` is a service-author surface and must not
  re-export low-level runtime/server internals
- generic SQL outbox helpers, including `service.createSqlOutbox(...)` and
  Trellis-owned helper-table migration artifacts, belong on
  `@oats-center/trellis/service*`; applications adapt their database and
  transaction handles to the generic `SqlExecutor` interface
- public TypeScript jobs helpers belong on `@oats-center/trellis` and
  `@oats-center/trellis/service*`, not on a standalone jobs package

## `@oats-center/trellis`

Canonical TypeScript entrypoint for contract-driven RPC, operation, event, and
transfer-grant-driven file communication. The root package is browser-safe, does
not eagerly load generated SDKs, exports the normal kind-specific contract
builders plus health helpers, and keeps host-specific service helpers on
`@oats-center/trellis/service*` subpaths.

Rules:

- browser auth uses a session key stored in IndexedDB plus a bind flow;
  SvelteKit apps may layer on `@oats-center/trellis-svelte`
- app, agent, device, service, and CLI code should communicate through
  contract-derived RPC, operation, event, feed, state, and transfer facades
  rather than raw NATS subjects
- RPCs are timeout-bounded and expected remote failures use explicit `Result`
  conventions rather than exception-driven control flow
- operations and events are contract-driven rather than raw-subject-driven in
  normal app code
- admin jobs access should use `Jobs.*` RPCs declared through the jobs SDK
  rather than a client-side jobs helper
- transfer execution belongs to transfer-capable operations; receive grants are
  consumed through runtime transfer helpers rather than raw store bindings

Language-specific app and service walkthroughs belong in
`/guides/libraries/typescript`, `/guides/libraries/rust`, and `/api`.

### Server-Owned Runtime Helpers

`@oats-center/trellis/service` owns the service-only runtime surface.

Rules:

- service code should use `service.kv`, `service.store`, and `service.jobs`
  rather than a nested `service.resources.*` runtime shape
- schema-backed service KV resources are exposed directly as typed
  `service.kv.<alias>` and handler `client.kv.<alias>` stores; only store
  resources use `.open()`
- jobs-enabled services should declare top-level contract `jobs` and use
  `service.jobs.<queue>` plus `service.wait()` / `service.stop()` rather than
  raw worker-runtime helpers or stream bindings
- transfer-aware operation contexts belong on the server runtime surface for
  transfer-capable operations
- SQL outbox creation belongs on the service runtime surface: TypeScript
  services create it with `service.createSqlOutbox(...)`, and the returned
  object is a plain dependency that handlers close over at registration without
  importing low-level repository or dispatcher internals
- extracted service RPC handler aliases that need service-only helpers belong on
  `@oats-center/trellis/service*`, not the browser-safe root package, so handler
  types expose the canonical object argument shape and narrow injected `trellis`
  facade for `kv`, `store`, and transfer-aware operation contexts

## Generated Packages

Each selected language produces one ordinary local package containing owned
APIs, exact locked dependencies, and participants. TypeScript exposes `apis` and
`participants` from its executable ESM entrypoint, with declarations alongside
the JavaScript. Rust exposes the same namespaces in one path-dependency crate.

Author API and participant declarations in native IDL, including selected `use`
actions. Do not build participant metadata with TypeScript arrays. There are no
independent API or participant packages. Registry APIs come from the validated
global cache; generated imports never refer to project installation state. See
[trellis-idl.md](../contracts/trellis-idl.md) for configuration and offline
behavior.

## `@oats-center/trellis-svelte`

Provides the app-level browser adapter for Svelte applications.

Rules:

- browser apps should centralize Trellis setup in one app-local module and
  expose app-scoped typed helpers to components
- the adapter derives the connected client type from the app contract and
  delegates runtime bootstrap/reconnect to the core browser client
- browser apps import their generated participant from the local package and use
  it as the authority for the connected client type; do not reconstruct metadata
- `trellis-svelte` should keep the typed Trellis client and reactive connection
  adapter scoped to app-owned context rather than exposing a synthetic runtime
  bag
- `@oats-center/trellis-svelte` MUST NOT expose raw NATS clients, NATS
  connection state, or other transport-owned handles as public API
- normal pages and components should not recreate auth state; they should
  consume the live app-scoped Trellis context
- app-facing auth helpers should not require raw URL plumbing or placeholder
  positional arguments from app code; they should expose options-shaped APIs
  with sensible redirect defaults
- app-facing auth helpers should accept opaque portal context so apps and custom
  portals can coordinate runtime UX without hard-coding portal-specific
  parameters
- browser clients should route revoked or missing sessions (`session_not_found`)
  through the auth-required path so Svelte apps can redirect to their login
  route and preserve the current return URL
- dynamic auth-instance selection remains a valid advanced case, but the default
  public browser-app API should optimize for the fixed-instance path rather than
  forcing every app through explicit auth-state construction
- browser-app integrations should not require a `serviceName` prop; if a client
  label is needed for telemetry, it should derive from contract metadata or
  internal defaults
- exact Svelte helper names, provider props, and page/component examples belong
  in frontend/library guides and `/api`

## `@oats-center/trellis/auth`

Provides the full auth helper, schema, protocol, and browser bind surface. The
browser-only portal/login helper facade also lives at
`@oats-center/trellis/auth/browser`. See:

- [../auth/trellis-auth.md](./../auth/trellis-auth.md)
- [../auth/auth-api.md](./../auth/auth-api.md)
- `/api` in the guides site for exact TypeScript auth helpers and Rustdoc links

## `@oats-center/trellis/telemetry`

Provides the specialized Trellis telemetry facade used by runtime libraries and
services without widening the root package. It owns trace propagation helpers,
Trellis span helpers, telemetry initialization, and low-cardinality error metric
recording. See [observability-patterns.md](./observability-patterns.md).

Rules:

- runtime code should import telemetry helpers from
  `@oats-center/trellis/telemetry` or internal telemetry modules, not from a
  public `./tracing` subpath
- browser-safe entrypoints must not statically import OpenTelemetry SDK or
  exporter packages
- error metrics must use stable contract or runtime labels and must not include
  raw NATS subjects, user/session identifiers, trace IDs, request IDs, payloads,
  or error messages

## Jobs Surfaces

Service-private jobs are part of the Trellis runtime surface rather than a
standalone TypeScript package.

- TypeScript service-local jobs live on connected service runtimes as
  `service.jobs`
- TypeScript admin jobs access uses `Jobs.*` RPCs selected in native IDL and
  exposed through generated participant callers
- subsystem semantics and API details live in:
  - [../jobs/trellis-jobs.md](./../jobs/trellis-jobs.md)
  - `/api` in the guides site for exact TypeScript signatures and Rustdoc links

## `@oats-center/trellis/participant`

Provides participant metadata and canonical runtime types used by generated code
and tooling. It is not a contract-authoring API. Native IDL is the source of
APIs and participants; consumers import the generated package. See:

- [../contracts/trellis-api-participants.md](./../contracts/trellis-api-participants.md)
- [../contracts/trellis-idl.md](./../contracts/trellis-idl.md)
- [../contracts/trellis-rust-contract-libraries.md](./../contracts/trellis-rust-contract-libraries.md)
