---
title: Files Transfer Patterns
description: Public contract-owned files APIs and operation-native transfer patterns over NATS.
order: 46
---

# Design: Files Transfer Patterns

## Prerequisites

- [trellis-patterns.md](./trellis-patterns.md) - Trellis architecture and
  communication model
- [store-resource-patterns.md](./store-resource-patterns.md) - service-owned
  blob-store resources
- [../contracts/trellis-api-participants.md](./../contracts/trellis-api-participants.md) -
  contract ownership and permission rules

## Context

Services often need to expose file-like behavior to apps and peer services
without exposing raw store bindings.

Examples:

- transfer an attachment into service-owned storage
- receive a generated export from a service-owned transfer endpoint
- inspect file metadata before deciding whether to fetch bytes
- delete a stored object through the owning service's business rules

`resources.store` solves service-owned blob persistence. It does not by itself
define a public API for callers.

`Files` is the public pattern that sits on top of service-owned `store`
resources.

## Scope

This document defines the public Trellis files pattern:

- which actions stay ordinary contract RPCs
- how byte transfer is modeled as one runtime concept with caller-facing
  directions
- how services back the public files surface with service-owned `store`
- how callers and providers receive per-chunk transfer progress

It does not define a global admin UI, cross-service shared raw store access, or
ordinary language-library walkthroughs. TypeScript and Rust usage examples
belong in `/guides/libraries/typescript`, `/guides/libraries/rust`, and the
generated API references linked from `/api`.

## Design

### Ownership Model

Rules:

- the owning service keeps direct access to `service.store.<alias>`
- clients and peer services do not resolve raw store bindings
- public file behavior is exposed through the owning service's contract surface
- if another service needs file access, it uses the owning service's `Files.*`
  API rather than the raw store binding
- `Files` is the public interface to `store` in the same way that contract-owned
  operations are the public async workflow interface for service-private
  execution machinery

### Public Files API Split

There are three common categories of file behavior.

#### Metadata and control RPCs

File metadata and small control actions remain ordinary contract-owned JSON
RPCs.

Examples:

- `Documents.Files.List`
- `Documents.Files.Head`
- `Documents.Files.Delete`

Rules:

- these methods use normal Trellis RPC auth and capability checks
- they return JSON payloads and `Result`-modeled failures
- `list` is a bounded keyset query rather than an arbitrary metadata query
  language: callers send `{ prefix?: string; cursor?: string; limit?: number }`
  and services return `{ entries, nextCursor? }`
- entries are ordered by object key; the default limit is `100` and the maximum
  accepted limit is `500`
- `nextCursor` is an opaque continuation bound to the normalized query; callers
  must not inspect or construct it, and malformed cursors or cursors reused with
  a different prefix are rejected
- an omitted `nextCursor` means the listing is exhausted
- listing is live rather than snapshot-based, but inserting or deleting keys at
  or before the continuation key does not duplicate or skip entries that already
  followed that key

#### Send transfer operations

When the caller sends bytes to the service, file bytes use an operation-native
model:

1. a contract-owned operation accepts JSON input and declares `upload` transfer
   support
2. the caller configures the operation input and sends bytes through the
   generated transfer-capable operation helper for that language
3. callers do not start the same send-transfer operation first and attach bytes
   later
4. the provider awaits the runtime's durable transfer completion signal and
   continues with service-owned processing

Example:

- `Documents.Files.Upload`

Rules:

- send transfer is modeled as a capability of an operation; upload/file-ingest
  remains operation-native
- the platform stages upload bytes independently of any participant-authored
  Store mapping; typed operation input carries declared transfer metadata
- the actual byte movement still uses raw NATS chunk traffic rather than
  JSON/base64 RPC payloads
- the transfer protocol is Trellis-owned runtime machinery, not a
  service-specific public protocol surface
- callers observe transport progress through operation watch transfer events,
  language-library transfer callbacks where available, and durable snapshot
  state
- providers observe the same transport progress through provider-side transfer
  update streams

#### Receive transfer RPCs

When the caller receives bytes from the service, metadata and control remain a
contract-owned RPC and the RPC returns a receive transfer grant.

Example:

- `Documents.Files.Download`

Rules:

- the RPC declares `download` transfer support
- the RPC response contains a Trellis transfer grant, not raw store binding
  details
- callers consume the grant through the language runtime's receive-transfer
  helper for large streams or buffered reads
- service code decides whether and how the requested object maps to a
  service-owned store entry
- product-facing docs may still use words such as upload and download, but the
  platform API should prefer transfer, send, and receive language where possible

### Native IDL Transfer Declaration

Transfer capability is declared in native Trellis IDL.

Example:

```trellis
operation Documents.Files.Upload {
  input FilesUploadRequest;
  progress FilesUploadProgress;
  output FilesUploadResult;
  upload;
}

rpc Documents.Files.Download {
  input FilesDownloadRequest;
  output FilesDownloadResponse;
  download;
}
```

Rules:

- `upload` marks an operation that accepts caller-sent bytes
- `download` marks an RPC whose response may carry a receive-transfer grant
- upload staging is platform-owned and is not configured by mapping the
  participant-authored operation to a store alias
- typed request and response models carry the domain metadata declared by the
  contract
- the service still owns any later mapping to a service store and any lookup,
  authorization, retention, or audit policy

### Runtime Helper Boundaries

Rules:

- language runtimes expose send transfer through generated transfer-capable
  operation helpers, not through a follow-up method on an already-started
  operation reference
- service-issued receive grants are consumed through root transfer helpers that
  expose streaming and buffered reads where the language supports them
- supported body forms, exact method names, callback shapes, and error wrapper
  types are language-library API details documented in
  `/guides/libraries/typescript`, `/guides/libraries/rust`, and `/api`
- metadata actions such as list, head, and delete remain ordinary typed RPC
  calls on the contract client
- all languages preserve the same chunk-progress semantics and Result-style
  expected failure model
- providers that expose send-transfer operations MUST await the provider-side
  durable transfer completion primitive before treating bytes as durably
  available; completion resolves only after the transfer endpoint has accepted
  EOF and durably staged the object, or fails with a transfer error if durable
  storage was not reached

### Wire Behavior

Rules:

- byte transfer uses raw NATS messages, not JSON/base64 wrappers
- request signing still uses session-bound proof headers
- send transfer sends ordered chunk requests on a runtime-owned transfer subject
  and receives per-chunk acknowledgements
- receive transfer streams ordered chunks from a service-owned transfer endpoint
  to the caller; callers use language-runtime transfer helpers with the returned
  receive-transfer grant rather than resolving raw store bindings
- the runtime emits one transfer update per acknowledged chunk on both caller
  and provider sides
- chunk sequence and end-of-stream markers are runtime protocol details owned by
  Trellis

This mirrors the general style used by NATS object store: raw chunk payloads
plus separate metadata/control frames.

### Store Backing

Rules:

- canonical v1 file persistence lands in the owning service's `resources.store`
- services may move staged uploads into one or more store aliases for
  service-owned persistence; declaring upload transfer support does not map an
  operation to a participant-authored store
- services may later mirror or copy files to external systems, but `Files` does
  not depend on those backends
- `Files` does not imply shared raw store access across services
- receive grants expose bytes from a service-owned endpoint; they do not expose
  or delegate raw store bindings
- once transfer completes, service code works with the staged object through the
  owning service's normal store API; exact helper names belong in `/api`
- Trellis should make staged transferred objects easy for the owning service to
  access, but it does not impose post-transfer processing policy on the service
  author

### Transfer Plus Operations

For caller-visible file-processing workflows, the recommended pattern is:

1. a contract-owned operation declares transfer support
2. the caller starts the operation and begins watching it, either directly or
   through language-runtime transfer helper callbacks where available
3. the caller sends bytes through the generated transfer-capable operation
   helper
4. the provider awaits the runtime's durable transfer completion signal and
   updates business progress or enqueues follow-up work
5. the operation completes when the service-owned workflow completes

Rules:

- transfer success means `bytes stored`, not `workflow finished`
- a durable transfer completion signal means all chunks were accepted, EOF was
  received, and platform staging completed; service code can then apply its own
  persistence and processing policy
- use runtime-owned transfer events or language-runtime transfer callbacks for
  progress bars and service-authored progress calls for domain milestones
- use operations for caller-visible progress and final results
- use jobs for service-private execution, retries, and background processing
  after the bytes are stored

### Events

If the owning service exposes file lifecycle events, they should be
contract-owned `Files.*` events rather than raw store events.

Rules:

- public file events represent the service's contract view of file changes
- direct store writes performed by the service may still be normalized into
  public `Files.*` events
- the public abstraction stays `Files`, not backend-native store notifications

### Non-Goals

This document does not define:

- direct client resolution of `resources.store`
- HTTP send/receive transfer endpoints
- arbitrary query/filter semantics for file listing
- shared writable store bindings across services
- a global cross-service files admin query surface in v1
