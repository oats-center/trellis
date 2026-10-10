# Design: Auth Protocol

Status: retired WO-02 protocol reference during the unreleased Revision 7
cutover. Do not implement these former bootstrap/bind routes. Current core
semantics are in [trellis-auth.md](trellis-auth.md); native contracts and
protocol source define the replacement formats. OAuth/Callout transport wiring
follows in the remaining implementation phases.

## Cryptographic Boundary

Trellis uses Ed25519 keys, SHA-256 digests, base64url without padding, canonical
JSON where specified, and length-prefixed domain-separated transcripts.
Proof-bearing JSON requests hash the complete raw request body before known
fields are projected, so unknown members remain integrity-bound even when an
open DTO ignores them.

The proof formats are independent of the JavaScript cryptography implementation.
Browser clients use WebCrypto when the operations Trellis needs are usable there
and an equivalent portable implementation otherwise; both produce identical
signed bytes. HTTPS is the normal deployment transport. An operator may
explicitly authorize plaintext HTTP for a non-loopback public origin, which
changes transport protection only: every proof format, digest, and key
derivation stays the same.

`trellis.session-proof.v1` supports these purposes:

- `browser-auth-request`;
- `browser-auth-progress`;
- `browser-bind`;
- `native-bootstrap`;
- `device-enroll`; and
- `context-refresh`.

Every proof binds purpose, canonical origin, request ID, issued-at milliseconds,
raw payload digest, and session public key. Browser request proofs bind the
initiating request. Browser bind proofs also bind the exact
`/auth/transactions/{transactionId}/bind` route and the initiating public key;
their `requestId` is a separate canonical ULID, not the transaction ID.

## Native Bootstrap

`POST /bootstrap/service` accepts exactly:

- `identityKeyId`;
- optional `name`;
- `sessionKey`;
- `connectionId`;
- `requestId`;
- `iat` in milliseconds; and
- `proof`.

`POST /bootstrap/device` has the same shape. The server rejects unknown fields
and never accepts deployment IDs, instance IDs, semantic evidence, resource
bindings, grant sets, or transport assertions from the caller.

Successful service and device bootstrap return the same installation shape:

```text
assignment
runtime
transports
```

`assignment` identifies the server-owned deployment and instance. `runtime`
contains the exact installed participant revision, resolved APIs, effective
grant, resource evidence, signed context, route JWT, expiry timestamps, inbox,
and session-key binding. `transports` contains exact Core NATS and WebSocket
endpoints. All wire expiries are epoch milliseconds.

Clients strictly decode this response and verify participant, revision,
artifact/API digests, logical uses, grants, resource evidence, session key,
inbox, context digest/signature, route JWT, and transport endpoints before
connecting.

## Device Enrollment

`POST /auth/device/enroll` is the only operation that creates or resumes a
device activation review. Its proof purpose is `device-enroll`. An optional
one-use provisioning secret may establish the durable device identity and is
consumed atomically. Polling reuses one identity credential, ephemeral session
key, and logical connection ID for the activation attempt.

The operation reports only `pending`, `rejected`, or `ready`. `ready` means the
review is approved; installation still occurs through `/bootstrap/device`.
Offline QR material is transported by the application and is not a server route.

## Browser Intent And Authentication Transaction

`POST /auth/requests` accepts:

```text
participantId, participantKind, requestId, origin, redirectTo,
sessionPublicKey, issuedAt, proof
```

The server validates the initiating proof and current participant/portal
eligibility, then returns `intentId`, an opaque signed `intent`, and `loginUrl`.
The client does not upload an artifact or authority digest. Request creation
does not persist an authentication transaction or start its deadline.

The `trellis.browser-sign-in-intent.v1` envelope binds the participant ID,
initiating public key, initiating origin, exact return target, selected portal,
issue time, and online issuer key ID. It carries no authenticated principal,
approved grant, consent result, or provider secret. A portal URL transports
`?intent=<opaque-envelope>`; it is observable, not a bearer login credential.
The runtime serves pages with `Referrer-Policy: no-referrer`.

Rendering calls `POST /auth/intents/view` to verify the intent and obtain
current non-secret login choices without creating an attempt. Signature failure,
unavailable signing issuer, participant removal, or changed portal/redirect
eligibility invalidates the request explicitly; idle elapsed time is not an
authentication-attempt deadline. This public rendering boundary does not reveal
transaction progress or IDs.

Detached initiating clients call `POST /auth/intents/progress` with `intent`,
`sessionPublicKey`, a fresh `requestId`, `issuedAt`, and `proof`. The
`browser-auth-progress` proof binds the complete payload and canonical Trellis
origin and must verify under the initiating public key in the intent. The
response is `waiting_for_user` when no outstanding attempt exists, `pending`
while authentication or consent is incomplete, `denied` after denial, or `ready`
with `transactionId` when the attempt can bind or replay completion. Only
`ready` reveals the transaction ID. The portal already owns its ID from
transaction creation; its transaction progress and consent reads require the raw
portal binding, not possession of a public ID or intent.

When the user starts local authentication or chooses an OIDC provider, the
portal persists a fresh pending binding indexed by intent before calling
`POST /auth/transactions` with the intent and its `portalBindingDigest`,
carrying the raw binding in `Trellis-Portal-Binding`. Trellis verifies the
intent and current eligibility, selects the current participant revision, and
assigns an independent `transactionId`. The attempt's deadline begins here, not
when the intent was created or the page rendered.

Creation atomically claims the intent-to-transaction index at the repository
boundary. At most one outstanding attempt exists per intent. A retry with the
same binding returns the existing unexpired transaction without resetting its
state or deadline; a different binding receives `intent_transaction_active`,
including while approval awaits bind. Denial, expiry, or consumed completion
permits a fresh independent transaction. NATS index changes use revision CAS; an
unclaimed candidate is never usable as a transaction. After a failed or
uncertain index acknowledgment, an exact read from the stream leader confirms
success only if the index selects the candidate. Unconfirmed writes retain their
candidate until the normal KV deadline because the write may still commit;
same-binding retries can recover the selected attempt.

The portal retains the raw 32-byte binding in portal-origin `sessionStorage`; it
reuses the pending binding on retry or reload and associates it with the
transaction only after receiving the ID. Removing the pending association
follows successful transaction persistence and URL continuation, not the first
request. Only its SHA-256 digest is persisted server-side. Bound actions require
the binding as designed. The transaction owns authenticated identity, provider
attributes, current consent, expected grant revision, approval/denial, and
completion. OIDC PKCE, nonce, state, and callback association refer to this
transaction. Callback continuity still requires the Trellis-origin HttpOnly
SameSite=Lax cookie and exact CAS-backed state record. Account-flow
continuations use `browserTransactionId` without changing account or device
review lifetimes.

Browser OIDC starts with `POST /auth/login/{providerId}` carrying JSON
`transactionId` and `portalBindingDigest` plus the raw portal-binding header.
Both initial and replayed browser callbacks return to the selected portal with
`transactionId`. Account-flow OIDC keeps its separate GET start route and
account-flow continuation.

Expired transactions cannot authenticate or complete. The portal retains the
signed intent, explains the expired attempt, and starts another transaction when
the user continues. Independent attempts do not inherit identity or consent;
denial or expiry does not destroy the signed intent.

Existing grants are reused only when shared issuance/policy semantics prove
compatibility without expanding what the user approved. Revision change alone
does not require consent. Additional authority eligible for user approval
presents current consent after authentication without a second password prompt.
Policy denial remains an authorization error; incomplete runtime/resource
evidence remains bounded pending. Approval recomputes current participant,
grant, capability, and resource consent: stale decisions are rejected and the
portal refreshes the view before another decision.

`POST /auth/transactions/{transactionId}/bind` requires proof of the initiating
private key. The first valid completion creates a login-only session; exact
supported replay returns that transaction's recorded semantic login result, not
another session. The response includes `intentId` and the minimal login
projection, not authorization. The client validates intent, participant, and
session public-key bindings, commits generation-fenced login state, then invokes
context refresh.

Only terminal refresh failure with an invalid login may clear login and become
authentication-required. Terminal authorization denial with a valid login
propagates its stable machine code without automatically starting another login.

## Context Refresh

`POST /auth/context/refresh` accepts the old context digest, participant
identity, request ID, issued-at milliseconds, and session proof. It revalidates
current principal, credential/login when present, exact `GrantBinding`,
installed revision, resource evidence, and issuer state. It returns the shared
`runtime` plus `transports` installation shape, including a new short-lived
route JWT.

Clients refresh proactively before expiry, and during actual recovery when
needed. Fresh native bootstrap uses the context returned by bootstrap.

A renewable authorization context and routing credential are not a lease on a
physical NATS connection. Routine equal-policy renewal validates and retains the
new context, then promotes application authorization in place without socket
churn. Next-connect credentials and endpoints stay current independently of the
immutable policy admitted on each physical generation. Healthy safe policy
growth automatically prepares, readies, and publishes a successor; it requires
neither an application transport-refresh API nor an upgrade notification. A
failed refresh leaves a still-valid predecessor untouched and retries with
bounded backoff.

Own refresh retains the original verified preparation, exact attempt identity,
corrected clock, and CONNECT companions throughout coverage warming and
promotion. Retaining a candidate does not replace installed own authority;
rejection or cancellation releases only that attempt's resources. Warming
borrows a safe admitted carrier when one exists. With no safe survivor, recovery
may open a private exact-candidate CONNECT stage solely to warm revocation
coverage. It admits no application intake and is not published as the default
before successful authority promotion and normal readiness. Ordinary adoption
then uses that same warmed socket. Cancellation or logical close releases the
private stage, and a late completion cannot resurrect it.

## Signed Authorization Context

`trellis.authorization-context.v1` includes:

- context digest and validity window;
- issuer key identity and signature;
- principal, identity, participant ID, and installed revision;
- logical connection ID and optional login session ID;
- session public key;
- exact effective `GrantSetV1`;
- exact resource evidence;
- server-issued inbox prefix; and
- a signature-bound transport authorization policy (`transportAuthorization`).

The server persists exact signed bytes plus the complete issuance snapshot in
Auth SQL before use. The digest-keyed NATS KV record is an exact runtime mirror.
Explicit revocations use separate immutable keys.

## Application Request Verification

Application requests carry `authorization-context`, `session-key`, and `proof`.
The proof binds the exact subject, payload digest, reply inbox, issued-at, and
request ID. Verification:

1. resolves the signed context by digest;
2. verifies issuer signature and context structure;
3. rejects explicit context or issuer revocation;
4. requires `session-key` to equal the signed context key;
5. verifies the message proof with that key;
6. validates the exact subject/action against the context grant; and
7. checks reply subjects against the server-issued inbox prefix.

The redundant session header is never an independent identity assertion.

## Event Verification

Events carry `authorization-context`, `session-key`, `proof`, event ID, event
time, subject, payload, and `Trellis-Event-Descriptor`. The descriptor names the
qualified API ID, exact dotted event name, and parameter count. The proof binds
the descriptor and every other event-specific field, so a parameterized event
cannot be mistaken for a longer event name that produces the same NATS subject.
Concrete parameter tokens are canonical UTF-8 encoded as unpadded base64url.
Events produced inside a context validity window remain historically verifiable
after ordinary context expiry, but explicit context or issuer revocation
invalidates them.

The Events journal stores raw headers and payload plus context digest and useful
principal/connection/login projections. It never stores a duplicate complete
context or grant set. Historical validation resolves immutable Auth SQL/KV
context bytes by digest.

## NATS Auth Callout

A Trellis NATS connection presents the session NKey signature and a Trellis
connect token; it presents no client JWT. The connect token is wire format
`trellis.nats-connect-token.v2` and carries the selected context digest plus the
short-lived `routingJwt` that authorizes this admission. In operator mode the
broker routes a JWT-less CONNECT to the external Auth Callout through a
server-generated, non-expiring, server-only Auth-account `default_sentinel` that
denies every publish and subscribe; the sentinel confers no Trellis application
authority. A CONNECT that supplies any other client JWT is rejected, with no
dual-protocol fallback. Auth Callout:

1. parses the v2 connect token and requires the session NKey and the
   server-routed `default_sentinel`;
2. validates the token's `routingJwt` — issuer and issuer account, subject equal
   to the session NKey, a present short expiry after the current time, and exact
   deny-all routing permissions — against that session NKey;
3. verifies the standard NATS nonce signature with the session NKey;
4. resolves and verifies the signed context by the token's digest and requires
   the session NKey to match the context;
5. rechecks current credential/principal/grant/participant/resource/issuer
   evidence against the immutable issuance snapshot and confirms the signed
   transport policy is still covered by current authoritative state;
6. installs the exact publish/subscribe policy signed into that context and
   records physical connection presence; and
7. returns a user claim whose authenticated name identifies the admitted context
   and physical attachment.

Auth Callout verifies initial/reconnect credentials and installs the exact
transport policy signed into the selected admissible context. The returned NATS
user claim is not bounded by renewable context or route-token lifetimes. It is
bounded only by actual underlying authorization deadlines when they exist.
Existing sockets remain subject to authoritative revocation and
physical-attachment enforcement. Admission identity and policy are retained
until the broker confirms the attachment is gone; ordinary context renewal does
not change that admitted record. A delayed control-plane partition can postpone
an active kick; Trellis does not promise disconnection before the enforcement
path reaches the broker, and it never reports a failed kick as complete.

The issued NATS user JWT carries no periodic lifetime of its own. NATS ACLs
enforce transport permissions but do not define semantic authority.

### Transport Authorization

The signed authorization context includes a required `transportAuthorization`
value of wire format `trellis.transport-authorization.v1`:

- `account`: the target NATS account public ID the policy applies to;
- `publishAllow`: sorted, deduplicated NATS subject patterns the attachment may
  publish;
- `subscribeAllow`: sorted, deduplicated NATS subject patterns it may subscribe
  to;
- `response`: `null`, or a bounded response allowance with `maxMessages` and
  `ttlMs`, both positive safe integers (`ttlMs` in milliseconds); and
- `hardExpiresAt`: `null`, or the exclusive Unix-seconds deadline of the real
  underlying authorization, which is not the context or route-token renewal
  window.

The policy is signature-bound with the context. Its digest is
`base64url(SHA-256(canonical JSON of transportAuthorization))`, computed with
the shared canonical-JSON implementation; the digest excludes the context
digest, issuance time, grant revision, routing-JWT bytes, endpoint list, and
local transport generation. The policy compares admitted policy **A** against
currently allowed policy **D** for the same account: `A <= D` when every
published and subscribed subject A permits is also permitted by D, A's response
allowance is absent or covered by D's with at least the same count and duration,
and A's hard deadline is no later than D's (`null` is infinity). `A <= D` and
`D <= A` is `current`; only `A <= D` is `upgrade_available`; otherwise the
socket requires `reduction_required`. These remain classifier wire labels, not a
public passive upgrade interface: safe growth drives automatic successor
adoption. A changed target account always requires a new physical attachment.

Subject containment is decided against the NATS grammar the compiler emits
(literal tokens, `*` matching exactly one token, and terminal `>` matching one
or more tokens), never by literal string-set subtraction: `a.>` covers `a.b`,
while `a.*` does not cover `a.b.c`. One shared pure implementation is used by
both SDKs.

The admitted attachment's authenticated name is
`trellis.auth.v1:<contextDigest>:<serverId>:<decimalClientId>`; the claim
subject remains the broker-generated one-use user NKey. After each actual
successful connection the SDK performs one bounded own-user information request
on that attachment (`$SYS.REQ.USER.INFO`), validates the local generation and
reply identity, resolves the named signed context, and confirms the
broker-reported account and permissions match that signed policy. The hard
deadline comes from that named signed policy. Because a named context is
immutable historical evidence, its later ordinary expiration does not invalidate
the attachment, and it is not reused as current application authorization after
expiry. A hard security revocation of its provenance remains server-enforced.

### Generation Lifecycle And Coverage

One stable logical client owns physical generations with immutable admitted
context and policy. A successor is readied before default publication, and new
intake stops on its predecessor. Accepted work retains its receiving generation;
safe predecessors drain after intake is accounted for and transport leases reach
zero. Unsafe, revoked, or hard-expired generations are forcibly retired rather
than kept alive by leases. Broker admission identity is historical evidence,
never current application authority.

Fresh public I/O requires usable installed own authority and a final fence over
the current signed policy, corrected clock, and physical attachment. Temporary
revocation-coverage loss suspends bounded acquisitions; it does not itself
revoke the digest. A distinct guarded registry-maintenance path can restore own
coverage on the exact safe published attachment without forcing a new socket or
admitting application work while suspended. Cached verification is digest-keyed,
while usability requires continuous revocation coverage. Coverage bindings
migrate make-before-break to the exact published generation; watch loss is
binding-local and fail-closed, while confirmed revocation is digest-global. See
[Runtime Caches](rust-authorization-state.md#runtime-caches) for the handover
boundary.

## Live Observation Sessions

Live observations and Operation watchers are authorized live sessions rather
than repeated request/reply. An opening request is a bounded, verified request;
the provider answers with a signed offer and, per session, signs every data
frame, challenge, end frame, and control response. This provider-message proof
is a distinct proof domain from application request proofs: it binds the exact
subject actually published to (the negotiated data subject for provider frames,
the validated reply inbox for control responses) and the exact raw body.

Control requests are verified through the same request path as other live
traffic: the authenticated caller tuple, exact route action permission, session
ownership, and a strict descendant reply of the caller's inbox prefix are
checked before any state, credit, or terminal mutation. At most one outstanding
control attempt is retained per session; acknowledgements bind its request ID,
session ID, logical command sequence, command hash, and accepted cursors, and a
replay of a processed command returns its cached semantic outcome with fresh
proof rather than re-applying it.

Ongoing data and control authority is retained, not re-fetched per frame: a
session holds a real guard over the existing covered authorization lease and
rechecks pinned identity, context validity, revocation, and installation
generation locally. A guard's purpose differs by endpoint: the provider's own
publishing role, a local or admitted remote observer's exact Subscribe/Observe
atom, or a consumer's pinned peer identity; a guard for one route never
overwrites another route's requirement. Data publish and control subscribe
authority is connection-scoped transport permission, not proof of sender
identity, and a legitimately reachable subject does not make an unsigned or
foreign-signed frame trusted.

Authorization loss fences the local live session. A provider whose signing guard
has lost the caller's revocation coverage cannot sign an authorization-loss END
for that session; local closure does not promise that a signed remote END was
sent.

## Errors

HTTP failures use stable machine-readable codes with statuses appropriate to
malformed input, invalid proof, unauthenticated login-only operations, denied
authority, stale revisions, missing required evidence, and unavailable runtime
dependencies. RPC expected failures use generated result variants rather than
exceptions.

## Non-Goals

- offline trust chains or rollback-floor negotiation;
- client-uploaded artifacts, grants, or resource evidence;
- session IDs as universal native runtime identity;
- subject-derived permission inference; and
- old proof or authority compatibility parsers.
