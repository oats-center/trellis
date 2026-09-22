---
title: Trellis Design Index
description: How the Trellis design docs are organized and which documents to read for a given task.
---

# Trellis Design Index

Use this index to find the smallest set of design docs needed for a task.

Do not load the entire `design/` folder by default. Start with one topic area,
then follow only the linked prerequisites that matter for the task at hand.

## Documentation Scope

The `design/` tree records Trellis architecture, protocol semantics, durable
invariants, lifecycle rules, and public Trellis-owned wire compatibility. It is
not the primary API reference for TypeScript or Rust packages.

Use `design/` for material that must remain true across implementations and over
time:

- architecture and package-ownership decisions
- invariants, lifecycle rules, and system rules
- protocol, wire, and authorization semantics
- durable data structures, native API formats, and compatibility rules
- runtime behavior that language libraries must preserve

Use the docs site for reader-facing and language-specific material:

- tutorials, task walkthroughs, and troubleshooting guides live under `docs/`
  and are published as `/guides/*`
- TypeScript language-library usage belongs in `/guides/libraries/typescript`
- Rust language-library usage belongs in `/guides/libraries/rust`
- generated TypeScript API reference comes from JSDoc on public entrypoints and
  is discovered through `/api`
- generated Rust API reference comes from Rustdoc and is discovered through
  `/api`

Design docs may name the public helper family that implements a system rule, but
ordinary examples such as how to connect a client, call an RPC, upload bytes, or
mount a handler should live in guides and API reference. Keep exact language
helper signatures in generated docs unless the signature itself is a protocol or
wire-compatibility requirement.

## Quick Participant Examples

These headings are intentionally named for fast human and AI lookup.

- Minimal installable service example:
  `core/service-development.md#minimal-installable-service-example`
- Minimal activated device example:
  `auth/device-activation.md#minimal-activated-device-example`
- When choosing between them, read
  `core/service-development.md#participant-kind-and-runtime-helper` first

## Core Platform Docs

| Document                                | Read When                                                                | Why                                                                                        |
| --------------------------------------- | ------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------ |
| `core/trellis-patterns.md`              | You need Trellis-wide architecture rules                                 | Service categories, platform boundaries, communication patterns                            |
| `auth/trellis-auth.md`                  | You are changing auth architecture                                       | Principals, installed participants, GrantBinding, contexts, bootstrap, and Auth boundaries |
| `auth/rust-authorization-state.md`      | You are changing Rust-owned auth state or materialization                | Durable records, exact authority materialization, issuable-state boundary                  |
| `auth/rust-auth-service-ownership.md`   | You are changing external auth, bootstrap, session, or callout ownership | TypeScript inventory and permanent Rust cutover boundaries                                 |
| `auth/device-activation.md`             | You are changing device preregistration or device activation             | Known-device activation flow, connect info, profiles, online activation                    |
| `contracts/trellis-api-participants.md` | You are changing manifests, codegen inputs, or permission derivation     | Canonical contract format, `uses`, subject ownership, activation rules                     |
| `contracts/trellis-idl.md`              | You are changing declarative contract source or compilation              | IDL source layouts, syntax, types, and protocol lowering boundary                          |

## Subsystem Design Docs

| Document                                       | Read When                                                           | Why                                                                       |
| ---------------------------------------------- | ------------------------------------------------------------------- | ------------------------------------------------------------------------- |
| `operations/trellis-operations.md`             | You are designing caller-visible async workflows                    | Operations model, auth model, internal control protocol, watch semantics  |
| `jobs/trellis-jobs.md`                         | You are designing service-private background execution              | Jobs model, stream/SQL projection, retries, worker lifecycle, admin model |
| `contracts/trellis-idl.md`                     | You are changing Trellis authoring or compilation                   | IDL syntax, semantic types, dependencies, and protocol lowering           |
| `contracts/trellis-rust-contract-libraries.md` | You are changing generated Rust libraries                           | Rust participant facades, alias model, generation rules                   |
| `tooling/trellis-cli.md`                       | You are changing Trellis CLI behavior or contract tooling workflows | CLI command architecture, install and upgrade flows, contract generation  |

## Cross-Cutting Pattern Docs

| Document                           | Read When                                                          | Why                                                                                           |
| ---------------------------------- | ------------------------------------------------------------------ | --------------------------------------------------------------------------------------------- |
| `core/platform-libraries.md`       | You are changing library/package boundaries                        | Package ownership and runtime responsibilities                                                |
| `core/files-transfer-patterns.md`  | You are changing the public files API or operation-native transfer | Contract-owned file metadata APIs, transfer-capable operations, and runtime helper boundaries |
| `core/state-patterns.md`           | You are changing the public shared state API                       | Named store declarations, runtime state semantics, admin inspection, and TTL behavior         |
| `core/kv-resource-patterns.md`     | You are changing KV buckets, keys, TTLs, or projections            | KV naming, TTL, and projection rules                                                          |
| `core/store-resource-patterns.md`  | You are changing service-owned blob store resources                | Store resource shape, runtime semantics, and auth boundaries                                  |
| `core/type-system-patterns.md`     | You are changing schemas, Result, or error modeling                | Shared type-system and validation rules                                                       |
| `core/service-development.md`      | You are implementing service code or service runtime ergonomics    | Service layout, lifecycle, jobs vs operations                                                 |
| `core/testing-patterns.md`         | You are adding, moving, or deleting tests for Trellis behavior     | Smallest real test boundaries, live integration discovery, and infrastructure                 |
| `core/observability-patterns.md`   | You are changing telemetry, correlation, health, or docs guidance  | Observability and request-correlation rules                                                   |
| `core/frontend-svelte-patterns.md` | You are changing Svelte frontend conventions                       | Trellis frontend state patterns                                                               |
| `core/capability-patterns.md`      | You are changing capability naming or deployment-role guidance     | Capability taxonomy and assignment guidance                                                   |

## Protocol, API, And Runtime Surface Docs

These documents define the public protocol, API, and runtime-facing surfaces.
Read them when you are implementing or reviewing library/runtime/codegen
ergonomics.

| Document                                       | Surface                     | Read When                                                                                                |
| ---------------------------------------------- | --------------------------- | -------------------------------------------------------------------------------------------------------- |
| `auth/auth-protocol.md`                        | Auth protocol surface       | Implementing auth callout, proofs, reply validation, or auth state model                                 |
| `auth/auth-api.md`                             | Auth public wire API        | Implementing `/auth/*`, `operations.v1.Auth.*`, `rpc.v1.Auth.*`, or auth events                          |
| `auth/trellis-auth.md`                         | Auth system/runtime design  | Implementing principals, login sessions, GrantBinding, context issuance, bootstrap, or runtime auth      |
| `auth/device-activation.md`                    | Device activation design    | Implementing known-device activation, connect info, or activation review flows                           |
| `operations/trellis-operations.md`             | Operations design           | Implementing caller-visible async workflows in TypeScript or Rust                                        |
| `jobs/trellis-jobs.md`                         | Jobs design                 | Implementing service-private background execution or jobs admin surfaces                                 |
| `contracts/trellis-idl.md`                     | Trellis IDL                 | Implementing IDL authoring, compilation, or generated TypeScript runtime ergonomics                      |
| `contracts/trellis-rust-contract-libraries.md` | Generated Rust libraries    | Implementing Rust generated SDKs, facades, descriptors, or runtime ergonomics                            |
| `core/state-patterns.md`                       | State design                | Implementing contract-owned state declarations, runtime state semantics, or migrations                   |
| `core/files-transfer-patterns.md`              | Files and transfer design   | Implementing service-owned files APIs and operation-native transfer behavior                             |
| `/api` in the guides site                      | Generated language API docs | Looking up exact TypeScript signatures, Rustdoc links, pending Rustdoc crates, or generated SDK surfaces |

## Suggested Read Paths

For ordinary app/service library usage, prefer `/guides/libraries/typescript`,
`/guides/libraries/rust`, and `/api` instead of the design docs.

### Implement Trellis operations in TypeScript

1. `operations/trellis-operations.md`
2. `auth/trellis-auth.md`
3. `contracts/trellis-api-participants.md`
4. `/api` for exact TypeScript signatures

### Implement Trellis operations in Rust

1. `operations/trellis-operations.md`
2. `auth/trellis-auth.md`
3. `contracts/trellis-api-participants.md`
4. `/api` for Rustdoc links

### Implement Trellis jobs in TypeScript

1. `jobs/trellis-jobs.md`
2. `core/service-development.md`
3. `operations/trellis-operations.md` only if the jobs attach to public
   operations
4. `contracts/trellis-api-participants.md` when changing job-owned resources,
   bindings, or provisioning surfaces
5. `/api` for exact TypeScript signatures

### Implement Trellis jobs in Rust

1. `jobs/trellis-jobs.md`
2. `core/service-development.md`
3. `operations/trellis-operations.md` only if the jobs attach to public
   operations
4. `contracts/trellis-api-participants.md` when changing job-owned resources,
   bindings, or provisioning surfaces
5. `/api` for Rustdoc links

### Work on type systems or errors

1. `core/type-system-patterns.md`
2. relevant subsystem design doc
3. `/api` for exact language API signatures when needed

### Add, move, or delete tests for Trellis behavior

1. `core/testing-patterns.md`
2. relevant subsystem design doc
3. `docs/src/routes/guides/releasing-trellis/+page.svx` for verification
   commands

### Work on KV resources or projections

1. `core/kv-resource-patterns.md`
2. relevant subsystem design doc

### Work on store resources

1. `core/store-resource-patterns.md`
2. `contracts/trellis-api-participants.md`
3. `/api` if the public runtime API changes

### Work on contract state

1. `core/state-patterns.md`
2. `contracts/trellis-api-participants.md`
3. `contracts/trellis-idl.md` when changing TS authoring
4. `/api` for exact TypeScript state helper signatures

### Work on files or transfer

1. `core/files-transfer-patterns.md`
2. `core/store-resource-patterns.md`
3. relevant subsystem design doc or `/api` for exact language signatures

### Work on service layout or runtime ergonomics

1. `core/service-development.md`
2. relevant subsystem design doc
3. `/api` for exact language signatures when needed

### Implement an installable service in TypeScript

1. `core/service-development.md`
2. `contracts/trellis-idl.md`
3. `contracts/trellis-api-participants.md`
4. `core/platform-libraries.md`
5. `/api` for exact TypeScript signatures

### Implement an activated device in TypeScript

1. `auth/device-activation.md`
2. `auth/trellis-auth.md`
3. `contracts/trellis-idl.md`
4. `contracts/trellis-api-participants.md`
5. `core/platform-libraries.md`
6. `/api` for exact TypeScript signatures

### Work on telemetry, docs, or request correlation

1. `core/observability-patterns.md`
2. relevant subsystem design doc

### Work on capability naming or deployment policy

1. `core/capability-patterns.md`
2. `auth/trellis-auth.md`
3. `contracts/trellis-api-participants.md`

### Change manifests, codegen, or discovery

1. `contracts/trellis-api-participants.md`
2. `contracts/trellis-idl.md` or `contracts/trellis-rust-contract-libraries.md`
3. relevant subsystem design doc
4. `/api` for exact generated SDK or runtime APIs

### Implement TypeScript participant/runtime surfaces

1. `contracts/trellis-idl.md`
2. `contracts/trellis-api-participants.md`
3. `/api` for exact TypeScript signatures

### Implement Rust participant/runtime surfaces

1. `contracts/trellis-rust-contract-libraries.md`
2. `contracts/trellis-api-participants.md`
3. `/api` for Rustdoc links

### Change auth or operation watch behavior

1. `auth/trellis-auth.md`
2. `auth/auth-protocol.md`
3. `operations/trellis-operations.md`
4. `/api` if the public language API changes

### Implement auth protocol or auth callout

1. `auth/trellis-auth.md`
2. `auth/auth-protocol.md`
3. `contracts/trellis-api-participants.md`

### Implement auth HTTP or RPC APIs

1. `auth/trellis-auth.md`
2. `auth/auth-api.md`
3. `auth/auth-protocol.md`
4. `auth/device-activation.md` if device activation is involved

### Implement TypeScript auth surfaces

1. `auth/trellis-auth.md`
2. `auth/auth-api.md`
3. `auth/auth-protocol.md`
4. `/api` for exact TypeScript signatures

### Implement Rust auth surfaces

1. `auth/trellis-auth.md`
2. `auth/auth-api.md`
3. `auth/auth-protocol.md`
4. `/api` for Rustdoc links

### Operate Trellis auth in production

1. `auth/trellis-auth.md`
2. `auth/auth-operations.md`
3. `auth/auth-protocol.md`

## Notes For AI And Reviewers

- load subsystem design docs for architecture
- use `/api` in the guides site for exact TypeScript and Rust API details
- load auth/contracts docs only when the task crosses those boundaries
- prefer task-specific reading paths over broad context dumps
- choose docs by participant kind (`service`, `device`, `app`, `agent`) and
  identity anchor (`web`, `cli`, `native`, `device-user`) rather than by repo
  folder name
- search for the exact headings `Minimal installable service example` and
  `Minimal activated device example` before inventing a new participant shape
