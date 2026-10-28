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

Distinct-user tests use ordinary accounts, explicit capability grants, and real
native OAuth consent. The Node testkit exposes `createUser`,
`grantUserCapabilities`, and `registerClient`/`connectClient` with `user`; the
Rust testkit exposes `create_user`, `grant_user_capabilities`,
`register_native_application`, and `login_native_application_as`. User handles
belong to one runtime, expose no password, and never borrow seeded administrator
authority. One registered application can have independent logins for multiple
users. Explicit-user login never adds or restores grants.

Both live runtime helpers serve the default generated installation TLS, and
every client trusts that runtime's CA explicitly: the Rust kit through
`trust_roots` and `https_client_builder`, the Node kit by adding the CA to the
test process's Node trust, spawned Node processes through
`NODE_EXTRA_CA_CERTS`, the CLI through `--ca-certificate`, and Playwright
browsers through `browserTrustEnvironment()`, a private NSS trust store that
reproduces a user importing the CA (NSS `certutil` is a test prerequisite).
Never use `ignoreHTTPSErrors` or disabled verification to reach a TLS runtime.

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

Executable Rust and Node tests are the catalog. Cross-language behavior is
proved by the smallest live test that crosses the real transport boundary;
Rust-only runtime behavior remains in ordinary Rust integration tests.

### Case-Owned Runtime

Each live test owns real NATS and temporary SQLite state. TypeScript harnesses
and executable acceptance own a real `trellis-server` process; the Rust testkit
owns the linked production runtime task. Built-in Jobs and Events run in the
ordinary production runtime. Tests use generated participants from a small
native-IDL fixture; they do not construct substitute API descriptors. Prebuilt
server and CLI paths are passed through the explicit native path source by the
repository integration helper. The released TypeScript testkit defaults to the
latest published CLI/server release, verified against its published checksum;
production `trellis-server init`
owns bootstrap, with certificates from `trellis-server tls issue` supplied the
way a production installation supplies trusted ones (development-mode tests
use generated TLS), and `trellis-server` runs the broker it describes (`--nats-download`). The private
sandbox retains config, data, state, and identity across full-host restart; only
verified immutable binaries are shared. Seeded admin is the default, with an
explicit browser-flow option for first-admin tests. Real broker adapters use an
isolated observer connection rather than a second TypeScript-owned NATS process.
Automatic TCP listeners stay outside the kernel's outbound ephemeral range.
Rust and Node reservations also hold the same loopback UDP port across the
release-to-bind handoff, preventing another testkit from claiming a listener
while its production child starts; the child binds TCP normally.
The TypeScript host runs in a dedicated Unix process group: shutdown first asks
the host to stop gracefully, then force-closes remaining owned descendants after
the deadline or host exit. Process-failure tests may deliberately kill or
replace their isolated broker, but must keep a replacement available while its
original host releases broker-backed ownership and reap it before normal managed
restart. There is no shared-runner registry, tenant allocator, artifact
manifest, or host-wide scheduler.

Rust and TypeScript live tests use ordinary Rust and Node discovery. The
pipeline runs both discovered suites against real Trellis infrastructure. Focused
local runs select tests through the native Rust and Node test runners rather
than a second registry or scheduler.

The external `trellis-testkit` Rust harness owns production Trellis and NATS
processes for service repositories, chooses its loopback
ports automatically, and drives real bootstrap, login, consent, and provisioning
boundaries. The harness owns its administrator connection and server/broker
children; the test owns generated clients and service tasks. Cancellation/drop
reaps owned children before sandbox cleanup. Server-only restart retains the
same running broker and durable installation. Readiness matches the production
`/readyz` process ID to the owned child. Binaries can be explicit or acquired
from the latest (or `TRELLIS_TEST_VERSION`) release, with offline cache reuse.
Unexpected child failures remain shutdown errors even when the child has already
exited, preserving `OnFailure` diagnostics. Source CI stages the CLI and server
alongside its prebuilt testkit executable; it does not depend on unpublished
release assets. Service registration reviews implemented and consumed APIs before
creating the deployment, so consumer-first setup does not depend on provider order.
Only `trellis-rs`, `trellis-protocol`, `trellis-models`, and `trellis-testkit`
are registry-distributed; server, compiler, and bootstrap implementations remain
private and are not consumer Cargo dependencies.

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
- **Tier 3, integrated gate:** the `Pipeline` workflow owns formatting,
  lint/type checks, generated freshness, package tests, and the complete
  unfiltered live suites with no hidden skips. On a release branch its publish
  jobs follow the acceptance gate and publish that run's verified packages,
  archives and images without repeating the checks.
  Trellis supports current stable Rust only; no older compiler compatibility is
  promised.

Tier 1 and Tier 2 results are scoped evidence, not a full verification claim. Do
not rerun Tier 3 after every implementation track; run it once from the final
integrated source state before preparing a release.
