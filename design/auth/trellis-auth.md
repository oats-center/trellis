# Design: Trellis Authentication And Authorization

Status: authoritative as-built model for the WO-02 authorization cutover.

## Principles

- Authentication establishes a principal. Authorization is always scoped to an
  installed participant revision.
- `GrantBinding` is the only current authority record. There is no desired or
  materialized authority, proposal, reconciliation, trust floor, or client-held
  authority state.
- Server-owned inputs are never accepted back as client assertions.
- Expected denials are modeled as typed results. Invalid signatures, malformed
  proofs, stale revisions, and unavailable required evidence fail closed.
- Auth owns durable identity, login, grant, issuer, resource, and signed-context
  history. NATS KV is a rebuildable runtime mirror, not the source of truth.

## Participants And Grants

An installed participant revision is an immutable snapshot of verified package
semantic evidence and exact participant lexical path. Revisions are
monotonically increasing per participant. The server selects the current
revision; clients do not upload generated artifacts or digests during bootstrap
or browser login.

`GrantBinding` is keyed by `(identityId, participantId)` and contains:

- the installed participant revision;
- approval mode, approved current-consent capabilities, approved resource
  commitments, delegation ceiling, and exact derived grants; and
- a finite set of Trellis platform privileges.

The only platform privilege is `PlatformPrivilege::Admin`. It authorizes
administrative RPCs but does not grant arbitrary participant atoms. Grant writes
use `expectedRevision`, validate every atom against the named installed
revision, and commit through the aggregate idempotency repository. The protected
bootstrap administrator cannot be demoted or revoked.

CLI, Console, activation Portal, and Auth runtime participants are installed at
startup from Trellis-owned source semantics. First-admin creation and reset
atomically write Admin bindings for CLI and Console in the same transaction as
the account credential. The Portal receives no Admin binding.

## Principals And Credentials

Users, services, and devices are durable principals. Services and devices use
provisioned identity keys; Trellis never stores their private key material.
One-use provisioning secrets are high entropy, stored only as hashes, returned
once, and consumed atomically.

`auth_sessions` contains user logins only. Native service/device connections do
not create login rows. Auth responses distinguish the logical connection from
the nullable login session. A physical NATS connection has a server-assigned
connection ID and operational presence record; it is not a durable login.

Password changes atomically update the Argon2 credential and revoke sibling
logins. Logout, password changes, and user-login revocation require a real
login. Native credentials cannot invoke login-only operations.

## Authorization Contexts

The online issuer signs `trellis.authorization-context.v1` snapshots. Issuance
rereads the current credential, principal, exact `GrantBinding`, installed
participant revision, required/optional resource evidence, and issuer state in
the signing transaction. Required missing evidence denies issuance; optional
missing evidence removes only affected optional grants.

Every issued signed context is stored durably and immutably in Auth SQL by
digest before it is usable. SQL retains complete historical context bytes and
the issuance snapshot needed to prove what was authorized. Ordinary expiry,
connection closure, grant replacement, login revocation, and issuer rotation
never delete historical records. Explicit context or issuer revocation remains
associated evidence and invalidates affected proofs.

At startup Auth republishes every retained SQL context into an empty or partial
`traversal_authorization_contexts` KV mirror, then publishes explicit
revocations. Publication requires create-or-exact-confirm behavior, read-back
byte equality, and stream configuration that cannot evict retained history.

Clients keep verified contexts, route JWTs, transport endpoints, clock offset,
and issuer keys in bounded process memory only. Provider-context caches have a
256-entry lease-aware LRU boundary; active entries cannot be evicted and retired
entries cancel their revocation watchers. There is no durable trust floor or
authorization-context store.

## Proofs And Transport

Bootstrap and browser bind use `trellis.session-proof.v1`. Proofs bind their
purpose, canonical origin, request ID, issue time, complete raw request digest,
and the ephemeral session public key. The same session key authenticates NATS.

Application requests and events carry:

- `authorization-context`: the signed context digest;
- `session-key`: redundant checked metadata that must equal the key inside the
  signed context;
- `proof`: the message-specific signature; and
- genuinely message-specific headers such as event ID, type, subject, and time.

Verifiers resolve authority by context digest, reject a mismatching
`session-key`, check explicit revocation, verify the proof, and authorize only
the exact action in the signed grant. Messages never carry complete contexts or
grant sets.

Auth Callout validates the short-lived route JWT, reconstructs and verifies the
connect proof, reloads the immutable context by digest, rechecks current
issuable state, and derives NATS permissions from accepted grants plus exact
participant/resource evidence. It never infers authority from subject strings or
trusts the redundant session key independently.

## Bootstrap

Native service/device bootstrap requires an existing provisioned principal,
proof of identity-key possession, and a valid session proof. The request
contains only identity and proof inputs. The server returns one shared
installation response containing:

- the server-owned assignment;
- exact installed package/participant semantic evidence;
- effective grants and structured resource evidence;
- the signed authorization context;
- a short-lived route JWT; and
- exact Core NATS and WebSocket transport options with millisecond expiries.

Service/device runtimes reject unknown response fields, verify every binding,
and create no durable login session. Device enrollment is the separate
`/auth/device/enroll` proof operation; `/bootstrap/device` only admits an
already approved device.

## Browser Authentication

`POST /auth/requests` creates a browser flow. The client supplies participant
ID, participant kind, canonical origin, request ID, redirect URL, raw-request
digest, session public key, and proof. Trellis resolves the installed revision
and current binding server-side.

Portal actions use a 32-byte browser binding. The portal retains the raw value
in portal-origin `sessionStorage`; Trellis persists only its SHA-256 digest and
requires the raw value in `Trellis-Portal-Binding`. Origin remains CSRF defense,
not authentication. OIDC additionally uses its HttpOnly SameSite=Lax callback
cookie and CAS-backed state record.

`POST /auth/requests/{flowId}/bind` verifies the bind proof and creates a user
login, but returns no authorization context. The client commits the validated
login result, then calls `/bootstrap/context/refresh` to obtain current
authorization. Browser persistence is IndexedDB `trellis-auth` version 3, store
`installations`, keyed only by canonical Trellis origin plus stable participant
ID. Generation, public-key, flow, and login compare-and-swap fences prevent a
stale tab from overwriting or clearing a newer login. Only the seed, minimal
login metadata, pending flow, and generation tombstone are durable.

## Events And Atomicity

Grant, participant, login, principal, issuer, resource, and device changes
commit state, idempotency result, and ordered post-commit actions in one SQLite
transaction. State changes revoke superseded contexts and request exact active
connection kicks after commit.

Auth events use the ordinary authorization path. A relay claim atomically
prepares immutable canonical body bytes, current preparation time, context
digest, session key, and proof for that attempt. Retries publish those exact
bytes and metadata. If the context is stale before preparation, the relay issues
and distributes a current context directly, without browser refresh or a second
user login.

## Non-Goals

- offline roots, certificates, manifests, rollback floors, or trust tooling;
- desired/materialized authority or reconciliation;
- client-authored installation assertions;
- universal durable sessions for services and devices; and
- compatibility readers for the unreleased retired authorization model.
