# Publishable Rust `trellis-test`: implementation and review contract

**Repository:** `oats-center/trellis`  
**Integration branch:** `main`  
**Reviewed baseline:** `27f79d192204de9de78645b1006ceeccb59bf932`  
**Baseline commit date:** September 23, 2026, 23:03:45 UTC  
**Plan prepared:** September 23, 2026  
**Plan refreshed against current `main`:** September 24, 2026  
**Status:** implementation specification; no implementation, build, publication, or merge has been performed as part of preparing this document.

> This file is a verbatim copy of the coordinator-supplied implementation contract
> (`trellis-test-public-implementation-plan-current-main.md`) placed in the feature
> worktree per section 3 so reviewers can associate requirements and evidence with
> the implementation. Requirements are owned by the coordinator; this copy is not
> itself an authority.

## 0. Authority, scope, and the image reference

This document is the design and acceptance contract for the implementation agent. The requester has selected the architecture described here. The implementation agent implements and verifies it; the coordinating reviewer owns architectural decisions and approval.

Do not redesign the solution, substitute a different package boundary, add a different test runner, publish private implementation crates, or reduce the acceptance criteria. Ordinary coding decisions such as variable names and local control flow are implementation work. Changing a public signature, ownership rule, supported workflow, dependency boundary, security behavior, or acceptance criterion is a design change and requires an explicit coordinator decision.

Work in a dedicated Git worktree. Submit the work for review. **Do not merge into `main` until the coordinating reviewer explicitly approves the exact final commit and base.** Do not publish crates, create release tags, enable automatic merging, or trigger a publishing workflow under this implementation authorization. Publishing is a separate maintainer-controlled action after merge.

### 0.1 Reference image / target goal

The requester says they will supply a UI image with the agent handoff. **No UI image was available in this task when this plan was written.** Reference that supplied attachment in the handoff as "user-supplied UI target reference," but do not claim it has been reviewed or invent its contents, filename, or screen requirements.

This task is a Rust library, server-configuration, and distribution task, not a UI implementation. The attachment does not authorize frontend work or supersede the package/API acceptance criteria below. There is no screenshot-matching acceptance test for this task. Any separately requested UI work needs its own coordinator-approved scope. The missing image does not block this implementation.

### 0.2 Evidence conventions

`[S1]`–`[S26]` identify source anchors listed in section 23. Repository links are pinned to the reviewed commit. New filenames and APIs explicitly marked **new** are prescribed deliverables, not claims about existing code.

Source is authoritative for current implementation details. A name or signature moving after the baseline is mechanical drift only if its semantics still satisfy this document. Record the mapping in the evidence ledger. A contradictory behavior is a design blocker, not permission to select a different architecture.

### 0.3 September 23 `main` refresh incorporated into this plan

This revision incorporates the three commits that landed after the first version of this plan:

- `cc6d1bd5299306f7563ddef8ca79504b5ca6546c` — `ci: isolate release package consumer smoke`. Preserve that artifact-consumer isolation work; extend it rather than restoring an older release/check workflow.
- `fb2771ed46eacb7cd0ec8148eae7ee7403afa827` — `Make local integration tests parallel-safe on OATS runners`. This is directly relevant: `trellis-server` now accepts one combined `--local-nats-ports=<nats>,<monitor>,<websocket>` option, the Rust managed-NATS integration test selects ephemeral loopback ports, the TypeScript harness uses kernel-assigned loopback reservations, and the Rust live Check invocation no longer serializes all integration tests with `--test-threads=1`.
- `27f79d192204de9de78645b1006ceeccb59bf932` — `Align RC notes with parallel local runtime behavior`. Preserve the release notes and documentation wording that managed NATS ports are selectable for parallel local instances.

Therefore this plan **must not re-add three separate NATS port flags, must not reintroduce a global test serialization flag, and must not implement the superseded shared lock-file allocator described by the earlier plan**. The public Rust harness must choose ports internally and automatically. No normal caller API or CI invocation should require hand-assigned ports.

---

## 1. Problem and desired outcome

An outside Rust service or application repository must be able to put `trellis-test` in its Cargo development dependencies, start an isolated real Trellis deployment, provision generated participants, connect its normal generated service/caller APIs, perform live tests, and shut everything down. It must not need a Trellis source checkout or private workspace dependencies.

At the baseline:

- `crates/trellis-test` is unpublished **and now contains a substantial embedded Rust runtime harness**: `runtime.rs`, `admin.rs`, `config.rs`, `error.rs`, and `ports.rs`, plus live crate-local tests for boot, admin login, and apply/provision. Its public entrypoint is `TrellisTestRuntime::start(TrellisTestRuntimeOptions)`. This implementation landed after the previous plan baseline and supersedes the old "parent-exit helper only" premise. [S1]
- That embedded implementation is intentionally **not** the public architecture specified here. Its normal dependency closure includes private workspace crates such as `trellis-bootstrap`, `trellis-cli`, `trellis-local-nats`, `trellis-runtime`, and `trellis-runtime-apis`, and it starts Trellis in-process through runtime internals. It therefore cannot satisfy A02/A03/A04/A05/A06 and must be replaced rather than published as-is. [S1, S3]
- The embedded harness is useful as a behavioral oracle only. Preserve or improve its externally meaningful coverage (real runtime boot/readiness/cleanup, real administrator login, real participant apply/provision, automatic four-port isolation), but re-express those behaviors through the out-of-process public design and external consumer fixture in this plan. Do not preserve its private dependency or in-process startup mechanisms for compatibility; there are no shipped external consumers to protect. [S1, S2]
- Rust CLI live-test infrastructure remains repository-local in `crates/trellis/tests/integration/cli.rs`; its only dependency on the Rust `trellis-test` crate is the small Linux parent-exit helper. Inline that helper locally as already prescribed so the public facade does not depend back on its dev testkit. [S2]
- `trellis init config` already generates real bootstrap material; released archives already contain both `trellis` and `trellis-server`. [S4, S5]
- `trellis-server` now exposes the ordinary combined option `--local-nats-ports=<nats>,<monitor>,<websocket>`, validates three distinct nonzero values, preserves 4222/8222/8080 when omitted, and forwards the resolved values to the existing `LocalNatsPorts`. The repository Rust integration test already selects four ephemeral loopback ports for HTTP/NATS/monitor/WebSocket before running managed mode. The runtime HTTP listener still binds an unspecified IPv4 address. [S2, S6, S7]
- The TypeScript test harness now obtains ports directly from kernel-assigned loopback listeners and retries a fresh startup up to three times only when startup reports an address-in-use race; the prior shared advisory-lock-file convention is no longer current behavior. [S17, S19]
- Rust `AgentLoginChallenge::complete` currently persists the login session through the default session store. That behavior must not be used inside a parallel, isolated test harness. [S8]
- Generated Rust APIs use an ordinary published `trellis-rs` dependency. They can be delivered as generated source; they do not require publishing the implementation runtime. [S9]
- Current publication policy and release packaging intentionally allow only `trellis-rs` and `trellis-protocol`. Both must be updated deliberately, not disabled. [S10, S5]


### 1.2 Coordinator resolution — DESIGN BLOCKER 1 (current-main embedded harness)

**Decision: implement on current `origin/main`; do not base this work on the historical `dfaa557e` commit. Replace the newly landed embedded Rust harness with the out-of-process publishable architecture in this document.**

The implementation agent is explicitly authorized and required to supersede the current private embedded harness as follows:

1. Create the feature worktree from the current `origin/main` commit recorded above (or a later current `origin/main` if it advances before worktree creation). Do not reset, cherry-pick back to, or otherwise base the feature on the old plan commit. Preserve every unrelated change now present on `main`.
2. Treat the current `crates/trellis-test/src/runtime.rs`, `admin.rs`, `config.rs`, `ports.rs`, `error.rs`, and the three crate-local live tests as **transitional source**, not as a compatibility surface. No compatibility shim for `TrellisTestRuntime::start(TrellisTestRuntimeOptions)` is required. This crate has not been published as the public Rust testkit.
3. Replace `runtime.rs` with the required builder/out-of-process orchestration. Replace `admin.rs` with the generated-projection/public-boundary implementation. Replace `error.rs` with the stable public error model. Add `process.rs`, `sandbox.rs`, and `identity.rs` exactly as specified below. Remove the old embedded-only `config.rs` and `ports.rs` once their needed behavior has moved into `sandbox.rs`/process orchestration; do not leave dead private-runtime code in the publishable crate.
4. Remove all normal/build/optional/target-specific dependencies from `trellis-test` on `trellis-bootstrap`, `trellis-cli`, `trellis-local-nats`, `trellis-runtime`, `trellis-runtime-apis`, and direct `async-nats`. The only Trellis Cargo dependency remains published `trellis-rs` as fixed by A03; administration types come from the generated source projection fixed by A06.
5. Do not copy the embedded runtime's in-process `run_with_stop`, direct `seed_admin_credentials`, direct `LocalNats` ownership, or source-compilation path into the replacement. Bootstrap goes through the real `trellis` executable; runtime/NATS lifecycle goes through the real `trellis-server` executable; administrator operations use the generated projected API.
6. Preserve the useful **behavioral intent** of the current tests, but rewrite/move it to the prescribed external live fixture. The replacement must still prove: real server boot/readiness/shutdown; real local administrator bootstrap/login; real generated participant install/apply/provision; and automatic parallel-safe port selection. The current tests are not retained merely to exercise a deleted embedded implementation.
7. Inline the small Linux parent-death helper into `crates/trellis/tests/integration/cli.rs` and remove the facade crate's dev-dependency cycle exactly as already specified.
8. Record this blocker resolution in the evidence ledger under "Baseline drift / coordinator-approved plan amendments." It is **not** permission to change any other A01–A22 decision.

This resolution intentionally keeps the newer Live-work/runtime/protocol changes on `main` and makes the public testkit exercise those production binaries instead of linking to their implementation crates.

### 1.1 Definition of the external-user experience

After publication, an ordinary application manifest can contain the following, with the actual released version substituted by release tooling/documentation:

```toml
[dev-dependencies]
trellis-test = "<released-version>"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

The test harness uses the matching released `trellis` and `trellis-server` executables. A NATS executable must be explicitly supplied, discoverable on PATH, or explicitly requested through the server's pinned-download policy. Compilation of the test crate never downloads or builds those executables.

The application continues to author contracts in Trellis IDL and use generated participant facades. There is no new handwritten contract format, assertion language, suite registration mechanism, or Cargo test runner.

---

## 2. Locked architecture decisions

| ID | Required decision |
|---|---|
| A01 | Publish `trellis-test` from the existing `crates/trellis-test` directory. Keep the workspace. |
| A02 | The harness is an out-of-process orchestrator of normal production executables, not an embedded Trellis server. |
| A03 | Its only direct Trellis Cargo dependency is the published `trellis-rs` facade. A transitive `trellis-protocol` dependency through that facade is expected. |
| A04 | No normal, optional, target-specific, or build dependency on private Trellis packages. No source inclusion of handwritten runtime/bootstrap implementation. |
| A05 | Reuse bootstrap by executing `trellis init config`; reuse NATS management by executing `trellis-server` in managed-NATS mode. |
| A06 | Distribute generated administration API source inside `trellis-test`. Do not add a Cargo dependency on `trellis-runtime-apis`. Section 6 fixes the generation/projection mechanism. |
| A07 | Use real bootstrap, login, participant installation, deployment consent, provisioning, authorization, NATS, and storage. No bypasses or test-only server feature. |
| A08 | Reuse the existing combined `trellis-server --local-nats-ports=<nats>,<monitor>,<websocket>` surface exactly as it exists on the refreshed baseline; do not add alternate/separate NATS port flags. Add only the ordinary HTTP bind-address configuration field described in section 8.2. |
| A09 | Port assignment is fully automatic and private to the harness. Reserve HTTP/native-NATS/monitor/WebSocket ports with four real loopback `TcpListener` sockets using port `0`, hold all four until immediately before server spawn, and perform a bounded fresh pre-auth retry only for a proven bind race. Expose no public/manual port-selection builder in this release. Never mutate the caller process's environment. |
| A10 | Add a storage-free user-login completion operation to `trellis-rs`; preserve the existing CLI login operation's persistence/admin checks. |
| A11 | Public helpers provision identities and return the existing SDK connection-option types. Generated participant facades remain the actual connection boundary. No parallel client/service abstraction. |
| A12 | Initial supported workflows include service participants and app/agent callers authenticated as the sandbox administrator. Device activation and the full advanced TypeScript administration surface are not part of this release. |
| A13 | Every service registration gets a distinct deployment. Reusing one deployment across different service participants is forbidden. |
| A14 | The sandbox administrator is never substituted for a service's provisioned identity. Each app/agent caller gets a fresh, participant-bound session. |
| A15 | The crate version, CLI version, and server version must match exactly, including prerelease identifiers, ignoring SemVer build metadata. No silent compatibility fallback. |
| A16 | Default NATS acquisition is PATH lookup without downloading. `DownloadPinned` is an explicit option; Trellis CLI/server auto-download is not implemented. |
| A17 | Explicit async shutdown is the normal path. A supervisor independent of the caller's Tokio runtime performs best-effort cleanup on drop/cancellation/panic. |
| A18 | Normal Cargo build, package verification, Rustdoc, and pure unit tests do not start infrastructure. Live tests are explicitly selected and never silently skip missing prerequisites. |
| A19 | Ordinary `Check` owns correctness and the full live suite. Release verification adds package/archive consumer smoke checks, not a duplicate full behavioral suite. [S11] |
| A20 | An artifact-only consumer job with no repository checkout is a release gate. A workspace-only green build is insufficient. |
| A21 | Linux and macOS on the existing x86_64/aarch64 release targets are the supported runtime platforms. Unsupported platforms return a clear error from startup; do not introduce a library-wide compilation error. |
| A22 | Merge requires explicit approval of the exact commit/base pair. Publication requires separate authorization. |

### 2.1 Final dependency boundary

```text
Outside application/service repository
  |-- trellis-rs + its generated application package
  `-- [dev-dependency] trellis-test
       |-- trellis-rs + ordinary crates.io libraries
       |-- package-local generated administration API source
       `-- executable boundary
            |-- trellis init config
            `-- trellis-server --local-nats... all
                 `-- nats-server
```

The following **normal/build/optional dependency closure is forbidden** for `trellis-test`:

```text
trellis-bootstrap, trellis-local-bootstrap, trellis-local-nats,
trellis-runtime, trellis-runtime-apis, trellis-events-runtime,
trellis-jobs-runtime, trellis-idl, trellis-codegen-rust,
trellis-codegen-ts, trellis-cli, trellis-server, xtask
```

Development tools may use those packages to build/generate/test the repository. That does not authorize putting them in the exported library's dependency closure.

---

## 3. Worktree and change-control procedure

1. Read repository `AGENTS.md`, `design/README.md`, and the task-relevant reading set in section 23. Use the repository's Rust specialist guidance where available. Do not claim to have used a skill unavailable in the implementation environment.
2. Inspect the existing worktree status. Do not reset, stash, clean, or overwrite another person's changes.
3. Fetch the remote and create a new worktree from current `origin/main`, not the retired `rs` branch, not another repository, and not the old reviewed SHA by default.
4. Record the actual base SHA and compare it with this plan's baseline. **Base the worktree on current `origin/main`, never on the historical `dfaa557e` plan baseline.** Preserve subsequent Live-work, runtime/protocol, release/OIDC/Podman, and CI changes. Stop only the affected workstream when new semantic drift contradicts the plan; report the exact conflict.

Suggested commands, run from an existing clone after confirming `origin` is the intended repository:

```sh
git remote -v
git status --short
git fetch origin
BASE_SHA="$(git rev-parse origin/main)"
git worktree add -b feat/public-rust-trellis-test \
  ../trellis-public-rust-test origin/main
cd ../trellis-public-rust-test
printf '%s\n' "$BASE_SHA"
```

If that branch or path exists, choose a non-colliding numeric suffix. Do not repurpose an existing worktree. That naming adjustment is not an architectural decision.

Copy this plan to `trellis-test-public-implementation-plan.md` at the worktree root. Create `trellis-test-public-evidence.md` with the template in section 21. Commit both, so reviewers can associate requirements and evidence with the implementation. Do not commit credentials or sandbox data.

Use a small sequence of meaningful commits corresponding to section 17. Follow repository commit-signing policy; never use someone else's signing identity. Push only the feature branch, and only when repository access permits it. Open a PR against `main`; do not enable auto-merge.

---

## 4. Deliverable and file map

Existing paths are source anchors, not an instruction to rewrite every file. New paths below are prescribed.

| Path | Required work |
|---|---|
| `crates/trellis-test/Cargo.toml` | Publishable metadata, dependency boundary, and package include list; no checkout-dependent test target. |
| `crates/trellis-test/README.md` **new** | External-user installation, binaries, runnable service/caller example, lifecycle and troubleshooting. |
| `crates/trellis-test/src/lib.rs` | Public exports and Rustdoc; private generated projection mount. |
| `crates/trellis-test/src/runtime.rs` **replace current embedded implementation** | Builder/startup orchestration, runtime state, public getters and shutdown. No `trellis-runtime` linkage or in-process server. |
| `crates/trellis-test/src/process.rs` **new** | Executable validation, child ownership/supervision, bounded logs and termination. |
| `crates/trellis-test/src/sandbox.rs` **new** | Directories, child environment, port leases, retention and safe cleanup. |
| `crates/trellis-test/src/config.rs`, `crates/trellis-test/src/ports.rs` **current transitional files** | Remove after the required behavior is migrated into out-of-process bootstrap orchestration and `sandbox.rs`; neither file may retain private-runtime dependencies or an alternate public port API. |
| `crates/trellis-test/src/admin.rs` **replace current embedded implementation** | Real bootstrap/login automation and typed participant/deployment/provisioning calls through the projected generated API; no `trellis-runtime-apis`/`trellis-cli` dependency. |
| `crates/trellis-test/src/identity.rs` **new** | Service/client identity wrappers and existing-SDK connection options. |
| `crates/trellis-test/src/error.rs` **replace current embedded error surface** | Stable error categories and redacted diagnostic data. |
| `crates/trellis-test/src/runtime_api/**` **new, generated** | Exact generated Rust source projection from `crates/runtime-apis/src/**`. |
| `integration/fixtures/testkit/tests/live.rs` **new** | Single explicitly selected Cargo live-test target; cases may use sibling modules. |
| `integration/fixtures/testkit/**` **new** | Small external-consumer fixture, its IDL, generated application package and live smoke tests. |
| `crates/server/src/main.rs` | **No new NATS port API.** Reuse and preserve the refreshed baseline's combined `--local-nats-ports` parsing/validation/forwarding. Touch only if a narrow test or diagnostic adjustment is required by this plan. |
| `crates/runtime/src/config/mod.rs` and existing config validation/accessors | Optional HTTP bind address, default-preserving parsing and validation. |
| `crates/runtime/src/server.rs` | Bind the configured HTTP address rather than hard-coding unspecified IPv4. |
| `crates/bootstrap/src/runtime_config.rs` | Initialize the new optional HTTP field without changing defaults. |
| `crates/bootstrap/src/nats_config.rs` | Quote/escape host-local NATS path values safely, with config-validation tests. |
| `crates/trellis/src/auth/browser_login.rs` | New storage-free session completion method; refactor the existing persistence path without bypassing checks. |
| `crates/trellis/src/lib.rs` | Expand the intentional public-package allowlist; retain private-package protection. |
| `crates/trellis/Cargo.toml`, `crates/trellis/tests/integration/cli.rs` | Remove the old process-helper-only dependency on `trellis-test`; keep that existing CLI test's tiny Linux guard local. Preserve its refreshed-baseline automatic ephemeral port selection and parallel-safe behavior. Avoid a facade/testkit development cycle. |
| `xtask/src/main.rs`, relevant generation/install implementation | Project generated admin source after runtime SDK generation; register the new fixture for generation. |
| `xtask/src/release/versioning.rs` and associated tests | Cover any new version-bearing fixture manifests and release preparation order. |
| `scripts/verify-rust-test-package.sh` **new** | Artifact-only package/consumer validation entrypoint; portable Bash for Linux/macOS. |
| `.github/workflows/check.yml` | Live harness coverage, generated-current coverage, package-consumer producer/isolated consumer and macOS smoke. |
| `.github/workflows/release.yml` | Package third public crate, archive consumer gate, ordered publish entry and artifact transfer. Preserve unrelated release changes. |
| `.github/workflows/pages.yml`, `docs/src/lib/docs.ts` | Include/discover `trellis-test` Rustdoc using the existing docs mechanism. |
| `RUST.md`, existing testing/release/library guides and `docs/static/llms*.txt` | Exact documentation updates in section 16. |
| `CHANGELOG.md` | State the new external Rust test capability and intentional new server/auth APIs. |

Do not create a second harness crate, a published "internal" support-crate family, a custom registry product, or a frontend page.

---

## 5. Package contract

### 5.1 Manifest

Keep workspace-inherited version, edition, license, repository, and lints. Set:

```toml
[package]
name = "trellis-test"
version.workspace = true
edition.workspace = true
license.workspace = true
repository.workspace = true
description = "Isolated live Trellis runtimes for Rust integration tests."
readme = "README.md"
publish = ["crates-io"]
include = ["Cargo.toml", "README.md", "src/**"]

[lints]
workspace = true
```

Use an explicit allowlist of shipped source files. The archive must contain the generated `src/runtime_api/**` tree. Put all repository live-harness tests in the separate unpublished consumer fixture at `integration/fixtures/testkit/tests/live.rs`; do not declare a checkout-dependent test target in the published crate. Package-local unit tests and Rustdoc examples must be self-contained and must not start infrastructure.

The new fixture is outside the root workspace's normal/default test selection. Its named `live` target is explicitly invoked by the commands in section 18 and by CI. No `test-support`, embedded-runtime, unsafe-auth, source-checkout, or live-integration feature is needed in the public testkit. Never gate production library behavior to make a test pass.

### 5.2 Direct dependencies

Use the workspace's existing versions when available. Add only the following direct dependency families, using the existing compatible version where already used in the repository:

- `trellis-rs` with both a local path and the release-managed registry version; no `runtime-internals` feature.
- `tokio` for async orchestration/time, with only the features actually required by this implementation.
- `reqwest` with default features disabled and `json`/`rustls` enabled, consistent with the facade.
- `serde`, `serde_json`, `thiserror`, `tempfile`, `url`, `base64`, `sha2`, `ulid`, `toml_edit`, and `semver`.
- `futures-util` because the embedded generated API source requires it.
- `libc` under a Unix target dependency for process groups, wait/locking primitives, and the existing Linux parent-death behavior.

Generate fresh session/identity keypairs through `trellis_rs::auth::generate_session_keypair`, not another implementation of Trellis proof signing. If a listed library is unnecessary, omit it and record that omission; adding an unlisted dependency family requires coordinator approval. Do not depend directly on `async-nats` just to bypass generated clients.

Cargo permits a local `path` plus registry `version`; published dependency resolution uses the registry. A path-only normal dependency is not a distributable solution. Development dependencies of the consuming application do not turn the testkit's own implementation dependencies into development-only dependencies. [S12]

### 5.3 Eliminate misleading successful builds

The public library must not contain `env!("CARGO_MANIFEST_DIR")` ancestor traversal, absolute checkout paths, `include!` of a sibling implementation crate, consumer-time code generation, `cargo run`, or an automatic source clone/build path.

Do not fix publication by using `--no-verify`, wildcard versions, a consumer-side private patch, optional private dependencies, or by adding versions to every private workspace development dependency. Versionless repository-only development dependencies are a separate Cargo packaging concern. [S12, S13]

---

## 6. Generated administration source: exact packaging mechanism

### 6.1 Use the existing canonical generated runtime SDK

Do **not** create a new administration contract or duplicate the TypeScript test contract. Use the existing generated output `crates/runtime-apis/src/**`, whose canonical inputs are the runtime's Trellis package and IDL. [S9]

Extend the existing `cargo xtask install` implementation so that, **after** regenerating `crates/runtime-apis`, it deterministically projects its Rust source tree into `crates/trellis-test/src/runtime_api/`:

1. Copy the complete source tree byte-for-byte, including `lib.rs`, API modules, participant modules, wire types, and any generation-owned relative source assets.
2. Do not copy its `Cargo.toml`, lockfile, target directory, or any runtime implementation source. This is a generated-source projection, not a nested Cargo dependency.
3. Replace the destination tree atomically from a staging directory so removed generated files cannot remain stale.
4. No regex transformation of generated Rust, AST rewriting, or selective hand-maintained list of generated RPCs.
5. Track the projection in Git, consistent with the current generated-current checks. Mark its ownership in the crate README and in generation tooling: generated, never hand-edit.
6. Run installation twice and require no second-pass changes. Compare a sorted relative-path/content digest manifest of source and projection.
7. Run the projection again after release-version preparation/regeneration, before packaging. No hard-coded `0.100.0` snapshots may survive a version bump.

### 6.2 Compile it as private modules, not as a crate

Mount the generated root inside `trellis-test`:

```rust
#[path = "runtime_api/lib.rs"]
#[allow(missing_docs)]
mod runtime_api;

// Existing generated code uses crate-root paths such as crate::apis,
// crate::types, crate::__types and crate::PaginationError.
// Preserve those paths inside this crate without exporting them to users.
#[allow(unused_imports)]
pub(crate) use runtime_api::*;
```

The crate's **public** exports are the explicit testkit types in section 7. Generated administration modules and their wire types must not leak through public signatures. Scoped allowances are for generated output only; do not disable missing-docs or warnings for the handwritten library.

This mounting is intentionally based on the existing generated root structure. [S9] If a future generator adds a conflicting root name or an external include, treat that as a generation integration failure. Fix it through the generator/projection contract with coordinator review; do not hand-patch the copied Rust.

### 6.3 Bootstrap administrator identity

The initial administrator login uses the canonical generated built-in CLI participant:

```text
runtime_api::participants::trellis_cli::Participant
```

Obtain its ID through the generated descriptor. It is already a trusted built-in participant in the real server. Do not try to log in as a newly invented test administrator participant before it is installed, and do not impersonate the built-in ID with newly fabricated evidence. [S14]

The generated Auth API client/descriptors then perform installation, deployment, provisioning, consent-policy, and identity inspection calls. No raw handcrafted RPC subjects or generic JSON-RPC escape hatch is part of the public testkit API.

---

## 7. Public API contract

The following are **new required APIs**, not claims that these methods exist today. Implement the listed names and semantics. Signatures below omit bodies; they are a specification, not an existing runnable example.

### 7.1 Runtime and builder

```rust
pub struct TrellisTestRuntime;
pub struct TrellisTestRuntimeBuilder;

pub enum NatsSource {
    Path(std::path::PathBuf),
    PathLookup,
    DownloadPinned,
}

pub enum WorkdirRetention {
    OnFailure,
    Always,
    Never,
}

pub struct TestTimeouts {
    pub startup: std::time::Duration,
    pub request: std::time::Duration,
    pub shutdown: std::time::Duration,
}

impl TrellisTestRuntime {
    pub fn builder() -> TrellisTestRuntimeBuilder;
    pub fn trellis_url(&self) -> &str;
    pub fn nats_url(&self) -> &str;
    pub fn websocket_url(&self) -> &str;
    pub fn workdir(&self) -> &std::path::Path;

    pub async fn install_participant<P>(
        &mut self,
    ) -> Result<InstalledParticipant, TrellisTestError>
    where P: trellis_rs::generated::ParticipantDescriptor;

    pub async fn register_service<P>(
        &mut self, name: &str,
    ) -> Result<TestServiceIdentity, TrellisTestError>
    where P: trellis_rs::generated::ParticipantDescriptor;

    pub async fn register_client<P>(
        &mut self, name: &str,
    ) -> Result<TestClientIdentity, TrellisTestError>
    where P: trellis_rs::generated::ParticipantDescriptor;

    pub async fn shutdown(&mut self) -> Result<(), TrellisTestError>;
}
```

Builder methods consume and return `Self`:

```text
cli_binary(path: impl Into<PathBuf>)
server_binary(path: impl Into<PathBuf>)
nats(source: NatsSource)
workdir_parent(path: impl Into<PathBuf>)
retention(policy: WorkdirRetention)
timeouts(timeouts: TestTimeouts)
start(self) -> async Result<TrellisTestRuntime, TrellisTestError>
```

Do not add command strings, arbitrary extra server flags, shell interpolation, attach-to-shared-runtime, or mutable-config escape hatches in this release.

### 7.2 Defaults and validation

| Setting | Required default / rule |
|---|---|
| Trellis CLI | Explicit builder path, otherwise `TRELLIS_TEST_CLI_BIN`, otherwise fail. |
| Trellis server | Explicit builder path, otherwise `TRELLIS_TEST_SERVER_BIN`, otherwise fail. |
| NATS | `PathLookup`; explicit `nats(...)` wins. In the absence of that builder choice, nonempty `TRELLIS_TEST_NATS_BIN` selects `Path`. |
| Sandbox parent | `std::env::temp_dir()`; always create a fresh child. Never adopt the caller's directory itself. |
| Retention | `OnFailure`. |
| Startup deadline | 120 seconds; 600 seconds when `DownloadPinned` is explicitly selected and no timeout override was supplied. |
| Request/registration deadline | 30 seconds for an entire public registration/install operation; each internal request is additionally bounded by the remaining operation/startup/shutdown deadline. |
| Shutdown deadline | 30 seconds. |
| Startup polling interval | 50 milliseconds; no fixed sleep used as proof of readiness. |
| Readiness HTTP request | At most 1 second and never past the startup deadline. |
| Diagnostic tail | Last 16 KiB per output stream; incremental UTF-8 handling; see section 12. |
| Network origin | `http://127.0.0.1:<allocated-http-port>`. No external/shared URLs. |
| Port selection | Fully automatic; four kernel-assigned loopback reservations per attempt. No public/manual port input. |
| Versions | Exact package/CLI/server SemVer match, ignoring build metadata only. |

Reject zero durations, empty explicit executable paths, non-executable/non-regular resolved executable targets, and unsupported runtime platforms before side effects that start a server. Relative paths are resolved against the caller's original current directory before selecting a sandbox working directory. Accept spaces and non-UTF-8 filesystem paths through `PathBuf`/`OsString`; do not assemble shell commands.

A missing prerequisite is an error, not a skipped test. An explicit but invalid setting must not silently fall back to another source.

### 7.3 Identity and result types

```text
InstalledParticipant
  participant_id() -> &str
  installed_revision() -> u64

TestServiceIdentity
  name() -> &str
  participant_id() -> &str
  deployment_id() -> &str
  instance_id() -> &str
  seed() -> &str
  connect_options() -> trellis_rs::service::ServiceConnectOptions<'_>

TestClientIdentity
  name() -> &str
  participant_id() -> &str
  login_session_id() -> &str
  connect_options() -> trellis_rs::client::UserConnectOptions<'_>
```

Store the runtime URL and request timeout in the identity values so their connection options are fully specified. Service options use the provisioned seed and name. User options use the new session's ID, seed, and exact generated participant ID. Both retain the SDK's normal authorization/bootstrap validation; never set a broad insecure-origin bypass for a loopback origin. [S15]

Expose service `seed()` because real outside service processes need it. Do not expose the sandbox administrator's password/session or the client session seed as convenient public fields. Implement redacted `Debug`; secrets must never be printed automatically. No generic serialization derive on secret-bearing types.

`TrellisTestError` exposes `kind() -> TrellisTestErrorKind`, `stage() -> TrellisTestStage`, `message() -> &str`, `workdir() -> Option<&Path>`, `server_code() -> Option<&str>`, `stdout_tail() -> &str`, `stderr_tail() -> &str`, and `cleanup_error() -> Option<&TrellisTestError>`. It implements `std::error::Error` with a safe retained source. Store a secondary cleanup failure in an optional boxed error, not by replacing the primary error.

Identity objects do not own the generated clients/services that the application connects. The application owns those transports and service tasks and must stop/drop them before shutting down its test runtime. The harness owns only its own administration connection and infrastructure processes. Document this clearly.

### 7.4 Connection usage decision

Use two explicit steps, rather than a second facade system:

```text
identity = runtime.register_service::<GeneratedProviderParticipant>("provider").await
provider = GeneratedProviderService::connect(identity.connect_options()).await

identity = runtime.register_client::<GeneratedCallerParticipant>("caller").await
caller = GeneratedCallerClient::connect(identity.connect_options()).await
```

The agent must deliver a compiling concrete example with actual generated names from the new fixture. Do not publish placeholder code as a working example. Do not add a `connect_client` wrapper that erases the generated caller type just to imitate the TypeScript spelling.

### 7.5 Naming and lifecycle state

Names are human-readable labels, not contract identities or filesystem paths. Accept trimmed nonempty Unicode text of at most 128 characters; reject control characters. Use generated IDs for all authorization boundaries and independent random identifiers for storage paths.

Names are unique across registrations within a runtime. A second registration with an already-reserved name returns `DuplicateName`; it does not reuse a deployment, silently replace a participant, or provision another identity. A failed registration retains that name as failed until shutdown, preventing ambiguous retries after partially completed remote writes. The error identifies the failed phase and safe public IDs; callers may use a new name or a new runtime.

`install_participant` is idempotent for an identical generated participant ID and package digest. Registration methods require a running runtime. `shutdown` is idempotent. Calls after shutdown return `RuntimeStopped`; they do not restart anything.

All mutating public methods take `&mut self`; there is no additional public lock/handle-sharing API. Separate runtime instances must operate concurrently without a global test serialization lock.

---

## 8. Ordinary server configuration and automatic port isolation

### 8.1 Reuse the existing managed-NATS port surface

The refreshed baseline already has the required production server capability. `trellis-server` accepts:

```text
--local-nats-ports=<nats>,<monitor>,<websocket>
```

The existing startup policy requires exactly three distinct nonzero `u16` values, rejects the option for `ExternalConfigured`, preserves 4222/8222/8080 when omitted, and passes the resolved `LocalNatsPorts` into managed NATS. **Do not replace this with three new flags, do not add a test-only port flag, and do not change the normal defaults.** [S6]

The Rust public testkit always supplies this existing combined option using its internally selected ports. Port numbers are an implementation detail of the returned runtime, not a configuration burden on the outside repository. There is no `.ports(...)`, `.nats_ports(...)`, fixed base-port, worker-index, test-name hash, or environment-variable port override in the initial public API.

Retain/extend the existing server argument-policy tests only as needed to protect the behavior the testkit relies on: omitted defaults, exactly three explicit values, zero/duplicate rejection, external-mode rejection, and `check` parity. Do not rewrite already-green refreshed-baseline tests merely to make the new crate own them.

### 8.2 HTTP listener address

Add `bind_address: Option<std::net::IpAddr>` to the normal runtime `HttpConfig` and expose its resolved value through the existing configuration/accessor pattern. Its TOML spelling is:

```toml
[http]
bind_address = "127.0.0.1"
```

An omitted address retains the current IPv4 unspecified bind address, `0.0.0.0`. An invalid address is a configuration error. Use the resolved address in `crates/runtime/src/server.rs` when constructing the listener socket address. The existing HTTP port, public origin, and origin authorization checks remain separate concerns. Update all `HttpConfig` struct literals mechanically, including bootstrap generation, with `None` unless the caller intentionally requests a particular address. [S7]

The harness edits the CLI-generated TOML with `toml_edit` to set only:

```toml
[http]
bind_address = "127.0.0.1"
rate_limit_max = 0
```

The rate setting uses the existing ordinary configuration surface so parallel local test setup does not run into the generated 60-request rate limit. Do not introduce a fast-password test profile, disable signature checks, or change production defaults. Bootstrap already enables the normal local-identity flow. Preserve the generated origins, paths, credentials, keys, authorization configuration, and the refreshed 30-second/5-second local lease defaults. [S16]

### 8.3 Four automatic kernel port leases

Every startup attempt owns four distinct loopback port reservations: Trellis HTTP, native NATS, NATS monitor, and NATS WebSocket. The harness must not ask the caller to provide any of them.

Implement the allocator in `sandbox.rs` as follows:

1. Bind four `std::net::TcpListener`s to `127.0.0.1:0`. Let the kernel select the port for each reservation; read each actual port with `local_addr()`.
2. Keep every listener open until all four reservations have succeeded, so one attempt cannot select the same live port twice and concurrently starting harness instances/processes cannot select a port that is still reserved by another allocator.
3. Do not scan a hand-picked private range, derive ports from PIDs/test names/worker numbers, create shared lock files, or serialize allocation/runtime execution behind a global mutex. The refreshed TypeScript harness and Rust CLI integration have moved to kernel-assigned loopback reservations; follow that direction. [S2, S17]
4. Generate the bootstrap bundle and finish all pre-spawn preparation while the four sockets are still held. The CLI bootstrap does not need those network listeners and must not force early release.
5. Immediately before spawning `trellis-server`, collect the four selected numbers, close all four reservation sockets in one narrow operation, then spawn the server without running unrelated work in between. Pass the NATS triple through the existing combined `--local-nats-ports=<nats>,<monitor>,<websocket>` option and the HTTP port through the generated config.
6. Once the server is spawned, the harness no longer claims to own the temporary reservation sockets; it owns the server process, whose managed NATS child owns the three NATS listeners and whose runtime owns the HTTP listener.
7. If any reservation itself fails, release every already-open reservation from that attempt and return/consider retry according to the common startup deadline. Never leave one socket held while abandoning the rest.

No user-visible stable ordering beyond the internal role mapping is promised. For diagnostics and tests, retain the selected ports in the runtime state and expose only the already-specified URL getters (`trellis_url`, `nats_url`, `websocket_url`), not raw mutable port controls.

### 8.4 Narrow bind-race retry policy

Closing reservation sockets before `trellis-server` binds them leaves an unavoidable operating-system handoff race with unrelated processes. Make that race self-healing for ordinary parallel tests without hiding real startup failures:

1. Validate binaries/versions and immutable builder inputs once before the attempt loop.
2. Allow at most **three** startup attempts, sharing the one overall startup deadline; this matches the refreshed TypeScript harness's bounded retry count. [S17]
3. Each retry creates a **fresh sandbox**, fresh four-port reservation set, fresh bootstrap bundle, and fresh server process. Never reuse partially written NATS/SQLite state after a bind conflict.
4. Retry only while startup is still pre-authentication and the failure is classified as `PortConflict`. Classification is limited to a selected port failing with the server/local-NATS current address-in-use diagnostics (including `port <selected-port> is already in use` or the OS `Address already in use` error while binding one of the four selected listeners). Do not retry arbitrary server exits, invalid configuration, download/checksum failures, auth failures, or test assertions.
5. The failed attempt must be fully reaped and its sandbox handled according to failure-retention policy before the next attempt begins. There may never be two live server attempts for one `start()` call.
6. Once first-admin bootstrap/login mutation starts, startup is no longer retryable. A failure after that point returns the original failure and performs cleanup; it does not create another deployment.
7. If all three attempts hit a bind race, return `TrellisTestErrorKind::PortConflict` with bounded diagnostics and the attempted role/port values. Do not instruct the caller to pick ports manually.

This retry exists to close the small release-to-bind window; it is not a generic resilience loop. The normal successful path performs one startup attempt.

### 8.5 Path rendering

Execute commands with `Command` and `OsString` arguments. For paths written into TOML, use the TOML serializer. The existing NATS renderer interpolates host paths; make the narrowly required renderer change to quote/escape NATS path values correctly for spaces, quotes, and backslashes. Apply that fix in `crates/bootstrap/src/nats_config.rs`, with real NATS config validation tests; do not copy the renderer into the testkit. [S7]

Non-UTF-8 executable paths must work through OS arguments. Non-UTF-8 sandbox paths cannot necessarily be represented in the existing UTF-8 configuration formats: reject those before configuration generation with `InvalidConfiguration`, identifying the path safely. Do not use lossy conversion to silently select a different directory. This is the explicit exception to otherwise preserving native paths.

---

## 9. Executable selection and startup state machine

### 9.1 Binary validation

Resolve and canonicalize explicit CLI/server paths before changing any child working directory. Resolve NATS PATH lookup once to a concrete executable path using the caller's snapshotted PATH; pass that path with `--local-nats=<PATH>`. Do not let the server select a different executable later. The server remains responsible for its existing trusted-NATS-path validation.

Run `trellis --format json version` and decode its `version` field. Run `trellis-server --version` and decode the single Clap version line. Both are normal existing commands. [S18, S6] Require successful exit and a valid version; do not accept an arbitrary version-looking substring from failed output. Allow unknown additional fields in the CLI version object, not multiple ambiguous version objects.

Compare with `env!("CARGO_PKG_VERSION")` by major, minor, patch, and prerelease identifiers. Ignore only build metadata. Error output identifies the executable, expected version, and actual version without suggesting that the consumer disable validation. The initial release has no relaxed-version option.

Apply a maximum ten-second deadline to each version command, additionally bounded by the overall startup deadline. Version checks run through the owned-process facility too: a hanging command must not outlive the failed startup.

### 9.2 Sandbox and environment

Always create a new random `trellis-test-<id>` child directory, mode 0700 on Unix, with an ownership marker containing a format version and a random ownership ID. It is not a shared runtime and not an adopted directory. Create private `home`, `config`, `data`, `state`, `cache`, `runtime`, and `logs` subdirectories. Secrets/log files must not be group/world readable.

Build child environments without mutating the parent. Use `env_clear()` and explicitly supply:

- The snapshotted PATH, `HOME`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME`, `XDG_CACHE_HOME`, and `XDG_RUNTIME_DIR`, with all profile paths pointing into the sandbox.
- `TMPDIR` pointing into the sandbox, `NO_COLOR=1`, `TOKIO_WORKER_THREADS=2`, and `TRELLIS_CACHE_DIR` pointing into the sandbox cache.
- Any runtime-config variable actually required by existing code, set to the same explicit sandbox config; do not inherit an ambient `TRELLIS_CONFIG`.

Do not inherit credentials, proxy variables, telemetry-export destinations, arbitrary `TRELLIS_*` overrides, or user Rust/Cargo configuration into runtime subprocesses. CLI/server are prebuilt executables, not Cargo invocations. Explicit pinned download uses the normal server acquisition path; no shared writable global cache is introduced. A caller needing an already-installed NATS should supply that path.

The harness's own HTTP client uses explicit loopback URLs, disabled redirect following, disabled ambient proxies, bounded request/body sizes, and the normal SDK origin checks. It must not read or write a default user login store. Path/port environment choices are snapshots, not settings watches.

### 9.3 Ordered stages

Implement one state machine with a single ownership guard acquired before the first side effect:

```text
Validate inputs and supported platform
  -> create private sandbox and process supervisor
  -> validate CLI/server versions and NATS source
  -> enter bounded startup-attempt loop
     -> create fresh sandbox
     -> reserve four kernel-assigned loopback ports
     -> execute real CLI bootstrap
     -> set HTTP bind/rate configuration
     -> release all four reservation sockets immediately before spawn
     -> start normal server with managed NATS using existing combined NATS-port option
     -> observe its real startup/bootstrap output and HTTP availability
     -> on classified pre-auth port conflict: fully clean attempt and retry (max three total)
  -> complete first-administrator bootstrap
  -> authenticate and connect the generated administrator client
  -> perform a real generated administration RPC
  -> return Running runtime
```

Use the CLI's existing JSON mode for bootstrap. Supply `init config` with `--out <sandbox-config-directory>`, `--trellis-port`, `--nats-server-url`, `--nats-websocket-url`, and `--public-origin`, all derived from the owned ports. The public and OAuth origin is the same `http://127.0.0.1:<http-port>`. Validate successful exit and the existing `generated`, `trellisConfig`, and `natsConfig` response fields; verify returned paths resolve inside the owned sandbox. Do **not** run `trellis init admin`, which follows a different storage-mutating path. [S4]

Start the server in `all` mode, with the explicit config, the refreshed baseline's single `--local-nats-ports=<nats>,<monitor>,<websocket>` argument, and exactly one NATS policy. Use `--local-nats=<resolved-path>` for PATH/explicit sources or `--nats-download` for `DownloadPinned`. Do not use `--system`, attach to a shared NATS instance, or fall back to a source checkout. All four ports are chosen internally; the caller supplies none.

Capture both stdout and stderr from the moment of spawn. Recognize the current production bootstrap URL formats: JSON `adminAccountUrl`/`bootstrapUrl` at the root or under `fields`, and the explicit `TRELLIS_ADMIN_BOOTSTRAP_URL=` line. These are startup outputs, not a reason to add a test-only endpoint. Validate that the URL has the exact owned origin and a nonempty `adminAccountToken`; never follow arbitrary output URLs. [S19]

Do not equate "TCP accepts connections" with ready. Startup succeeds only after that owned bootstrap token is consumed successfully through the real account flow, a proof-bound administrator session connects, and a generated Auth `Sessions.Me` call succeeds with the intended administrator identity. That establishes a usable authenticated Trellis boundary. Service registration and test calls subsequently verify the runtime features they need; do not fabricate a universal all-subsystems health assertion.

Every stage observes child exit and the common deadline. On failure, return the originating stage/error plus bounded diagnostics and await owned-process cleanup before completing `start`. A cancellation of the `start` future transfers cleanup to the supervisor; it must not drop a raw child handle and leave it running.

### 9.4 Startup result ownership

Keep the supervisor, selected endpoint metadata, sandbox ownership guard, administrator transport, and any log-reader handles inside the returned runtime. No static global runtime cache. No runtime shared by test name. No external discovery/mDNS. The supervisor owns the process handles from spawn through reaping, including short-lived version/bootstrap commands.

---

## 10. Storage-free authentication and real local login

### 10.1 New normal SDK operation

Add this public, Rustdoc-documented method to the existing `AgentLoginChallenge`:

```rust
pub async fn complete_session(
    &self,
    trellis_url: &str,
) -> Result<AdminSessionState, TrellisAuthError>;
```

`AdminSessionState` is the existing session-data type. Its historical name does not establish administrator privilege; document this explicitly for the new method. Do not invent a second session credential format.

The method must reuse the current poll, signed bind, response validation, origin validation, and session-field construction from `complete`. It returns the bound session data in memory, without loading/saving/clearing the default store and without requiring the user to have administrator privileges. Do not duplicate session-proof construction in the testkit. [S8]

Refactor existing `complete` to call `complete_session`, then preserve its existing persistence behavior and administrator verification flow. Existing CLI login callers keep their normal semantics. The harness uses only `complete_session`; it wraps the operation in its deadline. A successful bound non-admin session is not an authentication failure for this new operation, while existing admin-only `complete` must still enforce its privilege requirement.

### 10.2 First administrator

Generate a unique password using a fresh random 32-byte value encoded base64url, independent from every signing seed actually used by a session or service. Use the fixed local username `trellis-test-admin` inside each isolated runtime.

POST to `/auth/account-flow/<encoded-adminAccountToken>/local-password` with that username/password and require the existing successful `status: "created"` response. Use the production HTTP behavior as the contract. Do not insert database rows, issue your own administrator context, or disable first-admin checks. [S20]

### 10.3 Complete a local user flow

For both the canonical built-in CLI participant and each generated app/agent caller:

1. Call `trellis_rs::auth::start_agent_login` with the exact participant ID, owned runtime URL, and `allow_insecure_origin=false`.
2. Read the challenge's login URL, validate its origin, and extract the nonempty `flowId` query parameter.
3. Generate a fresh 32-byte portal binding. Its secret is base64url(raw bytes); its digest is base64url(SHA-256(raw bytes)), **not** the hash of the encoded secret.
4. POST `/auth/login/local` with `flowId`, username, password, and `portalBindingDigest`.
5. Read `/auth/flow/<flowId>` and, when authenticated/approval-required, POST `/auth/flow/<flowId>/portal` with `Origin` equal to the owned origin and `trellis-portal-binding` equal to the secret.
6. When approval is required, submit the exact server-computed consent selection described below to `/auth/flow/<flowId>/approval`, with the same origin and binding header.
7. Require a completed/approved flow, then invoke `complete_session`. Verify the returned session names the intended participant and owned origin. Construct the normal SDK connection options from those credentials.

These requests automate the existing local-login protocol; they do not define a new RPC contract. Use generated types wherever available. Private decoding types for the already-existing HTTP envelope may contain the exact fields consumed by this protocol, with source references and boundary tests. Do not hand-author canonical RPC descriptors, subjects, package evidence, or proof algorithms. [S21]

Consent uses the server's latest `consentView`: `installedRevision`, `expectedGrantRevision`, `decisionDigest`, required-and-eligible capabilities with their IDs/digests, required-and-eligible resources with their exact requested commitments, and the real companion decision when present. Submit `decision: "approve"` and `approval.mode: "capabilities"`. Reject denied, expired, mismatched, malformed, and ineligible-required transitions. Unknown HTTP response fields may be ignored; missing required fields must fail. Do not auto-follow redirects. [S21]

For a newly installed app/agent caller, configure the sandbox's ordinary built-in portal consent ceiling through generated `Auth.Portals.GrantOverrides.Put`, using the caller participant ID and capability IDs supplied by its server-computed consent view. Use no synthetic capability names, no wildcard grants, empty group/role mappings, and a fresh idempotency key. Refetch the consent view after changing policy before submitting approval. This is sandbox-only setup with the real administrator; it must not expand that caller's declared contract. [S20]

### 10.4 Administrator versus caller

The canonical administrator session connects using the built-in generated CLI participant. Require a real administrator identity before retaining its connection internally. All installation/provisioning calls use generated Auth descriptors over that connection.

A caller registration must start a **new** session for `P::ID`, even though the local test user is the sandbox administrator. Do not return, clone, or relabel the administrative transport as a generated application caller. The caller remains limited by its participant surface and the real server's authorization semantics.

The first release does not simulate arbitrary non-admin human roles. Document the test-user role instead of claiming authorization-policy coverage it does not provide. Service credentials remain independent provisioned identities. Tests that need denied capabilities must use real restricted participants and real server decisions, not a renamed admin connection.

---

## 11. Participant installation and identity provisioning

### 11.1 Common descriptor handling

Use the caller's generic generated `ParticipantDescriptor`: validate it, obtain `P::ID`, `P::PATH`, `P::KIND`, and `P::package_evidence()`. Use the generated evidence's root digest and original evidence representation. Convert between the SDK evidence representation and the generated administration input with checked serialization only where their Rust wrapper types differ; never reconstruct or weaken the evidence. [S22]

Keep an internal map by participant ID containing package digest and server-confirmed installed revision. An identical ID/digest reuses the successful installation. The same ID with a different digest within this runtime returns `ParticipantConflict`; this initial harness does not perform participant upgrades. Cache only completed remote results. Do not assume the requested revision was accepted.

### 11.2 `install_participant<P>`

Call generated `Auth.Participants.Install` using package evidence, participant path, root digest, `expectedRevision=0` for the first installation in this fresh sandbox, and a fresh idempotency key. Keep the returned participant ID and revision. Validate that the response matches the intended participant.

A conflicting preexisting installation is an error unless it is the identical installation already recorded by this harness. Do not copy the TypeScript implementation's blind revision-increment retry. No guessed revision, silent replacement, or fabricated install success. [S23]

### 11.3 `register_service<P>`

Require `P::KIND == Service`; otherwise return `UnsupportedParticipantKind` before creating a deployment. Reserve the name under section 7.5, then:

1. Ensure installation through `install_participant`.
2. Call generated `Auth.Deployments.Create` for a new service deployment: display name is the test registration name; `kind="service"`; null/absent optional participant, portal, expiry, and device-review values; `requiresDeviceDelegation=false`; fresh idempotency key.
3. Apply this participant to that deployment through generated `Auth.Deployments.Apply`, passing exact package evidence/path/digest, expected binding revision zero, and no initial approval.
4. If the generated declared response requires consent, construct approval solely from that response: eligible capabilities and their consent digests, eligible resources and exact requested commitments, server revisions/digests, and the companion decision the server actually requests. A required ineligible item fails registration. Submit one approval attempt with a fresh idempotency key. Revision conflicts or another unsatisfied consent response fail; no open-ended retries.
5. Generate a new service identity seed/public-key pair through the SDK helper. Call generated `Auth.ServiceInstances.Provision` with the returned deployment ID, installed participant ID, public key, null instance ID, and a new idempotency key.
6. Return `TestServiceIdentity` with the private seed and server-confirmed IDs. Its connection options use the existing service bootstrap path, not the administrator connection.

Never share a deployment between two service registrations; that would let applying one participant replace the other's binding. Do not mark a service "running" merely because it was provisioned. The caller connects and starts its generated service facade and handlers. [S23]

### 11.4 `register_client<P>`

Require `P::KIND` to be `App` or `Agent`, reserve the name, and install the participant. Perform the fresh, participant-bound local-login procedure in section 10, configuring only the necessary built-in portal ceiling. Return an owned `TestClientIdentity` whose `connect_options()` borrows its session ID/seed/URL and uses the exact participant ID.

Do not provision a service deployment for an app/agent. Do not treat a device as an app. `Device` receives the explicit unsupported-kind error in this first release. No QR/device enrollment implementation, custom user-management API, raw admin RPC method, restart API, proxy API, event-capture framework, or advanced test matrix is added by this plan.

---

## 12. Process lifecycle, errors, and diagnostics

### 12.1 Independent process supervisor

Use one private standard-library supervisor thread per runtime, with message channels and owned child handles. It is independent of the caller's Tokio executor. Async methods await completion through channels; they do not block Tokio workers while waiting for operating-system children. A guard established during `start` sends a stop request if the startup future is cancelled.

Launch each transient CLI command in its own process group. Launch the long-running Trellis server in its own process group; its normal managed NATS child stays within that owned group. Do not daemonize, use shell backgrounding, or signal a process selected only by a port number. Keep the original `Child` and group identity until cleanup completes.

On Linux, apply the existing parent-death signal behavior to the directly owned subprocess and check the parent-change race before exec. For normal cleanup, use the process group to cover descendants. Do not claim that Linux parent-death signals alone establish whole-process-tree cleanup. On macOS, explicitly document that an uncatchable parent termination cannot execute Rust cleanup; normal drop/cancellation/panic and graceful shutdown are the tested guarantees.

### 12.2 Shutdown sequence

`shutdown()` first closes the harness-owned administrator connection, then asks the supervisor to stop infrastructure:

1. Check whether the owned leader exited without prematurely discarding its identity.
2. If running, signal the owned server leader with `SIGTERM`, allowing normal server-managed NATS shutdown for up to ten seconds, bounded by the configured total deadline.
3. Signal any remaining owned process group with `SIGTERM`; allow up to two more seconds within that deadline.
4. If necessary, send `SIGKILL` to the owned group, reap the leader, and finish draining/joining log readers within the remaining budget.
5. Release the port leases and clean/retain the owned directory according to retention policy.

Do not use an exited-and-reaped PID for later signaling. On supported Unix systems, use non-reaping exit observation (for example the ordinary `waitid` `WNOWAIT` facility) while retaining the leader until final group cleanup; centralize that small unsafe OS boundary in `process.rs` and document its invariants. Only the supervisor performs child waits/signals. Never run `pkill`, `killall`, broad `/proc` sweeps, or terminate a process identified by an untrusted stale PID file.

A configured shutdown timeout is the overall budget, not ten seconds plus thirty seconds plus another unbounded wait. Forced termination is reported in the result/evidence. A cleanup failure must preserve the original startup/testkit error as well as the cleanup error; it must not be hidden by a second panic.

### 12.3 Drop and cancellation

Dropping the runtime or a partially started guard sends an idempotent cleanup request. The supervisor retains child handles, leases, and directory ownership until cleanup finishes, even when the caller runtime is shutting down. Do not depend on `tokio::spawn` from `Drop` for the only cleanup path.

Explicit `shutdown().await` is the deterministic success/failure reporting path. `Drop` is best effort and cannot report success to an already-unwound test. Test drop cleanup by observing real process exit and listener release from a supervising test process. Do not promise cleanup after machine power loss or uncatchable termination on every platform.

### 12.4 Retention

`OnFailure` keeps the sandbox after startup/registration/cleanup failure or a drop during panic; clean explicit shutdown without known harness failure removes it. A caller's arbitrary assertion/result error that is never communicated to the harness cannot always be inferred. Document `Always` for investigations needing retained evidence. `Never` requests removal even on failure, but must not abandon process cleanup.

Delete only the newly created directory whose in-memory ownership ID and marker agree. Do not recursively sweep a shared temporary parent, remove other harnesses' directories, follow symlinks outside the owned tree, or adopt the TypeScript stale-directory collector as an unbounded cleanup authority. If cleanup cannot establish ownership, retain the directory and report the reason.

### 12.5 Error model

Export `TrellisTestError`, `TrellisTestErrorKind`, and `TrellisTestStage`. Use structured data with private fields and documented getters rather than exposing private generated administration types.

Required error kinds: `UnsupportedPlatform`, `MissingBinary`, `InvalidBinary`, `VersionMismatch`, `InvalidConfiguration`, `PortAllocation`, `ProcessExited`, `Timeout`, `Bootstrap`, `Authentication`, `ParticipantConflict`, `UnsupportedParticipantKind`, `DuplicateName`, `AdminRpc`, `RuntimeStopped`, `Io`, and `Cleanup`.

Required stages: validation, version check, port allocation, config generation, server start, administrator bootstrap, administrator login, participant installation, deployment creation/apply, service provisioning, client login, and shutdown. Each error includes its kind/stage; safe public IDs or path when relevant; a stable server error code when available; and a source error where it can be retained without exposing secret-bearing generated payloads. `Display`/`Debug` redact secrets. Expected failures return `Result`, not library panics.

### 12.6 Bounded output

Drain stdout/stderr immediately and independently. Store a bounded last-16-KiB tail for each stream, handle split UTF-8, and cap unfinished log-line buffering at 64 KiB so a missing newline cannot exhaust memory. Parse bootstrap output before redaction; keep its token privately. Redact passwords, identity/session seeds, portal binding secrets, and bootstrap/account-flow tokens from diagnostic tails and returned errors.

Local raw subprocess logs may contain sensitive production startup output. Keep them mode 0600 in the private sandbox; never upload the raw sandbox, databases, bootstrap bundle, or credential files as CI artifacts. CI uploads only a sanitized evidence directory, with bounded sanitized logs and no live secrets. Record log capture failures as diagnostics; they must not become a silent successful startup.

---

## 13. Concrete consumer fixture and acceptance tests

### 13.1 Fixture layout and generated contract

Create an independent, unpublished Cargo project at `integration/fixtures/testkit` with its own `[workspace]` root, `publish = false`, a checked-in manifest/lockfile, native IDL inputs, generated Rust application package, and `tests/live.rs`. Keep it out of root workspace membership/default tests. Register its generation inputs with `cargo xtask install` in the same way existing integration fixtures are generated.

The fixture is a service-repository example, not another harness implementation. Its handwritten Rust uses only `trellis-rs`, `trellis-test`, its own generated package, and ordinary registry development dependencies. Its manifest declares registry versions for Trellis packages; repository-development overrides are supplied by verification commands, never embedded as private package dependencies.

Use package name `trellis-test-fixture`, with the release-managed version, and an IDL API named `Echo` at version 1. Declare one string-valued request/response shape, one RPC action `Echo`, and one event `Observed`, both carrying a `value` string. Declare these participants:

- `Provider`: service implementing the complete Echo API.
- `Caller`: app allowed to call Echo and subscribe to Observed.
- `AgentCaller`: agent with the same caller surfaces.
- `RestrictedCaller`: app with no Echo-call permission; give it an ordinary harmless declared surface only if required by the current IDL validation rules.
- `UnsupportedDevice`: ordinary valid device participant used only to prove the harness rejects unsupported registration kinds.

Use the native generator to determine Rust identifiers; record them in the README and use those exact generated identifiers in the compiling example. Do not hand-author `ParticipantDescriptor` or RPC/event descriptors. The IDL syntax is the existing language, not a newly designed manifest. The qualified identity is generated from this package; do not use the reserved `trellis.*` namespace.

Implement the provider with generated handlers. Each RPC returns the input value, publishes `Observed` with the same value through its declared event surface, and increments a local atomic call counter for assertions. Start it using normal generated service lifecycle APIs. Subscribe before sending the RPC and await the actual event. Stop the service and its task before stopping the harness, including on assertion failure.

For `RestrictedCaller`, attempt the Echo call using the public generated descriptor/normal SDK transport under that caller's actual session; do not fabricate a raw subject. The negative test proves the call does not execute the provider handler and returns a real authorization failure. It must not pass merely because a disconnected client or intentionally malformed request failed.

### 13.2 Mandatory tests

The table is the required acceptance checklist, not a second runtime test-discovery framework. Implement ordinary Rust tests and record their names against these IDs. Pure parser/value tests use actual input records; lifecycle/auth/transport assertions use real executables and public behavior.

| ID | Required proof |
|---|---|
| T01 | Package-local unit tests and Rustdoc compile without Trellis/NATS executables and without starting subprocess infrastructure. |
| T02 | CLI/server binary precedence follows section 7.2; missing or explicitly invalid paths fail, with no source build/PATH fallback for Trellis. |
| T03 | Version parsing accepts exact matching versions, distinguishes prereleases, ignores only build metadata, and rejects malformed/failed output. At least one real executable version path is exercised. |
| T04 | A real runtime bootstraps, authenticates, registers a Provider and Caller, performs the typed RPC, and receives the real typed event. |
| T05 | An AgentCaller completes its own real participant-bound session and calls the provider. |
| T06 | Two providers registered under different names have distinct deployment/instance IDs and retain their intended bindings. |
| T07 | At least eight independent runtimes start concurrently in one Rust test process with no caller-specified ports. They have distinct HTTP/NATS/WebSocket endpoints, storage and identities; nonce-bearing typed calls/events never cross. Do not serialize this test. |
| T08 | RestrictedCaller gets a real authorization failure and the provider handler is not invoked for the denied request. |
| T09 | Duplicate names fail deterministically; a failed registration does not silently reuse its name; identical participant installation is idempotent. |
| T10 | Unsupported participant kinds fail before provisioning. No device-as-service/app fallback. |
| T11 | Startup fails early and cleanly when the real NATS executable is absent, the config is invalid, or the real server exits before readiness. No hidden test skip. |
| T12 | Automatic port assignment works across processes: run two artifact-consumer Rust test processes concurrently, each starting multiple runtimes, while a repository TypeScript `TrellisTestRuntime` smoke also starts on the same host. No invocation supplies fixed ports; all complete without cross-runtime traffic or persistent conflicts. |
| T13 | Port-conflict handling is narrowly and verifiably classified: pure tests feed the actual current server/local-NATS address-in-use diagnostics plus the selected `PortSet` and prove only matching selected ports become `PortConflict`; a direct real `trellis-server` integration case starts with one explicitly occupied managed/HTTP port and proves the production server emits the expected failure and cleans its owned resources. Harness-level repeated concurrency/stress runs then prove automatic retry/startup remains stable without exposing a port override or race hook. |
| T14 | Normal shutdown releases all four ports, terminates server/NATS, reaps the owned direct child, and is idempotent. |
| T15 | Dropping a started runtime without explicit shutdown cleans up real infrastructure. A separate observer process verifies exit/release. |
| T16 | Cancelling an in-progress `start` future after a real child is spawned does not orphan it. Observe real process/log progress; do not add a synthetic runtime failure hook. |
| T17 | Panic/unwind cleanup works even when the Tokio runtime is dropped; Linux parent-exit handling is tested separately from ordinary drop behavior. |
| T18 | Force-stopping the real server during a test returns a meaningful failure and cleanup also covers its owned NATS descendant. No PID/port-based cleanup of unrelated processes. |
| T19 | Sandbox retention obeys all three policies. A preexisting sibling marker/file survives every cleanup case. |
| T20 | Test fixture parent HOME/default login-store sentinel and relevant environment variables are byte-for-byte unchanged after concurrent startup/login/shutdown. |
| T21 | `complete_session` binds and returns valid session data without default-store writes; existing `complete` retains its persistence/admin-verification behavior. Validate the separated paths at their real auth boundary, not only by string matching source. |
| T22 | HTTP and all managed NATS listeners use loopback in the harness. Server defaults and container-facing rendering remain unchanged outside the harness. |
| T23 | Binary and sandbox paths containing spaces work. Native non-UTF-8 executable paths are preserved; unsupported non-UTF-8 config paths fail explicitly rather than being altered. |
| T24 | Error formatting/log tails are bounded and redact all captured secrets, including a bootstrap token split across input chunks; overlong unterminated lines do not grow without bound. |
| T25 | Explicit NATS `DownloadPinned` delegates to the real server's verified acquisition; default PATH mode never downloads. Run the download case as a separately named network-enabled live test. |
| T26 | Generated administration projection exactly matches generation output, removes stale destination files, and is unchanged by a second install pass. |
| T27 | All three `.crate` archives package with verification enabled; testkit normal/build/target/optional dependency closure contains no forbidden private crates. |
| T28 | Fresh artifact-only consumer, without a checkout or Deno/Node/codegen, compiles and runs the real RPC/event and concurrent-runtime smoke against extracted public artifacts. |
| T29 | At least native Linux and native macOS live smoke pass. The existing four release targets build/package; record the exact architecture actually exercised, not just an OS label. |
| T30 | Release preparation for a prerelease rewrites all new package/fixture/generated version references, then produces mutually matching archives/binaries. No retained hard-coded base version. |
| T31 | Preexisting Rust CLI managed/external-NATS tests, TypeScript live tests, generated-current checks, and required repository Check jobs remain green. |

T13 does **not** require a deterministic test-only race injector. The production retry loop must exist exactly as section 8.4 specifies, but verification is split across (a) pure classification/state-machine tests using the exact diagnostics emitted by a real occupied-port server run, (b) that direct real server occupied-port case, and (c) repeated concurrent harness startup in T07/T12. This avoids adding a forbidden test-only runtime hook just to seize a port in the few instructions between reservation release and `exec`.

For T20, run a test executable in a temporary parent profile containing a recognizable existing session-file sentinel; let the harness create a different child profile. Inspect the sentinel from the observer, not through global environment mutations while other tests run. The same observer can verify process cleanup after that test executable unwinds.

For T21, reuse the existing real auth integration setup for admin-only persistence checks and add the new storage-free path there. A non-admin bound session should be accepted by `complete_session`; use ordinary user creation/account-flow setup through the generated admin API when proving that distinction. Never ignore an error from `complete` and then read a saved session, as that would perpetuate the isolation issue rather than test the new path.

No acceptance test may silently return early because infrastructure, binaries, network access for an explicitly selected download test, or an OS prerequisite is missing. Mark it failed or explicitly not run in the ledger. Unsupported-platform unit cases are distinct from claiming live platform coverage.

---

## 14. Packaging and artifact-only consumer verification

### 14.1 Producer phase

After generation and normal executable builds, package the public dependency set together with Cargo verification enabled:

```sh
cargo package --manifest-path Cargo.toml \
  -p trellis-protocol -p trellis-rs -p trellis-test
```

Use `--allow-dirty` only in the prepared-release job where release tooling deliberately rewrites versions, following the existing workflow. Do not use it to hide uncommitted implementation work in final review evidence. Multi-package packaging is the prepublication verification mechanism; do not require unpublished versions already to exist on crates.io. [S13]

Create a verification bundle containing only:

```text
packages/                 the three public .crate archives
binaries/                 matching trellis + trellis-server release/native archive
nats/                     explicitly supplied verified NATS executable/archive
consumer/                 fixture manifest, lockfile, own generated package, tests
verify-rust-test-package.sh
artifact-manifest.json    source SHA, versions, target, file SHA-256 hashes
```

No Trellis checkout, private crate source tree, Cargo workspace configuration, browser source, node_modules, or target build cache enters this bundle. Generated fixture/application source is allowed because it is exactly what an outside repository normally owns. Package-local generated admin source is already inside `trellis-test.crate`.

Validate archive paths during extraction: reject traversal, absolute member paths, and unsafe link targets. Never use a bundle to overwrite a user-selected directory. Generate staging directories atomically under a new temporary root.

### 14.2 `verify-rust-test-package.sh` interface

Implement a portable Bash script at the specified path, with `set -euo pipefail`, explicit arguments, cleanup traps, and no source-checkout assumptions:

```text
verify-rust-test-package.sh --bundle ABSOLUTE_BUNDLE_DIR --mode smoke
verify-rust-test-package.sh --bundle ABSOLUTE_BUNDLE_DIR --mode full
```

`smoke` runs the packaged consumer's typed RPC/event, parallel-runtime, and explicit-shutdown cases. `full` runs the fixture's full non-download live target. Both verify metadata/hashes and dependencies before testing. The separately selected pinned-download test remains in the normal live workflow, not an accidental network action during package compilation.

The script requires Cargo/Rust, Bash, and Python 3 for archive/manifest validation; the **published Rust crate** does not require Bash/Python. It must not require Deno, Node, the Trellis code generator, Git credentials, Docker, or Podman. The producer owns generation and binary acquisition before creating the bundle.

### 14.3 Prepublication source resolution

In a fresh consumer directory outside every Trellis workspace, extract the three unmodified public `.crate` archives and the fixture. Use a temporary Cargo config with `[patch.crates-io]` entries for **only these three extracted public artifacts**. Do not patch any package to the producer checkout, runtime source, or generated private Cargo crate.

This is an intentional prepublication test of the exact distributable sources, not a recommendation that users carry patches. The consumer fixture's own generated application package may be a local dependency, as in ordinary service repositories. The packaged testkit itself must not acquire any repository-relative dependency.

Use an isolated `CARGO_HOME` and `CARGO_TARGET_DIR`. Preserve access to the actual installed Rust toolchain, but do not import the producer's Cargo config or target cache. Resolve/generate the consumer lockfile for those staged sources, then run compilation/tests with `--locked`. Registry downloads for ordinary Rust dependencies are allowed during preparation; runtime test startup must not acquire Trellis source or tools.

Run Cargo metadata and inspect the graph as structured JSON. Every non-registry manifest must resolve under the three extracted public artifact roots or the consumer's own application/generated-fixture roots. Traverse normal/build and every target/optional declaration, not only dependencies enabled on the host. Fail on a forbidden package, hidden path/git dependency, private registry requirement, or source root outside that allowlist.

### 14.4 What this gate proves

Cargo normalizes the manifest, removes workspace/patch settings, and rebuilds the extracted package during packaging. That does not by itself exercise runtime executable discovery or account isolation. The additional no-checkout consumer proves the distributed library can actually run a test. [S13]

The consumer must compile a concrete documented test using the exact public APIs in this plan, start the matching distributed binaries, provision identities, call a generated method, observe a generated event, run two isolated runtimes simultaneously, and explicitly shut them down. Capture real command exit codes, artifact hashes, resolved dependency paths, and sanitized logs.

### 14.5 Actual registry check after publication

Add a documented maintainer command/procedure that repeats the fixture smoke using actual crates.io versions and **no patch entries**, after the three packages have been published and indexed. Do not run it against a nonexistent version or call the prepublication artifact test a successful crates.io install. This post-publication check is not authorization for the implementation agent to publish.

---

## 15. CI, release, and version integration

### 15.1 Preserve existing CI ownership

Keep the normal `Check` workflow responsible for correctness, including the existing Rust/TypeScript suites. The refreshed baseline deliberately removed `--test-threads=1` from the Rust live integration command after making local runtime tests parallel-safe. **Do not reintroduce global Rust test serialization.** Preserve the existing dynamic-port CLI integration behavior and add the new testkit concurrency cases under normal parallel test execution. [S2, S11]

Extend Check's generation/install path to produce and verify the testkit projection and new fixture artifacts. Ensure artifact transfer includes both before dependent builds start. Track generated output consistently; a missing generated tree must fail the generated-current job rather than be recreated invisibly by a consumer build script.

Add full native Linux testkit live verification to Check, including the isolated packaged-consumer producer/consumer split. Add native macOS smoke using the same real public workflow and explicit supported NATS binary. Use existing release targets rather than claiming unsupported hosts work.

### 15.2 Mandatory no-checkout job

Create a new artifact-only consumer job, `rust-testkit-consumer`, on a fresh hosted runner. **Do not add `actions/checkout` to this job.** It downloads only the verification bundle, installs the ordinary Rust toolchain, and executes the bundle's script. No dependency on a self-hosted runner's existing work directory, a private source mount, or ambient cached project configuration.

Check needs the full consumer target plus the other standard correctness jobs. Release needs the smoke target described next. A failed or unexpectedly skipped required consumer job fails its gate. Keep diagnostics bounded and sanitized; never upload the raw runtime sandbox.

### 15.3 Release packaging and archive smoke

In `.github/workflows/release.yml`:

1. Extend `package-rust`'s existing verified Cargo package command to include `trellis-test`, and upload the three `.crate` artifacts for consumer verification.
2. Ensure prepared-release artifacts contain the freshly generated administration projection and new fixture generation output.
3. For a tagged release, build the verification bundle from the **same CLI/server archive produced by the release build job**, not a separately rebuilt debug binary. On Linux smoke, select the matching Linux archive. Check its recorded hash and versions against the prepared release.
4. For a tagless release-verification run where cross-platform archive jobs are intentionally skipped, have the producer build matching native CLI/server executables from the prepared workspace and bundle those. The smoke itself still runs; do not silently drop the package-consumer gate because no release tag exists.
5. Add the artifact-only smoke result to `release-gate` alongside existing package/image requirements, with explicit tagged/tagless dependency conditions. Avoid a circular dependency: the producer/consumer smoke depends on preparation/packaging/binaries, not on `release-gate` or publication.
6. Preserve the recent OIDC, npm repository-URL, and Docker/Podman smoke fixes on `main`. Do not replace the release workflow with an older copied version.

Full behavioral tests belong to Check; the release smoke proves packaged artifacts and publication inputs. No crate is uploaded before the relevant verification gate succeeds.

### 15.4 Publication order and policy

Update the facade's package-publication policy test to intentionally exempt exactly these directories: `trellis`, `protocol`, and `trellis-test`. Every other internal crate remains non-publishable. Keep the test rather than deleting it. [S10]

Extend the existing release upload sequence to:

```text
publish trellis-protocol -> wait for index availability
publish trellis-rs       -> wait for index availability
publish trellis-test    -> wait for index availability
```

Reuse the existing token/OIDC mechanism, already-published checks, dry-run behavior, and registry-index wait helper. Do not invent a credentials workflow, store tokens in the repository, publish private dependencies, or alter unrelated JavaScript publication jobs.

Before the first upload, a maintainer must confirm that the `trellis-test` name is available/owned by the intended publisher and that the release identity may publish it. Record that as publication preflight. If the name is unavailable or rights are missing, mark publication blocked; **do not select a new package name, publish an empty name-reservation crate, or obtain unrelated credentials** without a coordinator/maintainer decision. Code-review completion and publication authorization are distinct.

### 15.5 Version discipline

The workspace's release-managed version is the source of truth. Extend `xtask`'s existing collection, rewriting, and tests for new version-bearing fixture manifests and documentation generation where applicable. Generated admin code must reflect the same prepared release as the runtime archive. [S24]

Keep the normal registry requirement style used by the existing generated SDK; do not introduce a separate testkit release numbering scheme. Document exact binary matching and show an exact pinned testkit version in reproducible CI examples. Do not require older installed binaries to support the new flags.

Add release-preparation tests for at least a stable version and a prerelease. They must demonstrate projection after regeneration, rewritten fixture/public requirements, and consistent reported binary/package versions. These are local verification operations in a disposable preparation directory/worktree, not a publishing workflow invocation.

---

## 16. Documentation changes authorized by this plan

The following exact documentation changes are part of the selected design. Treat this section as the proposal/authorization for these task-specific deltas; other design/documentation drift should be reported rather than silently rewritten. Follow repository formatting and documentation-generation practices. [S25]

| Location | Required delta |
|---|---|
| `crates/trellis-test/README.md` | Purpose, public API example, exact binary requirements, NATS selection, ordinary Cargo test use, lifecycle/retention, supported platforms, security/isolation limits, no source-checkout requirement. |
| Public Rustdoc | Document every exported type, method, error kind, default, ownership rule, and possible failure. Examples compile without launching infrastructure during documentation generation. |
| `RUST.md` | List `trellis-test` as the intentional public test package; identify the actual facade package as `trellis-rs`; replace the implication that all other directories must be private with the exact public package set. No broad unrelated historical cleanup. |
| `docs/src/routes/guides/testing-trellis-services/+page.svx` | Add a Rust section with the concrete fixture-based test, setup of matching binaries, explicit shutdown, and no private dependency patches for released usage. Preserve the TypeScript guide. |
| `docs/src/routes/guides/libraries/rust/+page.svx` | Link to Rust live-testing guidance and generated Rustdoc. Do not document internal bootstrap APIs as the normal authoring surface. |
| `docs/src/routes/guides/releasing-trellis/+page.svx` | Add third-package ordering, generated projection, artifact consumer gate, name/ownership preflight, and postpublication registry smoke. |
| `.github/workflows/pages.yml`, `docs/src/lib/docs.ts` | Publish/discover testkit Rustdoc through the existing mechanism. Verify the actual generated link; do not leave a falsely advertised pending or broken API surface. |
| `docs/static/llms.txt`, `llms-full.txt`, `llms-rust.txt`, and relevant cross-language guidance in `llms-typescript.txt` | Keep the documented external Rust testing workflow and package set consistent; no instruction to compile private runtime crates from a service repo. |
| `design/core/platform-libraries.md`, `design/core/testing-patterns.md` | Task-specific invariant delta only: third public Rust package; process-based test ownership; real public boundaries; native tests; internal runtime crates remain unpublished. Exact API examples belong in guides/Rustdoc. |
| `CHANGELOG.md` | Record the publishable Rust live-test harness, HTTP bind-address config, and non-persisting session completion. Preserve the refreshed RC note that selectable managed-NATS ports already exist for parallel local instances; do not describe that current flag as newly added by this work and do not claim publication has already occurred. |

Use a **compiling** example based on the actual generated fixture names. Its lifecycle must clean up services/tasks as well as the harness on an error path; a happy-path-only snippet with `?` leaving infrastructure running is not enough. A short wrapper function returning `Result` plus an outer explicit cleanup block is acceptable; do not add a public custom test-runner API merely to simplify the example.

Document that this initial `register_client` uses the isolated administrator as its human principal, that device/advanced TS-helper parity is not claimed, that production auth/authorization remains real, and that no parent-profile storage is touched.

Reference the supplied UI attachment only as stated in section 0.1. No UI code, colors, layout decisions, or screenshot claims are part of these documentation changes.

---

## 17. Implementation sequence and exit gates

Complete these work packages in order. Do not treat an earlier successful package build as permission to omit later external-consumer or review gates.

| Work package | Required implementation | Exit evidence |
|---|---|---|
| W0 — baseline | Worktree from current `origin/main`, actual base SHA, task plan/evidence files, narrow source-drift inventory including DESIGN BLOCKER 1. | Correct current repository/main base; no fallback to `dfaa557e`; unrelated work unchanged; blockers enumerated. |
| W1 — server isolation | Preserve existing combined managed-NATS port option; add default-preserving HTTP bind address; safely quote NATS paths; update only relevant parsing/config tests. | Existing NATS-port defaults/CLI stay unchanged; testkit can start parallel real servers with automatically selected disjoint loopback endpoints. |
| W2 — session boundary | Add `complete_session`; preserve old completion behavior; new real session-isolation tests. | New path has no default-store side effects; old admin-only behavior remains verified. |
| W3 — distributable source | Testkit manifest/dependencies, generated projection/mount, publication allowlist, removal of obsolete facade process-helper dependency. | Generated source compiles; no private dependency in the testkit closure; installation is repeatable. |
| W4 — runtime ownership | Builder, binary validation, directory/port leases, supervisor, log redaction, startup, shutdown/drop. | Real startup and cleanup cases pass, including cancellation and parallel allocation. |
| W5 — public workflow | Typed admin operations, install/register service/register client, redacted identity types and existing-SDK options. | Real provider/caller/agent workflows and denial/isolation cases pass. |
| W6 — consumer | Fixture IDL/generation, complete example, producer bundle, artifact-only script. | All three archives verified; full live consumer succeeds without private-source paths. |
| W7 — automation/docs | Check/release gates, macOS smoke, version handling, Rustdoc/docs/llms/changelog. | Required jobs and packaged smoke pass; concrete documentation example is tested. |
| W8 — review handoff | Final rebase check, all evidence, clean final feature commit, PR, no merge. | Coordinator can review exact diff and reproduce key tests from the handoff alone. |

Do not split W3 into publishing runtime/bootstrap packages. Do not simplify W4 by assuming tests always finish normally. Do not simplify W6 by pointing Cargo at the original facade directory in the isolated gate. Do not convert W7 into a publishing run to demonstrate that upload permissions work.

If a required external service is unavailable, finish and verify the independent work, then identify the unexecuted gate accurately. The change is not merge-ready while required verification remains missing.

---

## 18. Required verification commands and execution discipline

### 18.1 Repository preparation and pure checks

Run the repository's actual generation/build preparation before commands that need generated or embedded artifacts. At the reviewed baseline that includes `cargo xtask install`, protocol WASM, and embedded browser app generation as used by Check. Do not check in empty embedded files or disable a build check to bypass that preparation.

Use at least the following commands from the feature worktree, recording tool versions, full command, exit status, and final commit association:

```sh
rustc --version
cargo --version
deno --version

git status --short
cargo xtask install
cargo xtask install
cargo fmt --manifest-path Cargo.toml --all --check
cargo clippy --manifest-path Cargo.toml --workspace --all-targets -- -D warnings
cargo test --manifest-path Cargo.toml --workspace
cargo doc --manifest-path Cargo.toml -p trellis-test --no-deps
cargo run --manifest-path xtask/Cargo.toml -- release check-versions
```

Format changed Markdown/JSON/TypeScript/docs using `deno fmt -c ts/deno.json <changed-files>` before running their checks. Format Rust with the normal Cargo formatter. Do not bulk-format unrelated files. Compare generated-source hashes before/after the second install; a clean final worktree must follow the committed generated result.

Perform release preparation/version-rewrite tests in a disposable directory/worktree using the existing xtask tests/commands. Do not rewrite the integration worktree to a speculative next version merely to create a verification claim. Follow current release-guide non-publishing verification commands for metadata checks; record any required context arguments explicitly.

### 18.2 Native development live commands

Build the real binaries, then select explicit executable paths:

```sh
cargo build --manifest-path Cargo.toml -p trellis-cli -p trellis-server
export TRELLIS_TEST_CLI_BIN="$PWD/target/debug/trellis"
export TRELLIS_TEST_SERVER_BIN="$PWD/target/debug/trellis-server"
```

Set `TRELLIS_TEST_NATS_BIN` to a real, validated available NATS executable, or use the PATH mode deliberately. Resolve its absolute path and record its version/hash. Do not silently install a latest version. Explicit pinned acquisition is the separately named network test.

For the new independent fixture during repository development, use the following public-only source overrides (the fixture's checked-in manifest remains registry-oriented):

```sh
cargo test --manifest-path integration/fixtures/testkit/Cargo.toml \
  --config "patch.crates-io.trellis-protocol.path=\"$PWD/crates/protocol\"" \
  --config "patch.crates-io.trellis-rs.path=\"$PWD/crates/trellis\"" \
  --config "patch.crates-io.trellis-test.path=\"$PWD/crates/trellis-test\"" \
  --test live -- --nocapture
```

This command is **development verification only** and is not the artifact-only gate. Use a supported shell quoting strategy if the checkout contains quotes; do not split a path into arguments. The script implementation should write a TOML config file with proper quoting rather than interpolate untrusted strings. Commit a current fixture lockfile once resolution stabilizes and use `--locked` for final repeat runs.

Keep the explicitly network-enabled pinned-download case in `integration/fixtures/testkit/tests/download.rs`, its own `download` test target in the fixture, invoked separately by Check after network prerequisites are available. The `live` target covers non-download behavior. This is explicit test selection, not an ignored or silently skipped test.

Run the existing facade CLI live target and TypeScript suite unchanged in purpose:

```sh
cargo test --manifest-path Cargo.toml -p trellis-rs \
  --features live-integration --test integration -- --nocapture

deno test -A -c ts/integration/deno.json ts/integration
```

The refreshed baseline intentionally runs the existing Rust live integration without `--test-threads=1`. Keep it that way. The new fixture must demonstrate simultaneous runtimes inside a single test and across independent processes, all with automatic port selection.

### 18.3 Packaging and external execution

```sh
cargo package --manifest-path Cargo.toml \
  -p trellis-protocol -p trellis-rs -p trellis-test

bash scripts/verify-rust-test-package.sh --bundle "$BUNDLE" --mode full
```

`BUNDLE` is the actual producer output described in section 14, not the source worktree. The same consumer command must run in the no-checkout CI job. A local producer-run consumer alone does not prove the separate job's isolation.

Record `cargo metadata --format-version 1` and the manifest allowlist audit from the staged consumer. Inspect the normalized `.crate` manifests and source lists directly; do not rely on a text search of the workspace's Cargo.toml alone. Check that the new private generated source actually appears in the archive. Package inclusion is an explicit contract. [S26]

### 18.4 Selection and result integrity

All named acceptance tests must be accounted for. A zero-exit test filter that selected zero cases does not count as a pass. Record test counts. Explain intentionally separate platform/network selections. Do not translate "could not run" into "passed by inspection," and do not report all-platform support based only on cross-compilation.

Any feature-branch rebase or review-fix commit invalidates earlier final-tip evidence for affected code. Rerun the relevant commands and required CI, then replace the final ledger entries with their actual tested SHA. No merge based on an earlier green commit.

---

## 19. Definition of done

The implementation is ready for coordinating review only when all of the following are true:

- The new public package implements the specified service and app/agent workflow, without normal/build/optional/target dependencies on private workspace crates.
- It starts normal distributed CLI/server executables, uses real public bootstrap/auth/consent/provisioning boundaries, and leaves the external service's generated APIs in control of connections and handlers.
- Package-local generated administration source is complete, deterministic, included in `.crate`, and regenerated after release-version preparation.
- Default server behavior is unchanged; harness listeners, profiles, ports, and credentials are independently owned and loopback-isolated.
- Caller environment/default login storage is untouched; graceful shutdown, drop, cancellation, and documented platform limits are covered by real lifecycle tests.
- Every T01–T31 requirement has actual evidence or an explicitly reported blocker. No required missing gate is represented as complete.
- The artifact-only consumer has no checkout/private-source dependency, and actual release archive versions match the tested testkit.
- Existing required Check jobs and non-publishing release verification are green on the submitted final commit; generated output and fixture examples are current.
- Public Rustdoc and external-user documentation match implemented APIs and accurately state scope/limitations.
- The feature worktree is clean, a reviewable PR targets `main`, and the final handoff contains exact SHAs, test logs, artifact hashes, and known limitations.
- Nothing has been merged or published by the implementation agent before the coordinating approval gate.

This is a readiness checklist, not automatic approval. The coordinator independently reviews the diff and evidence.

---

## 20. Design blockers and prohibited substitutions

Report a design blocker when source drift or a discovered constraint conflicts with the locked architecture. Stop only the affected workstream, retain completed work, and continue independently verifiable tasks. Do not replace the requested solution with a technically convenient alternative.

Use this exact handoff structure:

```text
DESIGN BLOCKER <number>
Requirement: <section and decision/test ID>
Actual source: <commit, file, symbol, and relevant behavior>
Observed failure: <command/error or conflicting invariant>
Impact: <specific work that cannot satisfy the prescribed contract>
Completed independent work: <commits/tests>
Decision needed from coordinator: <one precise question>
No workaround implemented: <confirm no boundary/scope substitution>
```

Examples of changes that need approval: another public crate name; publishing bootstrap/runtime support; relaxing version matching; altering public identity types; adding arbitrary remote-runtime attachment; inserting auth/database bypasses; introducing a test-only server mode; shipping handwritten RPC schemas; changing generated-code ownership; dropping macOS support or a required lifecycle test; changing the approved merge procedure.

Examples that are ordinary implementation work: a moved source symbol with unchanged semantics, a non-colliding worktree name, a private helper being inlined to match repository style, or correcting a generated identifier in the example to the compiler's actual emitted spelling. Record source mappings, but do not ask the user to restate requirements already fixed here.

---

## 21. Evidence ledger and agent handoff format

Create and maintain `trellis-test-public-evidence.md`. Fill the following template with actual values; the placeholders below are a template, not permission to hand over missing evidence as a completed implementation.

```markdown
# Public Rust trellis-test evidence

## Identity
- Repository:
- Plan version / plan commit:
- Plan baseline: 27f79d192204de9de78645b1006ceeccb59bf932
- Actual implementation base SHA:
- Current origin/main SHA:
- Final implementation/tested SHA:
- Branch and worktree path:
- PR URL:
- Clean status output:
- Compiler/Cargo/Deno versions:

## Baseline drift
- Changed source anchors and mechanical mappings:
- Preserved unrelated release/CI changes:
- Coordinator-approved plan amendments, or none:

## Dependency and package proof
- Three crate names/versions/archive SHA-256 hashes:
- Normalized manifests and source lists:
- Public-only closure audit output:
- Generated projection path/content-hash comparison:
- Second-install drift result:
- Consumer non-registry source-root audit:

## Live runtime evidence
- CLI/server/NATS paths, versions, hashes, and build/source SHA:
- Linux runner/architecture and actual tested cases:
- macOS runner/architecture and actual tested cases:
- Concurrent-runtime isolation evidence:
- Parent-profile/session sentinel result:
- Explicit shutdown/drop/cancellation/panic evidence:
- Retention and sanitized-log results:
- Network download case result:

## Acceptance matrix
| Requirement | Actual test name | Command or CI job | Tested SHA | Result | Evidence |
|---|---|---|---|---|---|
| T01 ... T31 | | | | | |

## CI and release verification
- Required Check run/job links and status:
- Artifact-only no-checkout consumer job link:
- Tagged/tagless package-smoke results:
- Stable/prerelease version-preparation tests:
- Publishing workflow not invoked:
- Registry-name/ownership preflight: confirmed or not yet verified by maintainer:

## Remaining issues
- Test failures:
- Not-run tests with exact reason:
- Design blockers:
- Known platform/cleanup limits:
- Other preexisting unrelated failures:

## Review state
- State: READY FOR COORDINATOR REVIEW / BLOCKED
- Coordinator approval: NOT GRANTED
- Approved head SHA: none
- Approved main/base SHA: none
- Merge performed: no
- Publication performed: no
```

Store sanitized command output under a dedicated evidence artifact directory, not the runtime sandbox. Record relative artifact filenames and hashes in the ledger. Remove credentials/tokens before upload. Do not make source claims from screenshots when the source/diff/log exists.

Because committing the evidence file itself changes HEAD, identify the tested implementation SHA accurately and distinguish any subsequent evidence-only commit. Required CI must still run on the exact PR head submitted for approval. Do not fabricate a self-referential "final SHA" inside the very commit that creates it; use the PR/CI handoff for the final head association.

At handoff, send the coordinator the PR, branch/head/base, plan/evidence locations, significant API/config changes, actual verification summary, and unresolved items. State clearly that nothing has been merged. Attach the user-supplied UI reference only if it was actually provided, labeled under section 0.1; do not substitute an unrelated image.

---

## 22. Coordinating review, corrections, and merge gate

### 22.1 Independent review pass

The coordinator reviews the actual final diff and evidence, not just the agent summary. The required review sequence is:

1. Confirm repository, feature head, main/base, worktree cleanliness, and preservation of recent unrelated changes.
2. Inspect public API and dependency closure against A01–A22. Check all manifest sections, not only default-enabled dependencies.
3. Inspect generated-source provenance/inclusion and normal SDK connection use. Reject handwritten evidence/subjects or private type leakage.
4. Inspect default-preserving server changes, port allocation/lock lifetime, user-environment isolation, and absence of default session-store access.
5. Inspect real bootstrap/consent/provisioning paths, caller participant binding, independent service deployments, error preservation, and secret redaction.
6. Inspect the supervisor's cancellation/drop/exit/reaping ownership, process-group safety, and actual lifecycle tests.
7. Inspect the isolated consumer job: no checkout, unmodified extracted artifacts, allowed source roots only, real distributed binaries, nonzero test counts.
8. Inspect release version ordering, tagged/tagless smoke gates, publication safeguards, supported-platform evidence, and truthful docs.
9. Classify findings as must-fix or informational. Missing required verification is must-fix, not presumed safe because a local build worked.

The implementation agent corrects review findings on the same feature worktree/branch, updates the ledger, reruns affected checks, and resubmits. It does not waive findings, redefine acceptance, or approve its own work. A proposed design change remains a coordinator decision.

### 22.2 Exact approval

Approval must be explicit and identify both the feature head and the verified integration base, for example:

```text
APPROVED FOR MERGE
Repository: oats-center/trellis
PR: <actual PR>
Head: <40-character reviewed feature SHA>
Main/base: <40-character verified main SHA>
Required checks: <actual successful run identifiers>
Publication: not authorized by this merge approval
```

Silence, "looks promising," a green check, agent self-review, or earlier approval of the plan is not merge approval. The user approving this architecture authorizes implementation, not unreviewed integration.

### 22.3 Integration after approval

After that approval, the authorized merge actor must freshly verify:

- PR head still equals the approved head.
- `origin/main` still equals the approved base.
- Required checks apply to that head/base or the corresponding verified integration candidate.
- No review fixes, generated changes, or release-preparation edits have been added since approval.

If either head or base moved, update/rebase the feature branch in its worktree, rerun relevant verification, and request renewed approval for the new pair. Do not force-push main or merge a stale approval. A rebase is not a license to absorb incompatible design changes silently.

Use the repository's normal PR merge mechanism, with the approved head pinned where that mechanism supports it, and target `main`. Do not enable auto-merge beforehand. If repository permissions require a maintainer to perform the final merge, leave the approved PR ready for that maintainer rather than seeking credentials or using a workaround.

After the authorized merge, record the actual merge/integrated commit and verify the approved change is present on main and required post-merge checks run successfully. Keep the worktree until the integration record is captured. Delete it only after confirming it is clean and the owner permits cleanup. Publication remains a separate release-maintainer action with its own package-name/permission preflight.

---

## 23. Sources and mandatory reading anchors

Repository sources below are pinned to the reviewed baseline. These links establish the as-built starting point; they are not claims that the prescribed new APIs already exist. Read the narrow relevant portions and follow current symbols when preparing the implementation. Do not load unrelated design subtrees by default.

**[S1] Current private embedded Rust testkit to be replaced by this plan.** [crates/trellis-test/Cargo.toml](https://github.com/oats-center/trellis/blob/27f79d192204de9de78645b1006ceeccb59bf932/crates/trellis-test/Cargo.toml); [crates/trellis-test/src/lib.rs](https://github.com/oats-center/trellis/blob/27f79d192204de9de78645b1006ceeccb59bf932/crates/trellis-test/src/lib.rs).

**[S2] Repository-local Rust test infrastructure and refreshed dynamic-port managed-NATS coverage.** [crates/trellis/tests/integration/cli.rs](https://github.com/oats-center/trellis/blob/27f79d192204de9de78645b1006ceeccb59bf932/crates/trellis/tests/integration/cli.rs); [crates/trellis/Cargo.toml](https://github.com/oats-center/trellis/blob/27f79d192204de9de78645b1006ceeccb59bf932/crates/trellis/Cargo.toml).

**[S3] Private implementation dependency chain.** [crates/local-nats/Cargo.toml](https://github.com/oats-center/trellis/blob/27f79d192204de9de78645b1006ceeccb59bf932/crates/local-nats/Cargo.toml); [crates/bootstrap/Cargo.toml](https://github.com/oats-center/trellis/blob/27f79d192204de9de78645b1006ceeccb59bf932/crates/bootstrap/Cargo.toml); [crates/runtime/Cargo.toml](https://github.com/oats-center/trellis/blob/27f79d192204de9de78645b1006ceeccb59bf932/crates/runtime/Cargo.toml); [crates/runtime-apis/Cargo.toml](https://github.com/oats-center/trellis/blob/27f79d192204de9de78645b1006ceeccb59bf932/crates/runtime-apis/Cargo.toml).

**[S4] CLI bootstrap and its JSON output.** [crates/cli/src/app/bootstrap.rs](https://github.com/oats-center/trellis/blob/27f79d192204de9de78645b1006ceeccb59bf932/crates/cli/src/app/bootstrap.rs).

**[S5] Existing release packaging, archives, and ordered publication.** [.github/workflows/release.yml](https://github.com/oats-center/trellis/blob/27f79d192204de9de78645b1006ceeccb59bf932/.github/workflows/release.yml).

**[S6] Server CLI and startup/NATS policy, including the existing combined `--local-nats-ports` option.** [crates/server/src/main.rs](https://github.com/oats-center/trellis/blob/27f79d192204de9de78645b1006ceeccb59bf932/crates/server/src/main.rs). The refreshed behavior is also recorded in [CHANGELOG.md](https://github.com/oats-center/trellis/blob/27f79d192204de9de78645b1006ceeccb59bf932/CHANGELOG.md).

**[S7] Listener allocation, host-local NATS rendering, and HTTP binding.** [crates/local-nats/src/lib.rs](https://github.com/oats-center/trellis/blob/27f79d192204de9de78645b1006ceeccb59bf932/crates/local-nats/src/lib.rs); [crates/bootstrap/src/nats_config.rs](https://github.com/oats-center/trellis/blob/27f79d192204de9de78645b1006ceeccb59bf932/crates/bootstrap/src/nats_config.rs); [crates/runtime/src/server.rs](https://github.com/oats-center/trellis/blob/27f79d192204de9de78645b1006ceeccb59bf932/crates/runtime/src/server.rs); [crates/runtime/src/config/mod.rs](https://github.com/oats-center/trellis/blob/27f79d192204de9de78645b1006ceeccb59bf932/crates/runtime/src/config/mod.rs).

**[S8] Existing login completion, signed binding, and persistence.** [crates/trellis/src/auth/browser_login.rs](https://github.com/oats-center/trellis/blob/27f79d192204de9de78645b1006ceeccb59bf932/crates/trellis/src/auth/browser_login.rs); [crates/trellis/src/auth/models.rs](https://github.com/oats-center/trellis/blob/27f79d192204de9de78645b1006ceeccb59bf932/crates/trellis/src/auth/models.rs); [crates/trellis/src/auth/session_store.rs](https://github.com/oats-center/trellis/blob/27f79d192204de9de78645b1006ceeccb59bf932/crates/trellis/src/auth/session_store.rs).

**[S9] Canonical generated SDK root and dependency surface.** [crates/runtime-apis/src/lib.rs](https://github.com/oats-center/trellis/blob/27f79d192204de9de78645b1006ceeccb59bf932/crates/runtime-apis/src/lib.rs); [crates/runtime-apis/Cargo.toml](https://github.com/oats-center/trellis/blob/27f79d192204de9de78645b1006ceeccb59bf932/crates/runtime-apis/Cargo.toml).

**[S10] Public-crate policy test.** [crates/trellis/src/lib.rs](https://github.com/oats-center/trellis/blob/27f79d192204de9de78645b1006ceeccb59bf932/crates/trellis/src/lib.rs).

**[S11] Check workflow and test ownership policy, including parallel Rust live execution.** [.github/workflows/check.yml](https://github.com/oats-center/trellis/blob/27f79d192204de9de78645b1006ceeccb59bf932/.github/workflows/check.yml); [AGENTS.md](https://github.com/oats-center/trellis/blob/27f79d192204de9de78645b1006ceeccb59bf932/AGENTS.md).

**[S12] Cargo dependency rules.** [Specifying Dependencies](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html). Consult path-plus-version and development-dependency rules; do not confuse a consumer dev-dependency with the harness's implementation dependencies.

**[S13] Cargo package verification.** [cargo package](https://doc.rust-lang.org/cargo/commands/cargo-package.html). Consult normalization, verified extraction/build, package selection, and registry behavior for interdependent packages.

**[S14] Trusted built-in participant ownership.** [crates/runtime/src/platform/auth/builtins.rs](https://github.com/oats-center/trellis/blob/27f79d192204de9de78645b1006ceeccb59bf932/crates/runtime/src/platform/auth/builtins.rs).

**[S15] Existing SDK connection options and generated client boundary.** [crates/trellis/src/service/runtime_facade.rs](https://github.com/oats-center/trellis/blob/27f79d192204de9de78645b1006ceeccb59bf932/crates/trellis/src/service/runtime_facade.rs); [crates/trellis/src/client/connection.rs](https://github.com/oats-center/trellis/blob/27f79d192204de9de78645b1006ceeccb59bf932/crates/trellis/src/client/connection.rs); [crates/trellis/src/generated.rs](https://github.com/oats-center/trellis/blob/27f79d192204de9de78645b1006ceeccb59bf932/crates/trellis/src/generated.rs).

**[S16] Generated runtime config, local identity, rate defaults, and refreshed lease defaults.** [crates/bootstrap/src/runtime_config.rs](https://github.com/oats-center/trellis/blob/27f79d192204de9de78645b1006ceeccb59bf932/crates/bootstrap/src/runtime_config.rs).

**[S17] TypeScript kernel-assigned loopback reservation and bounded bind-race retry pattern.** [ts/packages/trellis-test/src/control_plane_config.ts](https://github.com/oats-center/trellis/blob/27f79d192204de9de78645b1006ceeccb59bf932/ts/packages/trellis-test/src/control_plane_config.ts).

**[S18] CLI JSON version implementation.** [crates/cli/src/app/runtime.rs](https://github.com/oats-center/trellis/blob/27f79d192204de9de78645b1006ceeccb59bf932/crates/cli/src/app/runtime.rs).

**[S19] Existing process output/bootstrap parsing.** [ts/packages/trellis-test/src/trellis_process.ts](https://github.com/oats-center/trellis/blob/27f79d192204de9de78645b1006ceeccb59bf932/ts/packages/trellis-test/src/trellis_process.ts).

**[S20] Real first-admin and portal-consent automation.** [ts/packages/trellis-test/src/admin_client.ts](https://github.com/oats-center/trellis/blob/27f79d192204de9de78645b1006ceeccb59bf932/ts/packages/trellis-test/src/admin_client.ts).

**[S21] Existing local login, portal binding, and consent HTTP protocol.** [ts/packages/trellis-test/src/admin/auth_flow.ts](https://github.com/oats-center/trellis/blob/27f79d192204de9de78645b1006ceeccb59bf932/ts/packages/trellis-test/src/admin/auth_flow.ts); [ts/packages/trellis/auth/browser/portal.ts](https://github.com/oats-center/trellis/blob/27f79d192204de9de78645b1006ceeccb59bf932/ts/packages/trellis/auth/browser/portal.ts).

**[S22] Generated participant descriptor/evidence ABI.** [crates/trellis/src/generated.rs](https://github.com/oats-center/trellis/blob/27f79d192204de9de78645b1006ceeccb59bf932/crates/trellis/src/generated.rs).

**[S23] TypeScript service installation, deployment consent, and provisioning.** [ts/packages/trellis-test/src/admin/deployment.ts](https://github.com/oats-center/trellis/blob/27f79d192204de9de78645b1006ceeccb59bf932/ts/packages/trellis-test/src/admin/deployment.ts).

**[S24] Release-managed versions and preparation.** [xtask/src/release/versioning.rs](https://github.com/oats-center/trellis/blob/27f79d192204de9de78645b1006ceeccb59bf932/xtask/src/release/versioning.rs); [xtask/src/release/mod.rs](https://github.com/oats-center/trellis/blob/27f79d192204de9de78645b1006ceeccb59bf932/xtask/src/release/mod.rs).

**[S25] Repository instructions and design reading index.** [AGENTS.md](https://github.com/oats-center/trellis/blob/27f79d192204de9de78645b1006ceeccb59bf932/AGENTS.md); [design/README.md](https://github.com/oats-center/trellis/blob/27f79d192204de9de78645b1006ceeccb59bf932/design/README.md).

**[S26] Cargo package file selection.** [Manifest include/exclude fields](https://doc.rust-lang.org/cargo/reference/manifest.html#the-include-and-exclude-fields) and [Publishing](https://doc.rust-lang.org/cargo/reference/publishing.html). Inspect actual archive contents rather than assuming generated files are shipped.

### Task-specific repository reading set

In addition to the source files above, the implementation agent should read the current narrow sections of:

- `design/core/platform-libraries.md` and `design/core/testing-patterns.md` for package/test boundaries.
- `design/contracts/trellis-rust-contract-libraries.md` and `design/contracts/trellis-api-participants.md` for generated facades and exact participant evidence.
- `design/auth/auth-api.md` and `design/auth/auth-protocol.md` for the public auth flows actually used here.
- `docs/src/routes/guides/releasing-trellis/+page.svx` for the repository's current non-publishing release verification procedure.

Read these in the actual feature base before editing. Source/design disagreements must be called out rather than silently resolved by the implementation agent. The plan's source review is not execution evidence: no Cargo compilation, live runtime test, package publication, or merge was performed while writing this document.

---

**Final instruction to the implementation agent:** implement the specified design in the dedicated worktree, prove it through the required real-boundary and artifact-only tests, submit the exact final diff/evidence for coordinating review, address the review findings, and merge into `main` only after explicit approval of the final head/base pair. Do not publish under this authorization.
