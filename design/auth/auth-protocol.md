# Trellis Authentication Protocol

Revision 7 is an unreleased clean cutover. The Rust protocol crate owns canonical
encoding and cryptographic verification; the Rust SDK consumes it directly and
TypeScript consumes its WASM build. The one exception is the
[per-message proof digests](#per-message-proof-digests), which TypeScript
computes natively against pinned vectors. Native IDL owns cross-service
contracts. This document describes boundaries and invariants, not a second
schema source.

TypeScript ships two WASM builds. Callers load the client build. Services and
devices load the provider build, a superset that adds request verification,
Operation record validation, provisioned admission and provider-side Transfer
and Live helpers. One process runs exactly one build.

## Selected installation and public verification metadata

The caller selects an installation's HTTPS Trellis origin. Explicit loopback
development is the only plaintext exception. Credential-free browser GETs can
read discovery and the issuer chain before the application is registered or its
CIMD document has been fetched. Public metadata does not grant application or
broker access. Portal cookies and client-bound token endpoints do not inherit
discovery's cross-origin policy.

Initial issuer trust comes from that authenticated selected origin or provisioned
installation trust. Broker-delivered public key bytes cannot create a trust root.
Subsequent keys require authenticated rotation links and monotonic sequence
checks. Retain installation identity and pinned trust across reconnects. A
rotated-out key authorizes live use until the link's signed `previousRetiredAt`,
one authority lifetime after the rotation, and only history after that. A
participant pinned at a later key learns that deadline from the signed
`issuerKey` answer's `liveUntil`. Auth
publishes an empty notice on `trellis.auth.issuers` after each rotation link and
each issuer revocation. On it a participant renews its own authority, then
re-reads the signed `issuerKey` state of every rotated-out key still inside its
overlap; a revoked key ends its overlap at once. The notice carries no trust:
participants act only on signed material.

Admitted clients may query independent signed verification metadata through
`trellis.auth.metadata.get`. Bind responses to the exact installation/account,
request subject, reply subject, nonce, and freshness window. Unavailability never
means authorization and cannot erase known revocations. Administrative authority
status remains a separate protected API.

## User OAuth and custody

Browser authorization uses authorization code with PKCE, exact redirect matching,
DPoP, and explicit capability consent. Previously unknown applications identify
themselves with HTTPS CIMD documents; Auth validates their retrieval and metadata
without requiring administrator registration. Registered Console eligibility is
distinct from arbitrary applications sharing a loopback origin.

The Trellis origin, where no application runs, owns login cookies, local/OIDC authentication, and
consent. An existing valid login can authenticate a new consent decision without
another password or provider login. Consent does not create missing account
entitlement or extend a login. It widens a retained grant only when the app names
that grant (`trellis_grant` on `/authorize`) and the user approves the request on
the consent screen. The grant must belong to the same user, client, binding,
DPoP key and login. Anything else is refused, never turned into a new grant.
Widening only adds: the grant keeps what it held, so its sessions renew into the
wider authority on the same refresh family and logical session. Revised
capability meaning and incompatible application APIs require current eligibility
and fresh consent.

Native authorization uses Device Code and durable Node/Rust credential custody;
browser custody uses IndexedDB. Bind every token exchange to the correct client,
grant, proof key, and selected installation. Refresh credentials are one-use:
transport retry must not replay a spent credential. Reuse a verified, unexpired
admission when valid rather than rotating custody to open another carrier.

## Provisioned CONNECT and device enrollment

Services and devices prove their provisioned identity credential, fresh runtime
session key/NKey, target installation/account, and validity window. Auth resolves
deployment, instance, participant, and accepted implementation from its records,
not client assertions. NATS authenticates the same proved runtime NKey.

Device enrollment is a separate identity-signed HTTP exchange at
`/auth/device/enroll`. Its `trellis.session-proof.v1` envelope binds the selected
origin, fixed enrollment route and purpose, identity key, runtime key, connection
and request identifiers, issue time, and complete canonical unsigned request.
Unknown request members remain signature-bound. Enrollment does not itself grant
runtime authority; provisioning/review and independently consented companion
assignment determine eligibility.

## Own authority, coverage, and renewal

Private signed admission binds the exact runtime key and broker attachment.
Replies use key-bound inboxes and bind the request nonce and subjects. Verify
installation/account, issuer trust, identity, lifetime, catalog/resource evidence,
and transport policy before promotion.

One logical owner retains verified authority and an ordered revocation mirror
across physical generations. Keep a healthy predecessor until replacement
snapshot verification completes. Actual coverage loss fences application use;
private maintenance may restore authenticated coverage and request fresh
authority using retained identity. Expired authority cannot authorize application
work. Cancelled renewal must settle before its replacement starts, and logical
shutdown cancels both maintenance and renewal. Signed revocations remain sticky.

## Requests, replies, and events

RPC, Operation starts, and Live openings use canonical API/action subjects and
shared API queue groups. There is no sticky caller-to-provider route selection.
Providers independently verify the caller's exact signed catalog, permissions,
generation, proof, and revocation state, and their own implementation eligibility.

Application proofs bind exact subjects, reply routing, payload, request identity,
and authority digest. A session-key header is checked against resolved signed
authority; it is not an independent trust assertion. Provider replies require
verified provider authority and request binding. A retained accepted request may
outlive ordinary authority replacement, but fresh signing requires current
authority and retained response validation still enforces its security checks.
Never automatically retransmit potentially executed application requests. A
broker no-responders reply proves no provider received the request, so callers
resend the same signed request, with its request ID and proof unchanged, for up
to five seconds within its deadline, with pauses doubling from 100 ms to one
second; this covers a provider that is reconnecting.

Events retain signed authority by digest. Historical verification uses original
JetStream publication time/sequence and the signed revocation cutoff, not replay
time. Preserve immutable issued authority and authenticated trust history.

## Authenticated verification material

Catalog snapshots and provider certificates are signed and content-addressed.
Issuer rotation re-signs each current catalog snapshot under the successor, so
live authority never references a catalog signed only by a retired key.
A participant authenticates each one once, when it is fetched: issuer
signature, record structure, and installation/account scope. It retains the
issuer key that authenticated it. Every later use rechecks only what can change:
the presented issuer key is that same key and is not revoked, the scope is the
same, and a certificate is within its validity window at the use time (live
time, or authenticated publication time for retained events). Issuer
revocation, retirement and session revocation are never cached as authorized.

## Per-message proof digests

Each request, response, event, Transfer frame and Live server message is signed
with Ed25519 over a 32-byte digest, and verified with strict verification. A
digest is SHA-256 over its components, each encoded as a 4-byte big-endian
length followed by its bytes. Text is UTF-8, counters are 8-byte big-endian
unsigned integers, times are 8-byte big-endian signed Unix seconds, and
digests are the raw 32 bytes of their canonical unpadded base64url form.
`H(x)` is SHA-256 of the exact received or sent bytes.

| Message                 | Components, in order                                                                                                                                                                                                                                                     |
| ----------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Request                 | `trellis.message.request.v1`, authority digest, API id, API generation, accepted revision, action, subject, reply subject (empty when none), request id, issued-at, `H(payload)`                                                                                         |
| Event                   | `trellis.message.event.v1`, authority digest, API id, API generation, accepted revision, action, subject, event id, event time, `H(payload)`                                                                                                                             |
| Response                | `trellis.message.response.v1`, installation id, NATS account, provider authority digest, provider certificate digest, API id, API generation, action, request authority digest, request id, request proof digest, subject, `success` or `error`, `H(payload)`, issued-at |
| Transfer frame          | `trellis.transfer-frame.v1`, canonical JSON of the frame descriptor, `H(payload)`                                                                                                                                                                                        |
| Transfer provider proof | `trellis.transfer-server-proof.v1`, context digest, subject, Transfer frame digest                                                                                                                                                                                       |
| Live provider proof     | `trellis.live-server-proof.v1`, context digest, subject, `H(body)`                                                                                                                                                                                                       |

A frame descriptor holds `transferId`, `direction`, `sequence` as a decimal
string, `kind`, and `terminal` for EOF only. A caller's Transfer request proof
signs the request digest whose payload is the 32-byte frame digest.

The Rust protocol crate is the reference implementation. TypeScript computes
these digests natively because they run on every message; the vectors in
`integration/fixtures/protocol/message-digests` are checked by both. Field
validation and every authorization decision stay in the shared verifier: a
signer only signs its own messages, and the verifier recomputes the digest.
Verifiers take the payload's SHA-256 rather than the payload: each SDK hashes
the bytes it already holds, natively, and payload bytes never cross into WASM.
TypeScript computes DATA-frame digests natively and the few completion, EOF,
control and signal frames through the shared protocol.

## Live, Transfer, Operations, and Jobs

Established Live and Transfer interactions retain authenticated owner-directed
subjects and generation ownership. Fresh openings are fenced until usable
authority is promoted; accepted work is not replayed during handover.

Transfer grants bind their actual owner, direction, allowed object/bytes, and
expiry. Completion, end acknowledgement, and cancellation preserve those bindings
across physical retirement. Operation references retain durable deployment and
executor ownership; signals and progress become publicly visible only after
persistence. Jobs retain broker receipts, coordination ownership, and declared
resource identity independently of physical transport generations.

See [Trellis Auth](trellis-auth.md) for policy/custody ownership,
[Operations](../operations/trellis-operations.md), and
[Jobs](../jobs/trellis-jobs.md) for durable workflow semantics.
