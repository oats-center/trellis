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

1. a contract-owned operation accepts JSON input and declares `upload` (staged)
   or `stream` (direct) transfer support
2. the caller configures the operation input and sends bytes through the
   generated transfer-capable operation helper for that language
3. callers do not start the same send-transfer operation first and attach bytes
   later
4. for `upload`, the provider awaits the runtime's durable transfer completion
   signal and continues with service-owned processing; for `stream`, the
   running handler reads the bytes as they arrive

Example:

- `Documents.Files.Upload`

Rules:

- send transfer is modeled as a capability of an operation; upload/file-ingest
  remains operation-native
- for `upload`, the platform stages bytes independently of any
  participant-authored Store mapping before the handler reads them; for
  `stream`, nothing is staged and the handler decides where the bytes go
- typed operation input carries declared transfer metadata
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

- `upload` marks an operation that accepts caller-sent bytes, durably staged
  before its handler reads them
- `stream` marks an operation that accepts caller-sent bytes delivered directly
  to its running handler; it excludes `upload`
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
- providers that expose `upload` operations MUST await the provider-side
  durable transfer completion primitive before treating bytes as durably
  available; completion resolves only after the transfer endpoint has accepted
  EOF and durably staged the object, or fails with a transfer error if durable
  storage was not reached
- providers that expose `stream` operations read the caller's bytes through a
  backpressured stream handed to the running handler exactly once; bytes arrive
  before the object is verified, so a handler must be able to abandon partial
  work when its stream ends in an error
- receive grants are served from a service store key or from any provider byte
  source whose size and SHA-256 digest are declared up front; the grant binds
  the declared metadata and a source that ends differently fails integrity

### Transfer v1 Session And Wire Behavior

Transfer uses the single `trellis.transfer.v1` wire format, with no fallback or
version negotiation. IDL `upload` and `download` declarations remain unchanged.
High-level generated helpers consume runtime-issued grants; applications do not
construct subjects or resolve physical storage.

A grant binds its transfer id, direction, expiry, provider and consumer logical
connection/session identities, exact data/control/signal subjects, and
frame/byte limits. Receive grants additionally bind logical `FileInfo`. The
subject families use the same canonical identity-token encoding as Live:

```text
transfer.v1.upload.data.<b64(P)>.<b64(C)>.<T>
transfer.v1.download.data.<b64(P)>.<b64(C)>.<T>
transfer.v1.control.<b64(P)>.<b64(C)>.<T>
transfer.v1.signal.<b64(P)>.<b64(C)>.<T>
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
- sequence, control-sequence, and credit cursors are checked u64 values in
  canonical decimal strings; object sizes (`size` in completion, EOF, and
  logical `FileInfo`) are JSON integers bounded by 2^53 - 1, like every other
  Trellis size field
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

Frames are capped at 1 MiB; effective frame size also accounts for both peers'
NATS payload limits and a 4 KiB header reserve. Outstanding windows are chosen
by the receiver, up to protocol maxima of 1,024 frames and 64 MiB. The default
offer is 64 frames and 16 MiB, from a 256 MiB process budget:

- the receiver admits its window from one process-wide receive budget before
  any frame can arrive, and releases it when the transfer ends on any path;
  admission shrinks an offer to what remains and refuses a transfer when less
  than one frame fits, so a process never overcommits its budget
- for an upload the provider receives: its grant carries the admitted window
- for a download the consumer receives: its activation declares the admitted
  window, and the provider sends within the smaller of it and the grant
- `activated` echoes the effective window, never larger than the grant; the
  sender obeys it
- both SDKs share one offer policy, compiled from the protocol crate

The consumer also reports its receive frame limit at activation. Credit is sent after eight newly consumed frames, 1
MiB of newly consumed bytes, or 25 ms of pending consumption, with final credit
during terminal handling. These are runtime protocol limits, not application
knobs.

When a frame's signed context digest matches the retained peer guard, reuse its
verified immutable material rather than preparing the same replacement again.
Every frame still verifies its signature/proof and checks current authority,
expiry, revocation, coverage and issuer trust. A changed digest takes the full
replacement path and commits only after successful frame authentication.

### Proposed: One-Pass Frame Proofs (Not Implemented)

Status: proposal for review; Transfer v1 currently binds each DATA proof to
SHA-256 of that frame's payload.

Every DATA byte is hashed twice at each endpoint: once for the compact frame
digest its request or server proof signs, and once into the object's running
SHA-256 that completion and EOF verify. Both SDKs now hash DATA natively, so
the second pass is pure duplicate work: it roughly doubles per-byte hashing at
each endpoint.

Proposal: bind each frame's proof to the object's running digest after that
frame instead of a separate payload hash. Both endpoints already maintain the
running hash; finalizing a copy of its state costs one block, not a pass over
the payload. Sequence validation already requires strictly ordered frames, so
the receiver checks frame N against its own running state after N frames.

- Integrity is unchanged: the final frame's proof covers the whole object, and
  every earlier proof covers its exact prefix.
- A frame cannot be verified out of order or in isolation; v1 already rejects
  out-of-order DATA.
- It changes the frame digest's definition, so it is a v1 wire change in both
  SDKs and the pinned message-digest vectors.
- TypeScript needs a running-hash copy: `node:crypto` `Hash.copy()` provides
  it; the portable fallback needs an equivalent incremental state copy.

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

A `stream` upload has no staging boundary. After verifying the completion, the
provider waits until the handler has consumed every frame, records the verified
delivery in the Operation (fenced, like the staged commit), and only then ends
the handler's stream and sends signed `committed`. The handler therefore never
observes end of file for bytes whose delivery is unrecorded, and the caller's
success means the handler received every verified byte, not that anything was
persisted. A transfer failure, cancellation or ownership loss ends the
handler's stream with an error. An execution that loses its stream cannot be
replayed: recovery fails the Operation, and the caller starts a new one.

| Boundary                                                                         | Meaning                                       | Observed by                                       |
| -------------------------------------------------------------------------------- | --------------------------------------------- | ------------------------------------------------- |
| delivered                                                                        | receiver accepted bytes through its stream    | provider handler (`stream`) or backend (`upload`) |
| verified                                                                         | final sequence, size and digest checked       | both endpoints, via signed completion and EOF     |
| application completed                                                            | handler finished, including any durable write | Operation output or RPC result                    |
| Cancellation acknowledged before the durable commit handoff aborts storage       |
| ingress and cannot expose a committed staged upload. Once that handoff begins,   |
| settle the in-flight commit rather than acknowledge an uncommitted cancellation; |
| cancellation after durable commit cannot undo it. A lost commit response is not  |
| proof of rollback: retain staged bytes until reconciliation confirms commit or a |
| newer uncommitted revision fences the uncertain write. Interrupted uploads       |
| restart from byte zero, without partial replay or resumption.                    |

A download streams through the runtime's store boundary or from a
provider-supplied source, splitting arbitrary chunks into bounded frames while
credit is available. A provider source is held to the size and digest its grant
declared. Signed EOF follows
all DATA on the same download subject. Both endpoints verify final logical
size/digest, and the consumer sends final credit/end acknowledgement before
releasing session ownership. No per-frame pull RPC remains.

Senders in both SDKs form frames only from bytes that are already ready, up to
the negotiated frame limit. Only the first read of a frame may wait: an
available prefix is sent rather than waiting for a full frame or EOF, so slow
producers still make progress, and source reads stay cancellation-safe inside
the session. Source prefetch is bounded to about one formed frame. Streaming
source blocks are snapshotted before advancing an iterator that may reuse its
scratch buffer. Both SDKs fill ready frames up to the negotiated limit. Exact buffer and frame-formation sizes are private implementation
policy, documented in source, not public tuning fields. Store-reader
cancellation and teardown remain session-owned; an application's
uninterruptible iterator cannot pin SDK cleanup. Producer-provided backing
buffers are not an SDK allocation guarantee.

### Store Backing

The Rust JetStream writer fills each chunk to 128 KiB across short reads (only
the final chunk may be shorter) and pipelines at most eight of them (1 MiB of
outstanding payload per writer). It preserves native Object Store
metadata and SHA-256 and awaits every chunk ACK before publishing metadata.
Source errors and pre-commit cancellation retain the admitting transport through
owned partial-chunk cleanup. Once metadata publication can have escaped, chunks
are retained and errors remain commit-indeterminate; confirmed commits with failed
old-version cleanup remain distinguishable from failed writes. A chunk ACK is the
configured JetStream durability boundary, not an additional fsync guarantee.

Rust and TypeScript readers retain finite byte-capped batches, fetched on
application demand. Paused readers must not drain the object or discard queued
chunks on an SDK expiry timer. Consumer loss recreates from the last yielded
stream sequence, not the broker's prefetched cursor. TypeScript queries consumer
INFO only to reconcile a failed or empty pull. Independent overlapping fetch
inboxes cannot be assumed to receive contiguous ranges from one consumer.

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

- for `upload`, transfer success means `bytes stored`, not `workflow finished`;
  a durable transfer completion signal means final sequence/size/digest were
  verified, platform staging completed, and the Operation transfer state was
  durably committed; only then may the handler begin processing
- for `stream`, transfer success means every verified byte reached the running
  handler and the delivery was recorded; durability is whatever the handler
  does with the bytes
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

Both runtimes record the same transfer metrics with the same labels:
`trellis.direction` is `upload` or `download` and `trellis.side` is `client` or
`provider` (`store` for storage refill waits). Each endpoint records one
`trellis.transfer.duration` per transfer and counts what it sends or verifies
on receipt:

- `trellis.transfer.frames` and `trellis.transfer.payload.bytes` for DATA
  frames and their raw payload;
- `trellis.transfer.wire.bytes` for serialized DATA messages, the NATS header
  block (descriptor, proof and authority headers) plus payload;
- `trellis.transfer.control.bytes` for every other serialized Transfer message:
  controls, signals, EOF, activation, completion and cancellation;
- `trellis.transfer.credit.controls` for cumulative credit messages.

Protocol byte overhead is `(wire + control - payload) / payload`. Broker
protocol lines and TCP framing are outside these counters.
`trellis.transfer.buffered.bytes` follows retained raw payload ownership and is
released on consumption or cleanup. Labels never include transfer ids,
subjects, principals, object keys, or arbitrary errors. There are no per-frame
spans.

`trellis.transfer.wait.duration` separates blocked sender credit waits from
streaming store refill waits. Refill measures awaiting the next backend message,
including buffered delivery and local dispatch; it is not pure network RTT.

### Non-Goals

This document does not define:

- direct client resolution of `resources.store`
- HTTP send/receive transfer endpoints
- arbitrary query/filter semantics for file listing
- shared writable store bindings across services
- a global cross-service files admin query surface in v1
