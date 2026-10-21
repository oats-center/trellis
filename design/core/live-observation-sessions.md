---
title: Live Observation Sessions
description: Signed, connection-owned transport for Live and Operation live observation.
---

# Live Observation Sessions

A **live observation session** is the ephemeral, caller-specific live transport
behind a Live `Watch` or an Operation observation. It replaces the older
indefinite request/reply reply-inbox stream. Ordinary RPCs and finite Operation
controls remain request/reply.

This document is the architecture contract for that transport. The exact wire
schemas, constants, and error codes live in the shared protocol crate
(`trellis-protocol`) and are consumed by generated TypeScript and Rust clients.

## Model

- A **Live** is an ephemeral caller-specific live view. Closing an observation
  ends only that view.
- An **Operation** is durable work. Its observer is ephemeral and does not own
  the executor; closing an observation never cancels, restarts, or changes the
  ownership of durable execution.
- One authenticated connection owns one live session manager. There is no
  central live-session database and no relay.
- Live transport is **bounded but not durable**: it flow-controls and
  acknowledges lifecycle transitions over Core NATS, does not retransmit
  application data, and does not promise exactly-once business processing.

## Wire flow

1. The consumer sends one bounded opening request to the exact compiled Live base with a
   fresh request-proof reply inbox. An Operation watch opening is carried on the
   existing Operation control route.
2. The provider authenticates the exact Live `Subscribe` (or Operation
   `Observe`) authority, reserves a session, and returns a **signed offer** over
   one finite reply. No producer starts here.
3. The first consumer iteration installs a distinct **exact** data subscription
   and sends `activate`. The provider enters `ACTIVATING`, returns an activating
   acknowledgement, and publishes one signed delivery-path challenge.
4. The consumer answers the challenge with a signed `pulse`. A fresh, verified
   pulse acknowledgement **commits `ACTIVE`**, and only then does the provider
   invoke the source factory exactly once.
5. Application data flows on the signed, connection-scoped data subject.
   Owner-directed controls carry liveness, cumulative credit, and closure.

A prepared handle that is never iterated expires with its reservation and never
starts a producer.

## Subjects

Standalone Live uses protocol v1 independently of the application's API major.
The opaque API token encodes the complete qualified API ID, so
`documents@v1` and `documents@v2` have different routes without introducing a
second Live protocol. Deployment-owned Operation controls keep `operation.v1`;
canonical Operation starts keep the application's `operations.vN` major.

```text
Standalone base:  live.v1.route.<b64(apiId)>.<action>
Bound Live base:  live.v1.route.<b64(apiId)>.<b64(providerDeploymentId)>.<action>
Operation base:   operation.v1.<b64(apiId)>.<b64(providerDeploymentId)>.<action>
Owner controls:   <base>.observe.<b64(P)>.<sessionId>
Live delivery:    live.v1.data.<b64(P)>.<b64(C)>.<sessionId>
```

Each consumer subscribes to its **exact** data subject, never the whole wildcard
it is permitted to receive. Owner controls are handled only by the replica that
accepted the session; opening routes keep their queue groups, but owner-control
subscriptions do not.

Action names may contain dots. A component that happens to decode as base64url
does not establish a deployment binding: the exact compiled, authenticated
catalog route determines whether an opening is canonical or deployment-bound.
Neither parsing nor provider selection falls back to another route or protocol.

## Identity and authentication

Provider-origin messages (offer, control response, challenge, data, end) are
signed over the **actual NATS subject and raw received bytes** with the
`trellis.live-server-proof.v1` domain. Owner-control replies are signed over the
reply inbox, never the request subject. The offer's identity fields use the
canonical subject-token projection of the full runtime key; proof headers and
retained identities use the full runtime key. A frame is accepted only when the
actual subject, current covered context, pinned provider tuple, session id, and
signature all verify.

Both endpoints retain a real covered authority lease, not a digest snapshot:
self and peer for a consumer, self and admitted caller for a provider.
Canonical Live openings use the shared request queue for eligible providers;
there is no persisted caller-to-provider selection. The accepted provider must
have independently verified authority for the exact API/action and generation.
An explicit deployment-bound opening or retained Operation reference also fixes
the expected deployment. The offer alone never establishes provider eligibility:
its complete provider tuple must match its verified context, and the offered
consumer tuple must match the opener's current local identity. The caller must
retain the exact authenticated Subscribe or Observe authority throughout the
observation, with current ordered revocation coverage on both sides.

Ordinary identity-preserving authorization refresh retains and replaces covered
authority evidence without replacing the physical transport, recreating the
observation, resetting sequence/credit state, or restarting the source. Quiet
sessions propagate fresh authority using existing signed controls and
challenges. A real physical disconnect is a real delivery interruption and
follows ordinary observation-loss rules. Live does not add application-data replay, coordinated
pause/resume, or transparent subscription migration to hide that interruption. A
changed runtime key, logical connection id, selected deployment, or removed
route ends the session. Invalid or foreign frames are discarded without
extending liveness and without closing an otherwise valid session.

An idle route also adopts promoted own authority before signing a new offer.
It cannot rely on an active session's timer to refresh the retained guard;
otherwise the first observer after the old context expires receives stale
authorization evidence despite a healthy renewed connection.

## Credit and buffering

Live and Transfer share internal sequence/window accounting and
cumulative-credit scheduling. This is not a public API or a shared wire
protocol. Live retains its typed/filtered queues, encoded-body costs, existing
limits, challenge/heartbeat and closure semantics; Transfer owns raw-byte queues
and storage completion. Operation Watch remains Live, while Operation upload
uses Transfer v1.

- The provider window is 64 outstanding frames and 1 MiB of exact encoded `DATA`
  bodies. Cost is the whole serialized frame body, not a surrogate.
- The consumer releases credit when a validated item is **handed to the
  application**, not when bytes arrive. It sends cumulative credit in a bounded
  control after 16 newly released frames or 50 ms, whichever comes first. At
  most one logical credit control is outstanding; retries reuse its identical
  body and sequence with a fresh proof, and newer consumption is coalesced into
  the next one. A pending challenge is answered with a pulse that also carries
  the current cursors.
- A deliberately filtered frame still occupies its ordered slot, so the consumed
  prefix cannot advance past an earlier unread value; the prefix then advances
  across consecutive filtered markers.
- Wire cursors are checked u64 values serialized as canonical decimal strings.
- A verified sequence gap, or a challenge/end that implies an unreceived
  trailing sequence, closes the session as `delivery_gap`. Duplicates neither
  deliver twice nor inflate credit.

## Liveness and closure

- After activation the provider schedules one challenge every 10 s; an
  unanswered challenge is re-sent unchanged every 2 s. Only a fresh challenge
  answer extends the peer-inactivity deadline; ordinary data and duplicate
  controls do not.
- A quiet, owned, authorized session stays active indefinitely. There is no
  total session age limit and no lifetime message quota.
- A stalled application backlog (outstanding data with no consumption progress)
  closes as `consumer_slow`. A quiet empty stream has no consumption deadline.
- Normal source completion publishes `end` after every admitted frame; the
  consumer acknowledges and drains already-queued items in bounded `DRAINING`
  before resolving `complete`.
- Handing out the final queued value commits `complete` and resolves `closed`
  immediately; the application need not poll once more. Abort, disposal, own
  authority loss, or a decode/protocol failure before the remaining queue is
  handed out discards it and commits the corresponding non-complete outcome.
- Explicit close and end-ack share one bounded best-effort exchange with fresh
  proofs per retry and a single total budget. `session_not_found`, transport
  loss, or no response leaves remote cleanup **unconfirmed**. A repeated logical
  close receives the current closed receipt, signed for its fresh request id.
- Closing resources that have not actually settled keep occupying admission.
  When owned cleanup exceeds the shared grace it counts once in
  `trellis.live.cleanup.pending`, the session stays `closing`, and both are
  released only at actual later settlement.

## Observation versus execution

- Closing an Operation observation never calls cancellation, writes
  `cancelRequestedAt`, or changes the executor lease.
- Reopening an Operation observation reconciles the current durable snapshot;
  transient updates missed while disconnected are not replayed.
- Live and Operation observers never receive an automatic producer restart or a
  silent data-loss fallback.

## Observability

Seven families are emitted by the endpoint record that owns the local state, in
both SDKs:

- `trellis.live.sessions` moves one unit between `prepared`, `activating`,
  `active`, `draining`, and `closing`, and is removed only at actual local
  cleanup completion. There is no `closed`/tombstone series.
- `trellis.live.ends` records exactly once at the local terminal commit,
  including admitted prepared sessions that expire or are cancelled.
- `trellis.live.handshake.duration` measures first activation through committed
  `ACTIVE` or activation failure, once, without including unused prepared time.
- `trellis.live.buffered.bytes` tracks actually retained serialized payload,
  transferring the charge from raw admission to decoded/staged ownership.
- `trellis.live.frames` counts admitted outgoing and verified incoming messages
  once per wire attempt with a `data`/`control` class and direction.
- `trellis.live.rejections` uses the fixed categories `invalid_signature`,
  `foreign_identity`, `invalid_protocol`, and `over_capacity`.
- `trellis.live.cleanup.pending` counts owned cleanup that exceeded the grace
  until it truly settles.

Each session reports its `trellis.kind` as `live` or `operation_watch`. Labels
are bounded; session and Operation ids, subjects, principals and arbitrary error
text never become dimensions. No span is held open for the lifetime of a
session, and no per-frame spans are emitted.

## Testing

- Live monotonic deadlines are tested through the production deadline owner
  under a controllable local clock. This is virtual elapsed time, not a real
  multi-hour soak.
- NATS response-permission expiry and independent static live delivery are
  tested against an isolated broker with a deliberately short response
  allowance.
- Cross-language paths are exercised with bounded real signed traffic and real
  lifecycle events on normal builds.
- An application-owned idle handle remains active by design; garbage collection
  is never the cleanup mechanism.
