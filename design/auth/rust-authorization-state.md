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
lease-aware eviction and revocation-watch lifetime. No durable rollback floor,
manifest state, route token, grant cache, or context cache exists.
