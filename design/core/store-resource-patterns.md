---
title: Store Resource Patterns
description: Service-owned opaque blob-store resource shape, runtime semantics, and authorization boundaries.
order: 45
---

# Design: Store Resource Patterns

## Prerequisites

- [trellis-patterns.md](./trellis-patterns.md) - Trellis architecture and
  communication model
- [kv-resource-patterns.md](./kv-resource-patterns.md) - related service-owned
  resource patterns and naming guidance
- [../contracts/trellis-api-participants.md](./../contracts/trellis-api-participants.md) -
  canonical contract and binding model

## Context

Some services need a place to store large opaque values that do not fit well in
RPC payloads or typed KV entries.

Examples:

- temporary caller-sent files awaiting processing
- generated exports or reports before delivery
- intermediate binary artifacts produced between workflow steps
- service-local attachments that are not modeled as typed records

Today Trellis already has service-owned resources such as `kv` and first-class
`jobs`. `store` should follow the same ownership pattern while exposing a
blob-oriented runtime surface instead of a typed record API. Services do not
declare arbitrary stream resources in v1; subsystem streams are provisioned by
the owning runtime feature, such as jobs or operations.

## Scope

This document defines the native `store` resource shape, its service-owned
lifecycle, binding semantics, and runtime invariants.

Caller-visible file transfer is defined separately in
[files-transfer-patterns.md](./files-transfer-patterns.md).

## Design

### Definition

`resources.store` is a service-owned opaque blob store.

Rules:

- each store alias belongs to exactly one installed service contract
- only the owning service resolves the binding for a store alias
- the contract surface must not expose backend-native object-store terminology
  or management knobs
- values are opaque bytes plus small metadata, not typed JSON records
- services discover stores through normal resource bindings rather than through
  cloud-management credentials
- deployment reconciliation creates, updates, or removes Trellis-owned stores
  and records observed resource evidence used by context issuance

An IDL `store` declaration is intended for service-local and service-owned
binary data. It is not a shared public data plane and it does not change the
ownership rules used by `kv`.

### Contract Shape

Example:

```trellis
service Documents {
  store uploads {
    title "Uploads";
    description "Temporary uploaded files awaiting processing.";
    ttl 1d;
    desired_max_object 64MiB;
    desired_max_total 10GiB;
  }
}
```

Rules:

- store aliases are logical names chosen by the service author
- aliases are stable API surface for the service runtime
- the native IDL resource fields are `ttl`, `desired_max_object`, and
  `desired_max_total`; `ttl 0` means no automatic expiry
- `desired_max_object` and `desired_max_total` are planning hints only; they do
  not grant approval, determine readiness, configure an enforced limit, or
  report an actual limit
- contract declarations request logical stores, and Trellis chooses the concrete
  physical identity during reconciliation
- Trellis validates store declarations from the presented contract, but physical
  store identity is scoped to the deployment and contract lineage rather than
  the digest so compatible service updates preserve objects

### Reconciliation And Ownership

Rules:

- reconciliation may create a missing Trellis-owned store or update compatible
  operational settings such as retention
- a physical store is Trellis-owned only when its ownership evidence matches the
  resource being reconciled
- an existing foreign store is never adopted, modified, or treated as a
  successful binding
- resource removal destroys only a store with matching Trellis ownership
  evidence; a foreign store at the same physical identity is never destroyed
- changes that would weaken retention or otherwise invalidate existing objects
  require explicit administrative handling rather than silent reconciliation

### Binding Shape

Service bindings should expose effective installed limits rather than only
requested values.

Example binding payload:

```ts
type StoreResourceBinding = {
  name: string;
  ttlMs: number;
  maxTotalBytes?: number;
  maxObjectBytes?: number;
};
```

Rules:

- `name` is an opaque physical identifier chosen by Trellis
- bindings stay keyed by logical alias so service code remains stable across
  environments
- only successfully provisioned or bound store aliases appear in
  `bindings.store`
- bindings expose only the information the service runtime needs to use the
  resource safely
- `ttlMs`, `maxTotalBytes`, and `maxObjectBytes` report observed or effective
  runtime limits, not the declaration's desired planning values
- bindings include `maxTotalBytes` only when a finite total-store limit is
  actually observed or enforced
- bindings include `maxObjectBytes` only when the runtime write path actually
  enforces a finite per-object limit; it must not be inferred from
  `desired_max_object`
- bindings must not expose operator or platform management credentials

### Runtime Semantics

The store runtime surface should mirror the KV runtime style as closely as store
semantics allow.

Rules:

- all failable public store APIs return `Result`
- generated service handles resolve a higher-level runtime object from the
  installed binding
- `create(...)` follows KV `create(...)` semantics and fails if the key already
  exists
- `put(...)` follows KV `put(...)` semantics and overwrites the current object
  for that key
- `get(...)` returns an entry object rather than only raw bytes so metadata is
  available without a second lookup
- `waitFor(...)` polls `get(...)` until the object appears, then returns the
  same `TypedStoreEntry` shape a direct `get(...)` would have returned
- `waitFor(...)` remains a store primitive rather than a policy helper: it does
  not read, stream, move, or delete bytes on the caller's behalf
- listing accepts `{ prefix?: string; cursor?: string; limit?: number }` and
  returns `{ entries, nextCursor? }`; entries are ordered by object key
- the default list limit is `100`, the maximum accepted limit is `500`, and
  listing never exposes an unbounded mode
- `nextCursor` is opaque and bound to the normalized prefix query; malformed
  cursors and cursors reused with a different prefix are rejected
- an omitted `nextCursor` means the listing is exhausted
- listing is live rather than snapshot-based; keyset continuation means inserts
  or deletes at or before the previous continuation key do not duplicate or skip
  entries that already followed it
- `stream()` is the primary body-access path for large values; `bytes()` is a
  convenience helper
- streaming writes enforce an effective `maxObjectBytes` limit while bytes flow
  when that limit is present in the binding
- successful operations expose logical metadata only; physical bucket names,
  object identifiers, and chunk subjects remain runtime internals

### Object Metadata Model

Stores hold opaque bytes plus small metadata.

Example info shape:

```ts
type StoreInfo = {
  key: string;
  size: number;
  updatedAt: string;
  digest?: string;
  contentType?: string;
  metadata: Record<string, string>;
};
```

Rules:

- metadata is limited to string pairs in v1
- metadata should stay small and descriptive rather than becoming a secondary
  document database
- info surfaces should expose Trellis-level semantics such as `key`, `size`, and
  `updatedAt` rather than backend-specific chunk or object identifiers

### Key and Retention Rules

Rules:

- store keys are logical object keys within one store alias
- keys may be path-like and may include `/`
- keys are exact-match identifiers; prefix matching is only for `list(...)`
- `ttl` is declared in native IDL, and materialized bindings expose effective
  retention as `ttlMs`
- deployments may choose limits according to platform policy, but bindings must
  report only the effective or observed limits actually applied

### Authorization

Stores follow the same service-owned authorization model as other resource
bindings.

Rules:

- installed store bindings may derive additional runtime permissions needed to
  use the backing implementation
- those permissions are scoped to the installed physical store binding, not to
  general cloud-management APIs
- store-derived permissions remain service-local to the owning installed
  contract binding
- a backing implementation may require both publish and subscribe permissions;
  the contract surface still remains backend-agnostic

### Non-Goals

This document does not define:

- direct client access to store bindings
- caller-visible send or receive transfer session protocols
- multi-owner or shared write access across services
- backend-specific features such as links, sealing, or chunk-size tuning
- a typed JSON value model; use `resources.kv` for that

### Relationship To Files Transfer

Trellis file transfer uses `store` as the canonical v1 backing storage.

That does not change the rules in this document:

- `resources.store` remains service-owned
- non-owner clients do not resolve store bindings
- file transfer authorization still begins with explicit contract-owned
  `Files.*` APIs from the owning service, such as send-transfer operations and
  receive-transfer grants returned by RPCs or operations
- the public abstraction is `Files`; `store` remains the service-owned backing
  capability
- receive transfer grants must not be treated as raw store delegation; they are
  scoped runtime grants for bytes exposed by the owning service
