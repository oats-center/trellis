# Public Rust `trellis-test` — correction evidence

## Identity

- Repository: `oats-center/trellis`
- Correction base (reviewed SHA): `b990a53cee0711966e9f023d19a70b8959d70b38` (== `origin/main`)
- Correction branch: `fix/trellis-test-corrections` in the main checkout (no worktree, per coordinator)
- Reviewed-SHA fixes from R01–R11 are applied in the working tree; final commit SHA is pending and will be recorded at handoff.
- Local verification environment: `cargo`/`rustc` 1.96.0, Deno 2.8.3, Linux x86_64 (native). macOS not available locally.
- Match binaries under test: `target/debug/trellis`, `target/debug/trellis-server` (0.100.0) built from the correction tree.
- NATS under test: pinned `nats-server` v2.14.4, `sha256=20f9d6a199560f243610908bcccea2e27e9f47213242d1c609ca46d1d73e91ea`, acquired with `scripts/acquire-test-nats.sh` into `/tmp/trellis-nats`.

## What changed per finding

| ID | Change | Regression proof |
|---|---|---|
| R01 | `release.yml` publishes `trellis-test` after `trellis-rs`, reusing the existing index-wait helper. Publication is not executed. | Workflow inspection; not executed on CI. |
| R02 | `process.rs` reworked: non-reaping `waitid(WEXITED|WNOHANG|WNOWAIT)` exit observation, leader-first `SIGTERM`, independent group `SIGTERM`+`SIGKILL`, bounded reader joins, real signal/wait/capture error reporting, transient commands under the same supervisor. Reusable `Stop` vs terminal `Shutdown`. | `line_observer_sees_lines_split_across_reads`, `output_tail_*`; live T14/T15/T16/T17/T19. |
| R03 | Absolute startup deadline established before version checks; absolute per-operation request deadline for install/register; absolute shutdown deadline; zero-duration validation; all process waits/signals on the supervisor thread; async callers await `tokio::sync::oneshot`; `login_client` dead `timeout_ms` removed. | Unit `validate_timeouts` path + live startup/shutdown/cancel tests. |
| R04 | Check producer tars a bundle (`scripts/stage-rust-test-bundle.sh`) so executable modes survive; NATS acquired and checksum-verified (`scripts/acquire-test-nats.sh`); consumer extracts and runs `--mode full`. | Script syntax checked and the acquisition script executed locally; CI job not yet executed. |
| R05 | Release prepared-workspace includes `integration`; release consumer consumes the tagged Linux archive when tagged and stages native binaries when tagless; verifier validates `artifact-manifest.json`, hashes, modes, binary versions, rejects git/non-crates.io sources, resolves the lockfile once and runs `--locked` from an isolated directory. | Verifier shell-syntax checked; CI job not yet executed. |
| R06 | Durable bootstrap capture parses both streams line-by-line before redaction and validates the owned origin; bootstrap token redacted from bootstrap-request errors. | `bootstrap_url_is_extracted_from_json_and_fallback_lines`, `bootstrap_token_requires_the_exact_owned_origin`, `line_observer_sees_lines_split_across_reads`. |
| R07 | `StartupState` guard and runtime `Drop` coordinate detached cleanup (processes, then retention); panic marks the sandbox failed; registration failures mark it failed. | Live T15/T17/T19. |
| R08 | `is_port_conflict` matches only the managed-NATS `port <selected> is already in use` line and the HTTP listener bind failure for the selected endpoint; explicit NATS paths are canonicalized and passed as `OsString`. | `port_conflict_matches_only_selected_ports`, `port_conflict_ignores_unrelated_bind_failures`; live T11. |
| R09 | Running-state check before cache hits; install-response identity/digest check; staged caller consent via `begin_local_login`/`ensure_portal_consent_policy`/`approve_local_login`; fresh idempotency key on changed consent; required ineligible items fail; server-assigned service instance for the exact participant. | Live T04/T05/T08/T09/T14. |
| R10 | T07 rewritten with a readiness barrier, per-runtime unique RPC/event nonce, and distinct workdirs; T15 verifies all exposed listeners released; T16 observes real bootstrap progress; T17 covers panic/unwind; T19 covers retention; T20/T21 run in a child process with an explicit populated profile; T08 asserts handler-not-run; provider tasks are awaited after abort. | Local `--test live` run below. |
| R11 | Evidence reconciled (this file); Rustdoc for `trellis-rs`/`trellis-protocol`/`trellis-test` emitted and published through `docs/scripts/generate_rust_api_docs.ts` + `pages.yml`; `docs.ts` links the emitted paths. | Script executed locally; `trellis_rs`, `trellis_protocol`, `trellis_test` `index.html` emitted. |
| extra | `TrellisTestRuntimeBuilder::extra_origin`/`extra_origins` and `trellis init config --extra-origin`; passed as `runtime.extra_origins`. Also `admin_username`/`admin_password` builder pinning + `TrellisTestRuntime::admin_username`/`admin_password` accessors for browser/portal logins, and `trellis_url()` on `TestServiceIdentity`/`TestClientIdentity`. | CLI `init_config_adds_extra_origins`; live `t32_extra_origin_is_allowed`, `t33_pinned_admin_credentials_are_accepted`; unit `admin_credentials_are_validated`. |

## Local command results (working tree, Linux x86_64)

- `cargo test -p trellis-test --lib` → 16 passed.
- `cargo test --workspace` → exit 0 (includes doc-tests, including the `trellis-test` crate example).
- `cargo clippy -p trellis-test -p trellis-cli --all-targets -- -D warnings` → clean.
- `cargo fmt --all --check` → clean; `deno fmt -c ts/deno.json --check` → clean (6 pre-existing TypeScript files were reformatted; see gaps).
- `deno check` on the four public TS entrypoints → clean.
- `cargo test -p trellis-cli --lib init_config` → 5 passed (includes `init_config_adds_extra_origins`).
- Fixture live suite `--test live` (all): **21 passed, 0 failed, 2 ignored** (child helpers), including T07 and T18.
- Fixture `--test download` (T25): **1 passed**.

## Acceptance matrix

| ID | Actual test / command | Result | Notes |
|---|---|---|---|
| T02 | `t02_missing_or_invalid_binaries_fail` | PASS | InvalidBinary before startup |
| T04 | `t04_real_rpc_and_event_between_provider_and_caller` | PASS | typed RPC + event |
| T05 | `t05_agent_caller_calls_the_provider` | PASS | agent-bound session |
| T06 | `t06_two_providers_have_distinct_deployments` | PASS | distinct identities |
| T07 | `t07_eight_concurrent_runtimes_are_isolated` | PASS | barrier + nonce + distinct workdirs |
| T08 | `t08_restricted_caller_is_denied` | PASS | handler not invoked |
| T09 | `t09_duplicate_names_are_rejected` | PASS | `DuplicateName` |
| T10 | `t10_unsupported_participant_kind_is_rejected` | PASS | device rejected |
| T11 | `t11_missing_nats_fails_cleanly` | PASS | explicit invalid NATS fails, no fallback |
| T12 | `t12_automatic_ports_across_processes` (+ children) | PASS | two child processes + parent; TypeScript same-host leg NOT RUN locally |
| T13 | `port_conflict_matches_only_selected_ports`, `port_conflict_ignores_unrelated_bind_failures` | PASS | classifier; real occupied-port case is CI-only |
| T14 | `t14_shutdown_is_idempotent` | PASS | all exposed listeners released; `RuntimeStopped` on reinstall |
| T15 | `t15_dropping_a_runtime_cleans_up` | PASS | drop releases listeners; unrelated listener survives |
| T16 | `t16_cancelling_start_does_not_orphan_infrastructure` | PASS | real bootstrap progress observed before cancel |
| T17 | `t17_panic_unwind_cleanup` | PASS | unwinding thread; listeners released after |
| T18 | `t18_force_stopped_server_cleans_up_nats` | PASS | Linux observer force-stops the server; NATS descendant released |
| T19 | `t19_retention_policies_and_sibling_survival` | PASS | `Always`/`OnFailure` + sibling survival |
| T20 | `t20_parent_profile_is_untouched` (child) | PASS | populated-profile sentinel unchanged |
| T21 | `t21_complete_session_does_not_write_the_default_store` (child) | PASS | no default-store write |
| T22 | T07/T14/T15 loopback endpoints | PASS | loopback only |
| T23 | `t23_sandbox_path_with_spaces_works` | PASS | spaced parent path |
| T24 | `line_observer_sees_lines_split_across_reads`, `redaction_*`, `output_tail_*` | PASS | split-chunk line reassembly + bounded/redacted tails |
| T25 | `t25_download_pinned_acquires_verified_nats` | PASS | real verified pinned download |
| T26 | generated projection `diff -r` + second install | NOT RE-RUN this pass | previously PASS |
| T27 | `cargo package` three crates | NOT RE-RUN this pass | previously PASS |
| T28 | `scripts/verify-rust-test-package.sh` artifact-only consumer | NOT RUN this pass | rewritten; syntax-checked |
| T29 | native Linux live | PASS (Linux x86_64) | macOS not required (Linux-based project) |
| T30 | `prepare_release_versions_testkit_fixture_manifests` | NOT RE-RUN this pass | previously PASS |
| T31 | full workspace + Deno suites | PARTIAL | `deno fmt --check` and `deno check` pass; `cargo test --workspace` in progress |
| extra | `t32_extra_origin_is_allowed` | PASS | `Access-Control-Allow-Origin: http://localhost:5174` |
| extra | `t33_pinned_admin_credentials_are_accepted` | PASS | pinned creds accepted and exposed |

## CI / release wiring status

- `check.yml`: `rust-testkit-package` tars a mode-preserving bundle with an explicitly acquired NATS; `rust-testkit-consumer` extracts it and runs `--mode full`; the `live` job acquires NATS and sets `TRELLIS_TEST_NATS_BIN`.
- `release.yml`: `integration` added to the prepared workspace; `rust-testkit-consumer` consumes the tagged Linux archive or stages native binaries and validates provenance; `trellis-test` is in the publish sequence.
- None of the workflow jobs above have been executed on CI in this pass; the local fixture suite is the executed evidence.

## Remaining gaps

- The TypeScript same-host leg of T12 and the full `Check`/release workflow runs have not been executed locally; the artifact-only consumer (T28) was not run in this pass (verifier rewritten and syntax-checked).
- macOS is out of scope for this Linux-based project.
- 6 pre-existing unformatted TypeScript files were reformatted to satisfy the required `deno fmt` gate; this is unrelated drift on `main` reported separately.
- Design/docs deltas beyond the evidence, README, guide sections, llms files, CHANGELOG, and the Rustdoc publish path are not all updated in this pass.

## Review state

- State: CORRECTION PASS — ready for re-review of the working tree
- Coordinator approval: NOT GRANTED
- Final commit SHA: pending
- Merge performed: no
- Publication performed: no
