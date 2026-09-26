---
title: Testing Patterns
description: Trellis testing policy for smallest real boundaries and live integration coverage.
order: 90
---

# Design: Testing Patterns

## Prerequisites

- [trellis-patterns.md](./trellis-patterns.md) - Trellis architecture and
  runtime boundaries
- [../contracts/trellis-api-participants.md](./../contracts/trellis-api-participants.md) -
  contract-owned surfaces and permission derivation

## Design

This is the Trellis repository's correctness policy, not a required test
framework or organization for downstream services. Consumer repositories choose
their own test tooling. The live runtime helper is available when their tests
need to exercise the actual Trellis boundary.

Trellis behavior is distributed runtime behavior. Tests that prove Trellis
runtime behavior must use a live Trellis control plane and real TypeScript and
Rust client/service paths.

### Smallest Real Boundary

Test an invariant at the smallest real boundary that proves it. Use complete
live Trellis integration when transport, authorization, cross-language behavior,
process lifecycle, restart, NATS, or distributed coordination is part of the
invariant. Use real component or adapter integration tests for deterministic
transaction, repository, projection, reducer, and state-machine invariants that
do not require the complete runtime. Such tests use the production
implementation and a real backing adapter such as temporary or in-memory SQLite,
not a fake implementation of the boundary under test.

Do not prove Trellis behavior with fake NATS, fake Hono, fake storage, fake
runtime, fake auth, fake generated clients, or fake control-plane responders.
Those tests create a second implementation of Trellis and drift from the
runtime.

When behavior is not reachable through a public generated API, test the
authoritative production component directly at its real storage or transport
adapter boundary. Do not expose runtime internals, add generic chaos frameworks,
or add synthetic fault hooks solely for tests.

### Test Ownership

Executable Rust and Deno tests are the catalog. Cross-language behavior is
proved by the smallest live test that crosses the real transport boundary;
Rust-only runtime behavior remains in ordinary Rust integration tests.

### Case-Owned Runtime

Each live test starts and stops its own real NATS and `trellis-server` processes
and owns its temporary SQLite state. Built-in Jobs and Events run inside that
server. Tests use generated participants from a small native-IDL fixture; they
do not construct substitute API descriptors. Prebuilt server and CLI paths may
be supplied through environment variables. There is no shared-runner registry,
tenant allocator, artifact manifest, or host-wide scheduler.

Rust and TypeScript live tests use ordinary Rust and Deno discovery. The normal
Check runs both discovered suites against real Trellis infrastructure. Focused
local runs select tests through the native Rust and Deno test runners rather
than a second registry or scheduler.

The external `trellis-testkit` Rust harness follows the same process-based
ownership for service repositories. It starts the released `trellis` and
`trellis-server` executables out of process, chooses its loopback ports
automatically, and drives real bootstrap, login, consent, and provisioning
boundaries. The harness owns the infrastructure processes; the test owns the
generated clients and service tasks it connects. Internal runtime crates stay
unpublished: a service repository consumes only `trellis-rs` and `trellis-testkit`.

### Live Observation Timing Proof

Live observation sessions have bounded deadlines (reservation, control,
heartbeat, peer inactivity, consumption stall, cleanup, close exchange) that
otherwise require real elapsed time to exercise. Prove them at three distinct
boundaries, and record which boundary each test establishes:

- **Deterministic clock:** exercise the actual production deadline owner under a
  controllable local clock. In Rust that is `tokio::time::Instant` with paused
  test time; in TypeScript it is an injected monotonic clock. Advance time in
  ordered due events and await an explicit completion signal for each
  transition; advancing time alone does not prove the handler ran. This is the
  production reducer, not a copied algorithm or a test-only fake runtime.
- **Real broker:** prove NATS response-permission expiry and count with a real
  isolated broker using a deliberately short response allowance, and prove that
  compiler-derived static live subjects are independent of it.
- **Real interoperability:** exercise ordinary builds, real infrastructure, and
  bounded meaningful cross-language traffic with real lifecycle events and
  validated frame order. Virtual elapsed-time evidence is not a real multi-hour
  soak, and a kept-alive quiet session is not a throughput benchmark.

Do not add a user-facing fast mode, timeout scale, product test endpoint, or
second wire version to shorten a test, and do not treat a CI job's upper timeout
as a required minimum duration.

### Smaller Test Boundary

Real component and adapter integration tests may cover transaction, repository,
projection, reducer, and state-machine invariants without starting the complete
runtime. Pure unit tests remain appropriate for:

- security, data-integrity, and observable regression checks
- exact byte-level codecs, canonicalization, proofs, and signatures where
  independent implementations must agree
- compiling generated language consumers, including a watch recovery consumer
  that uses the newly generated field
- publication inputs and CLI behavior that users actually rely on

Do not freeze implementation details with generated-source substring assertions,
test-name inventories, empty-array/private-shape checks, or large parser
matrices without a demonstrated regression. Native IDL needs a small successful
compile case and a useful malformed-source diagnostic, not one test per grammar
branch. Semantic protocol logic shared through Rust/WASM does not need a second
cross-language corpus. Keep vectors only for independently implemented wire or
cryptographic boundaries.

Checked-in demos are consumer acceptance: run their normal dependency update,
generation, type-check, and Cargo commands. Do not copy them into test fixtures,
add demo runners, or maintain a parallel scenario catalog.

If a smaller test needs fake Trellis runtime pieces to pass, move it to live
integration or replace it with a real component boundary. Delete it after live
TS/Rust coverage proves the same behavior if no independent smaller invariant
remains.

Retained smaller tests should make the proving boundary clear. Comments, when
needed, should name the deterministic invariant, not say that behavior is merely
"private" or "not public". Current Trellis behavior is the behavior to protect;
the question is which smallest real boundary proves it honestly.

### Verification Practice

Verification has three explicit tiers:

- **Tier 1, inner loop:** format changed files and run the smallest affected
  package tests, type checks, and live fixture or case that prove the change.
- **Tier 2, phase gate:** run preparation when generated artifacts may change,
  workspace formatting, lint and documentation checks, affected package suites,
  and the matching TypeScript and Rust live tests.
- **Tier 3, integrated gate:** the normal `Check` workflow owns formatting,
  lint/type checks, generated freshness, package tests, and the complete
  unfiltered live suites with no hidden skips. The release workflow does not
  repeat those checks; it verifies release metadata and the exact packages,
  archives, images, and publication inputs assembled from a green `rs` base.
  Trellis supports current stable Rust only; no older compiler compatibility is
  promised.

Tier 1 and Tier 2 results are scoped evidence, not a full verification claim. Do
not rerun Tier 3 after every implementation track; run it once from the final
integrated source state before preparing a release.
