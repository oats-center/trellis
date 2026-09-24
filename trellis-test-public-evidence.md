# Public Rust trellis-test evidence

## Identity
- Repository: oats-center/trellis
- Plan version / plan commit: coordinator contract copied to `trellis-test-public-implementation-plan.md` (commit `67b6c065`)
- Plan baseline: 27f79d192204de9de78645b1006ceeccb59bf932
- Actual implementation base SHA: 27f79d192204de9de78645b1006ceeccb59bf932
- Current origin/main SHA: 27f79d192204de9de78645b1006ceeccb59bf932
- Final implementation/tested SHA: (pending; feature branch tip)
- Branch and worktree path: `feat/public-rust-trellis-test` at `/home/abalmos/git/qlever/trellis-public-rust-test`
- PR URL: (pending; not opened)
- Clean status output: (pending)
- Compiler/Cargo/Deno versions: (recorded at handoff)

## Baseline drift
- Changed source anchors and mechanical mappings: `crates/trellis-test` on current `main` carried the embedded
  in-process harness (`runtime.rs`, `admin.rs`, `config.rs`, `ports.rs`, `error.rs`, plus
  crate-local tests). Replaced per the coordinator resolution with the out-of-process design
  (`runtime.rs`, `process.rs`, `sandbox.rs`, `admin.rs`, `identity.rs`, `error.rs`) plus the
  projected generated administration source.
- Preserved unrelated release/CI changes: `cc6d1bd5`, `fb2771ed`, `27f79d19` are ancestors of the base.
- Coordinator-approved plan amendments: **DESIGN BLOCKER 1 resolved (plan section 1.2)** — implement on
  current `origin/main`, replace the embedded harness, remove the forbidden private dependency
  closure, inline the Linux parent-death helper into `crates/trellis/tests/integration/cli.rs`,
  and move behavioral coverage to the external fixture.

## Dependency and package proof
- Three crate names/versions/archive SHA-256 hashes: trellis-protocol 0.100.0, trellis-rs 0.100.0,
  trellis-test 0.100.0; `cargo package -p trellis-protocol -p trellis-rs -p trellis-test`
  succeeded with verification enabled (archives in `target/package/`).
- Normalized manifests and source lists: the testkit archive contains 18 files under
  `trellis-test-0.100.0/src/runtime_api/`, including `src/runtime_api/lib.rs`.
- Public-only closure audit output: the artifact-only consumer's `cargo metadata` audit passed
  (`dependency audit passed for N packages`) with no forbidden private crate and every
  non-registry manifest under the three extracted public roots or the consumer's own roots.
- Generated projection path/content-hash comparison: `diff -r crates/runtime-apis/src
  crates/trellis-test/src/runtime_api` → IDENTICAL.
- Second-install drift result: a second `cargo xtask install` produced no changes to the projection.
- Consumer non-registry source-root audit: passed (see closure audit).

## Live runtime evidence
- CLI/server/NATS paths, versions, hashes, and build/source SHA: `target/debug/trellis`,
  `target/debug/trellis-server` (0.100.0), and `~/.cache/trellis/nats-server-v2.14.4`.
- Linux runner/architecture and actual tested cases: x86_64 Linux; fixture live target 12/12
  (T02, T04, T05, T06, T07, T08, T09, T10, T14, T15, T21, T23); artifact-only smoke 7/7 (T07 filtered).
- macOS runner/architecture and actual tested cases: not run.
- Concurrent-runtime isolation evidence: T07 starts eight runtimes in one process; all HTTP/NATS/
  WebSocket endpoints distinct.
- Parent-profile/session sentinel result: T21 asserts the preexisting profile sentinel is
  byte-for-byte unchanged and no admin-session store is written.
- Explicit shutdown/drop/cancellation/panic evidence: explicit shutdown + idempotency (T14);
  drop/cancellation/panic observer tests not yet implemented.
- Retention and sanitized-log results: package-local retention unit tests pass; sanitized-log
  split-token test not yet implemented.
- Network download case result: not run (`DownloadPinned` is a separately selected case).

## Acceptance matrix
| Requirement | Actual test name | Command or CI job | Tested SHA | Result | Evidence |
|---|---|---|---|---|---|
| T01 | `sandbox::tests::*`, `cargo test -p trellis-test --lib` | local | feature tip | PASS | 5 unit tests, no infra |
| T02 | `t02_missing_or_invalid_binaries_fail` | fixture `--test live` | feature tip | PASS | InvalidBinary before startup |
| T03 | (version parsing) | — | | PARTIAL | implemented; explicit test pending |
| T04 | `t04_real_rpc_and_event_between_provider_and_caller` | fixture `--test live` | feature tip | PASS | RPC + event |
| T05 | `t05_agent_caller_calls_the_provider` | fixture `--test live` | feature tip | PASS | agent-bound session |
| T06 | `t06_two_providers_have_distinct_deployments` | fixture `--test live` | feature tip | PASS | distinct deployments/instances |
| T07 | `t07_eight_concurrent_runtimes_are_isolated` | fixture `--test live` | feature tip | PASS | 8 runtimes |
| T08 | `t08_restricted_caller_is_denied` | fixture `--test live` | feature tip | PASS | real denial, handler not run |
| T09 | `t09_duplicate_names_are_rejected` | fixture `--test live` | feature tip | PASS | DuplicateName |
| T10 | `t10_unsupported_participant_kind_is_rejected` | fixture `--test live` | feature tip | PASS | device rejected |
| T11 | (missing NATS / early exit) | — | | PARTIAL | implemented; explicit test pending |
| T12 | (cross-process) | — | | NOT RUN | artifact smoke covers in-process only |
| T13 | `port_conflict_matches_only_selected_ports`, `port_lease_*` | `cargo test -p trellis-test --lib` | feature tip | PARTIAL | classification + lease; real occupied-port case pending |
| T14 | `t14_shutdown_is_idempotent` | fixture `--test live` | feature tip | PASS | idempotent shutdown |
| T15 | `t15_dropping_a_runtime_cleans_up` | fixture `--test live` | feature tip | PASS | drop stops the server |
| T16 | (start cancellation) | — | | NOT RUN | pending |
| T17 | (panic/unwind) | — | | NOT RUN | pending |
| T18 | (force-stop server) | — | | NOT RUN | pending |
| T19 | `*_retention_*` | `cargo test -p trellis-test --lib` | feature tip | PASS | three policies + sibling survival |
| T20 | `t21_complete_session_does_not_write_the_default_store` | fixture `--test live` | feature tip | PARTIAL | sentinel unchanged; dedicated T20 concurrency pending |
| T21 | `t21_complete_session_does_not_write_the_default_store` | fixture `--test live` | feature tip | PASS | storage-free bind |
| T22 | T07 distinct endpoints | fixture `--test live` | feature tip | PASS | loopback only |
| T23 | `t23_sandbox_path_with_spaces_works` | fixture `--test live` | feature tip | PASS | spaced sandbox path |
| T24 | (bounded redacted tails) | — | | PARTIAL | bounded tails + redaction implemented; split-token test pending |
| T25 | (DownloadPinned) | — | | NOT RUN | pending |
| T26 | projection `diff -r` + second install | `cargo xtask install` | feature tip | PASS | identical + idempotent |
| T27 | `cargo package` three crates | local | feature tip | PASS | verified archives |
| T28 | artifact-only consumer smoke | `scripts/verify-rust-test-package.sh` | feature tip | PASS | no checkout, 7 live pass |
| T29 | Linux native live | fixture `--test live` | feature tip | PARTIAL | Linux only; macOS pending |
| T30 | versioning paths registered | `cargo build -p xtask` | feature tip | PARTIAL | paths added; stable/prerelease tests pending |
| T31 | existing suites | — | | NOT RUN | repo-wide re-run pending |

## CI and release verification
- Required Check run/job links and status: jobs added (`live` fixture step, `rust-testkit-package`,
  `rust-testkit-consumer`); not yet executed.
- Artifact-only no-checkout consumer job link: `rust-testkit-consumer` (no `actions/checkout`); not yet executed.
- Tagged/tagless package-smoke results: `rust-testkit-consumer` added to `release.yml`; not yet executed.
- Stable/prerelease version-preparation tests: not yet implemented.
- Publishing workflow not invoked: yes
- Registry-name/ownership preflight: not yet verified by maintainer

## Remaining issues
- Test failures: none in the covered set.
- Not-run tests with exact reason: T02/T03/T05/T11/T12/T15/T16/T17/T18/T23/T25/T29(macOS)/T30/T31 —
  not yet implemented or executed (see matrix).
- Design blockers: DESIGN BLOCKER 1 resolved; none open.
- Known platform/cleanup limits: only Linux exercised; drop/cancellation/panic observer tests pending.
- Other preexisting unrelated failures: none observed.

## Review state
- State: IN PROGRESS (W0–W4 complete, W5 complete, W6 largely complete, W7 partial, W8 in progress)
- Coordinator approval: NOT GRANTED
- Approved head SHA: none
- Approved main/base SHA: none
- Merge performed: no
- Publication performed: no
