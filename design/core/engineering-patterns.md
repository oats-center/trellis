---
title: Engineering Patterns
description: The coding patterns Trellis enforces in review, the class of bug each prevents, and how to check new code against them.
order: 15
---

# Design: Engineering Patterns

## Prerequisites

- [trellis-patterns.md](./trellis-patterns.md) - architecture and runtime
  boundaries
- [testing-patterns.md](./testing-patterns.md) - test boundaries and live
  integration
- [observability-patterns.md](./observability-patterns.md) - logging,
  correlation and health

## Patterns we enforce

Each pattern below names a rule, the class of bug it prevents with an example
that reached the repository, and what a reviewer looks for. New and changed
code in the runtime, the SDKs, the CLI and the tests is checked against all of
them. Code that has to break one says why in a comment next to it.

### Decide inside the transaction that acts on the decision

**Rule.** A check that decides whether to write, and the write it allows, run
in one transaction or under one owner. Never read a condition, await something
else, then act on what was read.

**Why.** Between the check and the act another writer can change the answer.
Enrollment checked that a deployment was active, created its Operation storage,
then checked again; a removal that committed between the checks deleted
nothing, and the late creation left buckets no deployment could use (commit
2265c88b7). `Issuers.Revoke` checked which key signed the reply apart from the
commit, so a revoke that raced a rotation committed with a reply the caller
rejected; it now decides in the transaction that commits it (c07a4ff97).

**Check.** Look for a read whose result guards a later write with an `.await`,
an RPC, a lock release or a second transaction in between. Move the condition
into the write's `WHERE` clause or transaction, or record the in-flight work so
the competing writer waits for it, as the enrollment fix does.

### A well-formed ID is definitive

**Rule.** An argument shaped like an ID is looked up by ID alone. A miss is
"not found", never a retry as a name, title, username or email. A lookup that
fails (timeout, unavailable, denied) is reported as that failure, not as a
miss and not as a reason to try another lookup.

**Why.** Falling back to name matching acts on whatever a search returns. CLI
edit, delete and role commands could act on another account whose username
matched an ID, and a timed-out ID lookup was retried as a name (022c8dfb3,
8864712ac, issue #144).

**Check.** Every lookup by ID handles three outcomes separately: found, not
found, and failed. A `match` that folds `Err` into `None`, or a
`.or_else(|| by_name(..))` after an ID lookup, is the bug.

### Tests assert on error codes and types, never on prose

**Rule.** A test checks a failure by its code, declared error type or variant,
and then by state read back afterwards. It does not match words in the
message, unless the message itself is the behaviour under test, such as a CLI
line an operator reads or a reference the caller is shown.

**Why.** Prose changes whenever someone improves a message, so a prose
assertion fails for no behavioural reason, and it passes when a different
failure happens to use the same words. The settings, webhook and durable event
tests matched message text; they now check `revision_conflict` and the
declared error type and read the stored values back (9a0d83746, issue #131).

**Check.** Look for `contains(`, `includes(`, `assert.match` or regexes over
`error.message`, `to_string()` or stderr. Replace them with the code or type,
or say in the test why the text is the contract.

### Retry transient failures, set aside permanent ones, stop only on unusable state

**Rule.** Background work classifies each failure as one of three kinds and
acts on it:

- transient (a lock, a busy database, a broker not answering, a full disk):
  the same work runs again after a bounded backoff, 250 ms doubling to 30 s
  unless the loop has a stated reason for other bounds;
- permanent (a constraint violation, a payload that cannot be decoded): the
  item is logged once at error with its ID and set aside, so the items queued
  after it go on;
- unusable (a corrupt database file): the subsystem that owns the state stops.

**Why.** A loop that stops on any failure turns a passing lock into an outage;
one that retries everything forever lets one bad item hold back every item
after it. Before 2f890a80b, a lock another process held stopped startup, and an
Auth effect that failed the same way every time blocked the effects queued
behind it.

**Check.** Rust loops use `trellis_rs::__internal::{retry, Backoff, Failure,
Halted}`; the runtime classifies storage errors through
`storage::ClassifiedFailure`. TypeScript loops use `Backoff` from
`ts/packages/trellis/backoff.ts`. A hand-written `delay = (delay * 2).min(..)`
or a loop that returns on every `Err` is the bug.

### A repeating failure is logged once, then rate-limited, and surfaced in health if it persists

**Rule.** The first occurrence of a failure is logged with its cause. Repeats
are logged at most once per interval, with how many were held back, and the
recovery is logged once. A failure that persists past a few attempts shows in
a health check or `Health.Storage`, so an operator sees it without reading
logs.

**Why.** A loop that retries several times a second and logs each attempt
floods the log and hides everything else; one that logs nothing leaves an
operator no way to find a stuck subsystem. Stuck Auth enforcement was logged
every minute and showed nowhere else; it is now logged when it starts, hourly
while it lasts and once when it recovers, and the `enforcement` health check is
degraded meanwhile (5d7588c1d). JetStream refusals are logged at most every ten
minutes and listed in `Health.Storage` (66df7e1f1).

**Check.** Rust uses `trellis_rs::__internal::LogLimit`. A cadence that must
survive a restart keeps its last-logged time in the same transaction that
records the failure, as stuck enforcement does with `stuck_logged_at`. Look for
a `warn!` or `error!` inside a retry loop with no limiter, and for a persistent
failure no health check reports.

### Revocation is eventually consistent, and a stale view never blocks good traffic

**Rule.** A participant's view of revocations only informs how it verifies
other parties. A mirror that is down, lagging or catching up does not fail or
hold up the participant's own requests, connects or activations. Auth and the
broker enforce revocation of a participant's own session.

**Why.** Waiting for a fresh revocation snapshot made every connect and request
depend on the mirror, so a mirror outage became an outage of good traffic. A
revoked session cannot renew, so its authority stops verifying everywhere
within one authority lifetime anyway (543628c9d).

**Check.** Look for an `await` on a revocation snapshot, mirror readiness or
catch-up on a send, connect or activation path. Recovery of the mirror belongs
in the background.

### A failure's reference is shown to the caller and logged once with it

**Rule.** A failure the caller sees carries a reference (an error ID). The
server logs exactly one line for it, under that same ID, with the request ID,
trace ID, route, caller and cause. Rust and TypeScript use the same field
names.

**Why.** Before c50b4db98 and 176167ecb, a Portal "Something went wrong" or a
Console error had nothing to look up, and some failures logged two lines and
some none, so a reference could not find its cause.

**Check.** A handler that fails returns the error with its reference and does
not log separately; the request loop logs it. Look for an extra `error!` next
to a returned failure, and for a `server_error` or `unavailable` answer with
no reference.

### Every external side effect is an idempotent durable effect

**Rule.** Work that changes something outside the transaction that decided it
(a broker resource, a published event, a revocation, a stream deletion) is
recorded as a durable effect in that transaction and carried out afterwards by
a worker. Running the effect twice is harmless, and a crash between any two
steps leaves the effect pending, so the next pass or the next start finishes
it.

**Why.** Doing the side effect inline loses it when the process stops between
the commit and the effect, and doing it before the commit leaves a side effect
for a write that never happened. A removed deployment's intake stream was
deleted before its webhook records were updated, so a stop in between left the
records pending and pointing into a deleted stream (3a6f98878). App sign-out
removed the refresh token before revoking its family, so a failed revocation
left the sessions live while a retry answered success (d9bf4b262).

**Check.** Find each call that reaches NATS, the file system or another service
from a write path. It either runs inside the transaction's own effect record
and worker, or its state is derived again from durable records on each pass.
The effect handler tolerates finding its work already done.

### The operator is trusted; defend against everyone else

**Rule.** Whoever operates an installation controls it fully: its host, its
NATS operator or system account, its configuration and secrets, or the
`platform.manage` role. Trellis does not carry defences whose only adversary
is that operator, and it does not add a second proof of something another
layer already proves. Defences against other users, services, apps, devices,
network attackers, webhook senders and untrusted input stay, and so do
correctness checks.

**Why.** A check against the operator proves nothing, because the operator
can change the code, the database, the broker or the file it guards, and each
one adds an error path, a test and a way for a good installation to fail. An
import names each stream it restores from the deployment's own resource and
allocation IDs, and that name is enough to know the stream belongs to the
deployment; a metadata tag or a database record proving the same thing again
only guards against someone who can already edit the broker or the
database. Replacing such a stream still asks the operator first, because
that is a decision, not a proof. A bundle digest that the importing administrator confirmed by hand
repeated what the passphrase-sealed envelope already proves: a bundle that
opens with the passphrase is whole and came from whoever holds it.

**Check.** For each new check, name the party it stops. If the only party is
the operator, or the property is already proved by another layer (an AEAD
envelope, an authenticated NATS account, a transaction), leave the check out.
A review finding whose only attacker is the operator is answered by saying
so, not by building a proof.

### Each test owns its resources for its whole life

**Rule.** A test holds its ports, target directories, data directories and
other state from start to finish, including across restarts of the runtime it
drives. It does not release a resource and reserve it again later, and it does
not share a build directory or database with another test.

**Why.** Released resources are taken by the next test that asks. A TypeScript
test runtime gave its ports back once the server was ready and reserved them
again on restart, so another test file's runtime could take one and the
restart failed with `EADDRINUSE` (9208a621d). The generated-consumer fixtures
shared one Cargo target directory, so one fixture's rebuild deleted the
`trellis-rs` rlib another was compiling against (2638c6dcd, issue #151).

**Check.** Look for a port picked, closed and bound later; a fixed port; a
target directory, temp directory or database path shared between tests; and a
restart that reserves its resources again. A flake of this kind is a bug in
the test, not load.
