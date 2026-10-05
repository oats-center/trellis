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
- how callers and providers observe consumed-byte progress and durable
  completion

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
- all languages preserve the same consumed-byte progress semantics and
  Result-style expected failure model; durable Operation progress may be
  coalesced
- providers that expose send-transfer operations MUST await the provider-side
  durable transfer completion primitive before treating bytes as durably
  available; completion resolves only after the transfer endpoint has accepted
  EOF and durably staged the object, or fails with a transfer error if durable
  storage was not reached

### Transfer v2 Session And Wire Behavior

Transfer uses `trellis.transfer.v2`, with no v1 serving, fallback, or
negotiation. IDL `upload` and `download` declarations remain unchanged.
High-level generated helpers consume runtime-issued grants; applications do not
construct subjects or resolve physical storage.

A grant binds its transfer id, direction, expiry, provider and consumer logical
connection/session identities, exact data/control/signal subjects, and
frame/byte limits. Receive grants additionally bind logical `FileInfo`. The
subject families use the same canonical identity-token encoding as Live:

```text
transfer.v2.upload.data.<b64(P)>.<b64(C)>.<T>
transfer.v2.download.data.<b64(P)>.<b64(C)>.<T>
transfer.v2.control.<b64(P)>.<b64(C)>.<T>
transfer.v2.signal.<b64(P)>.<b64(C)>.<T>
```

Rules:

- each endpoint subscribes to exact session subjects, never its permission
  wildcards; provider subscriptions exist before the grant is returned
- consumer receive subscriptions exist before authenticated activation; DATA
  starts only after the consumer verifies the provider's signed `activated`
  signal
- the session pins its admitting transport generation until owned work settles;
  authority growth does not migrate it, and physical disconnect ends it
- DATA payloads are raw bytes, never JSON/base64; ordered sequences start at
  `1`, while cursor `0` means no DATA received or consumed
- checked u64 wire counters use canonical decimal strings
- upload frames carry session-bound request proofs over a compact digest binding
  transfer id, direction, sequence, control discriminator, and payload hash;
  there is no payload-sized proof copy or per-DATA request/reply
- provider frames/signals use a separate transfer-specific proof domain binding
  the actual subject, current covered context, pinned identity, and payload
  hash; Live's server-proof domain is not reused
- controls use the exact signal subject as reply, with authenticated monotonic
  control sequences and cumulative cursors
- senders obey both outstanding-frame and outstanding-byte windows; received
  credit does not release capacity, only consumed credit does
- upload consumption means the backend reader consumed the entire frame;
  download consumption means the destination accepted the frame through its
  write/Web Streams backpressure path
- credit is cumulative and coalesced, not a blocking exchange per frame; a
  verified sequence gap, impossible credit, or exceeded window ends the transfer

The initial protocol policy caps frames at 1 MiB, outstanding frames at `16`,
and outstanding bytes at 4 MiB. Effective frame size also accounts for both
peers' NATS payload limits and a 4 KiB header reserve. The consumer reports its
receive limit at activation. Credit is sent after eight newly consumed frames, 1
MiB of newly consumed bytes, or 25 ms of pending consumption, with final credit
during terminal handling. These are runtime protocol limits, not application
knobs.

### Flow Control Is Not Durability

An upload's authenticated completion follows all DATA on the same upload data
subject, declaring final sequence, size, and SHA-256 digest. An empty upload has
final sequence `0`. The provider independently verifies sequence continuity,
size, and digest, closes backend input cleanly, awaits backend write completion,
and validates its returned logical metadata. For an Operation upload, the fenced
durable Operation transfer commit must also succeed before the provider sends
signed `committed`. Only verified `committed` resolves caller upload success.

```text
received -> consumed/credited -> backend committed -> Operation committed
```

Credit is bounded network backpressure, never evidence of durable persistence.
Cancellation acknowledged before the durable commit handoff aborts storage
ingress and cannot expose a committed staged upload. Once that handoff begins,
settle the in-flight commit rather than acknowledge an uncommitted cancellation;
cancellation after durable commit cannot undo it. A lost commit response is not
proof of rollback: retain staged bytes until reconciliation confirms commit or a
newer uncommitted revision fences the uncertain write. Interrupted uploads
restart from byte zero, without partial replay or resumption.

A download streams through the runtime's store boundary, splitting arbitrary
backend chunks into bounded frames while credit is available. Signed EOF follows
all DATA on the same download subject. Both endpoints verify final logical
size/digest, and the consumer sends final credit/end acknowledgement before
releasing session ownership. No per-frame pull RPC remains.

### Store Backing

Rules:

- physical persistence stays behind runtime store streaming abstractions;
  backend-specific protocols and identities never appear in transfer grants
- services may move staged uploads into one or more store aliases for
  service-owned persistence; declaring upload transfer support does not map an
  operation to a participant-authored store
- the current JetStream adapter and future local/cloud adapters implement the
  same storage-neutral byte-stream semantics; `Files` does not define itself in
  terms of any one backend
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
- a durable transfer completion signal means final sequence/size/digest were
  verified, platform staging completed, and the Operation transfer state was
  durably committed; only then may the handler begin processing
- durable transfer progress is monotonic and coalesced from backend-consumed
  bytes; `chunkIndex` and `chunkBytes` describe the latest credited frame
  represented, not a promise of one watch event per frame; final persisted
  `transferredBytes` is exact
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

### Transfer Observability

Both runtimes retain `trellis.transfer.duration` and
`trellis.transfer.wire.bytes`, and record `trellis.transfer.frames`,
`trellis.transfer.credit.controls`, and `trellis.transfer.buffered.bytes` from
the endpoint that owns the actual work. Buffered bytes follow retained raw
payload ownership and are released on consumption or cleanup. Labels are bounded
logical direction/side/outcome values, never transfer ids, subjects, principals,
object keys, or arbitrary errors. There are no per-frame spans.

### Non-Goals

This document does not define:

- direct client resolution of `resources.store`
- HTTP send/receive transfer endpoints
- arbitrary query/filter semantics for file listing
- shared writable store bindings across services
- a global cross-service files admin query surface in v1
