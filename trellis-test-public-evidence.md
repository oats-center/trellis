# Public Rust trellis-test evidence

## Identity
- Repository: oats-center/trellis
- Plan version / plan commit: coordinator contract `trellis-test-public-implementation-plan-current-main.md`, copied to `trellis-test-public-implementation-plan.md` in this worktree (see that file's commit)
- Plan baseline: 27f79d192204de9de78645b1006ceeccb59bf932
- Actual implementation base SHA: 27f79d192204de9de78645b1006ceeccb59bf932
- Current origin/main SHA: 27f79d192204de9de78645b1006ceeccb59bf932
- Final implementation/tested SHA: (pending)
- Branch and worktree path: `feat/public-rust-trellis-test` at `/home/abalmos/git/qlever/trellis-public-rust-test`
- PR URL: (pending; not opened)
- Clean status output: (pending)
- Compiler/Cargo/Deno versions: (pending)

## Baseline drift
- Changed source anchors and mechanical mappings: `crates/trellis-test` on current `main` contains the embedded harness
  (`runtime.rs`, `admin.rs`, `config.rs`, `ports.rs`, `error.rs`, plus crate-local tests
  `runtime_boot.rs`, `admin_login.rs`, `apply_and_provision.rs`). The plan's `[S1]`
  baseline description of a "process helper only" no longer holds.
- Preserved unrelated release/CI changes: `cc6d1bd5`, `fb2771ed`, `27f79d19` are ancestors of the base and are preserved.
- Coordinator-approved plan amendments: **DESIGN BLOCKER 1 resolved by the coordinator (plan section 1.2)** —
  implement on current `origin/main`, replace the embedded harness with the out-of-process
  publishable design, remove the forbidden private dependency closure, inline the Linux
  parent-death helper into `crates/trellis/tests/integration/cli.rs`, and move the behavioral
  coverage to the new external fixture. No other A01–A22 decision changed.

## Dependency and package proof
- Three crate names/versions/archive SHA-256 hashes: (pending)
- Normalized manifests and source lists: (pending)
- Public-only closure audit output: (pending)
- Generated projection path/content-hash comparison: (pending)
- Second-install drift result: (pending)
- Consumer non-registry source-root audit: (pending)

## Live runtime evidence
- CLI/server/NATS paths, versions, hashes, and build/source SHA: (pending)
- Linux runner/architecture and actual tested cases: (pending)
- macOS runner/architecture and actual tested cases: (pending)
- Concurrent-runtime isolation evidence: (pending)
- Parent-profile/session sentinel result: (pending)
- Explicit shutdown/drop/cancellation/panic evidence: (pending)
- Retention and sanitized-log results: (pending)
- Network download case result: (pending)

## Acceptance matrix
| Requirement | Actual test name | Command or CI job | Tested SHA | Result | Evidence |
|---|---|---|---|---|---|
| T01 ... T31 | | | | | |

## CI and release verification
- Required Check run/job links and status: (pending)
- Artifact-only no-checkout consumer job link: (pending)
- Tagged/tagless package-smoke results: (pending)
- Stable/prerelease version-preparation tests: (pending)
- Publishing workflow not invoked: yes
- Registry-name/ownership preflight: not yet verified by maintainer

## Remaining issues
- Test failures: (pending)
- Not-run tests with exact reason: (pending)
- Design blockers: DESIGN BLOCKER 1 resolved (plan section 1.2); none open
- Known platform/cleanup limits: (pending)
- Other preexisting unrelated failures: (pending)

## Review state
- State: IN PROGRESS (W0 complete)
- Coordinator approval: NOT GRANTED
- Approved head SHA: none
- Approved main/base SHA: none
- Merge performed: no
- Publication performed: no
