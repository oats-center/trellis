# Design: Rust Authorization State

Status: authoritative Rust persistence model after WO-02.

## Ownership

The Rust Auth runtime owns all authorization state and transactions. Native IDL
owns public RPC/event schemas; generated Rust and TypeScript SDKs consume them.
There is no TypeScript Auth service or handwritten wire-schema authority.

## Durable Records

Auth SQL stores:

- principals and immutable public identity keys;
- user-login sessions only;
- deployments, service instances, devices, and one-use provisioning secrets;
- immutable installed participant revisions with exact participant and resolved
  source-package semantic evidence;
- one current `GrantBinding` per identity and participant;
- resource declarations, assignments, and exact runtime evidence;
- online issuer lifecycle state;
- every issued signed authorization context and complete issuance snapshot;
- explicit context/issuer revocations;
- portal policy and browser/account-flow records;
- idempotency results; and
- ordered post-commit actions, including immutable prepared event deliveries.

Native service/device connections do not create session rows. Physical
connection presence is operational NATS KV state keyed by server-issued
connection ID.

## Installed Participants

An installed revision is immutable and contains the complete canonical
participant plus exact resolved APIs. Installation validates namespace ownership
and API resolution before allocating the next participant-local revision.
Built-in Auth, CLI, Console, and activation Portal artifacts are installed from
canonical repository-owned bytes at startup.

## GrantBinding

`GrantBinding` is keyed by `(identityId, participantId)` and references one
installed revision. It contains exact normalized grants and finite platform
privileges. Writes require the expected binding revision, validate all atoms
against the referenced installed snapshot, and protect the bootstrap
administrator from demotion/revocation.

There is no desired state, materialization, proposal, reconciliation, or
separate identity/deployment authority record.

## Issuance

Issuance runs in one SQL transaction and rereads current principal,
credential/login, binding, installed revision, resource evidence, and issuer. It
computes exact grants and expiry, signs the context, stores immutable context
bytes and the complete snapshot by digest, and commits publication work.

Required unavailable evidence denies issuance. Optional unavailable evidence
removes only affected optional grants. Admission later recomputes current state
and requires exact equality with the immutable issuance snapshot.

## Historical State

Context rows are permanent history. Ordinary expiry, connection closure, grant
replacement, login revocation, and issuer rotation never delete them. Explicit
revocation is separate associated state. Startup rebuilds the NATS KV context
mirror from all SQL rows before readiness.

The Events journal stores only context-digest references and useful projections.
It resolves immutable context history from Auth when validating old events.

## Transactions And Events

Every aggregate mutation authorizes the actor and checks idempotency in the same
transaction as state writes. Replay returns the committed result without
re-execution. Context revocations, typed events, and connection-kick intents are
ordered post-commit actions committed with the mutation.

Auth event delivery preparation is itself claim/attempt fenced. The first valid
claim stores canonical payload bytes, current event time, context digest,
session key, and proof; retries reuse the exact tuple.

## Runtime Caches

Auth's context KV and connection presence are rebuildable runtime indexes.
Client/provider authorization caches are bounded process-memory state with
lease-aware eviction. Verified live and historical authorization is keyed by
context digest; usability requires continuous revocation coverage, not continued
attachment to the socket that first resolved it. Each coverage binding owns its
exact watch and transport lease, not authority validity. No durable rollback
floor, manifest state, route token, grant cache, or context cache exists.

Bindings follow the exact retained published generation, including idle retained
entries. Migration prepares and warms a successor watch while continuing to poll
the old authoritative watch. The final compare-and-swap checks retained
publication, guarded current signed authority and corrected clock, physical
safety, cache lifecycle, and the exact entry/binding being replaced. The binding
swap is synchronous; only then is the predecessor watch and lease released.
Cached entries, borrowers, and verifier handles survive. A superseded
preparation aborts and reconciles against the latest publication without waiting
for the default API timeout. An absent publication or failed provisional setup
does not invalidate healthy old coverage. Watch loss fails closed only for the
binding that lost coverage; confirmed revocation from either watch is
digest-global, including when observed before successor initialization.

Fresh application acquisition requires usable installed own authority and a
final signed-policy, corrected-clock, and physical fence. Coverage suspension
blocks bounded application acquisition, while distinct guarded registry
maintenance can reestablish own coverage on the exact safe published attachment.
Maintenance still requires a valid unrevoked installed signed context and hard
deadline; it does not publish application usability merely by acquiring a lease.
A live session closes locally on authorization loss, but unusable caller
coverage cannot sign a remote authorization-loss END.

Own refresh carries the original verified preparation, exact attempt identity,
clock, and CONNECT companions through retention and promotion; digest equality
alone cannot substitute another preparation. Candidate coverage stays separate
from installed own state until the guarded synchronous promotion. Failure,
supersession, or cancellation releases only that attempt's resources, preserving
healthy installed authority and independently borrowed entries. Warming first
borrows one safe admitted carrier. No-safe-survivor recovery may instead open a
private exact-candidate CONNECT stage for coverage only, with no application
intake or default publication. Successful promotion lets ordinary adoption ready
and publish the same warmed socket. Cancellation or logical close releases it;
late completion cannot resurrect the stage. Equal-policy renewal needs no new
socket; safe growth automatically adopts a ready successor. Accepted work keeps
its receiving generation, safe predecessors drain after lease zero, and unsafe,
revoked, or hard-expired generations are forcibly retired.
