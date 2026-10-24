# Live Integration

Tests protect observable behavior, security/data integrity, exact wire
compatibility, and real regressions. Executable Cargo and Node tests are the
catalog; there is no matrix, shared-runtime assignment, or custom scheduler.

Prepare generated artifacts once, then run each catalog through its runner:

```sh
cargo xtask install
scripts/run-rust-tests.sh
npm run test:integration
```

Neither runner needs a preset environment. `scripts/run-rust-tests.sh` acquires
the pinned NATS server and Dex identity provider (Go is required to build Dex),
builds the CLI, server, and Events fixture, sets the test executable variables,
and accepts ordinary filters after `--`, for example
`scripts/run-rust-tests.sh -- upstream_oidc`.

The ordinary `npm run test:integration` runner executes four
test files concurrently by default. Set `TRELLIS_TEST_CONCURRENCY` to override that limit.
Pass Node test flags and paths after `--`, for example
`npm run test:integration -- --test-name-pattern='generated runtime workflows' ts/integration/runtime_test.ts`.
Each Node test
owns and stops its real NATS and Trellis processes. The Rust testkit instead
owns the linked production runtime task and real NATS process; CLI/server
executables are required only for TypeScript and executable-specific acceptance.
First use downloads pinned NATS tools. The generated
`fixtures/runtime/contract.trellis` project exercises one cross-language RPC
plus authorization/revocation, operation, event, state restart, job retry,
transfer integrity, and live observation cancellation.

For the external Rust testkit fixture, patch the full published implementation
closure when testing workspace source (`scripts/run-ts-integration.sh` provides
those patches). The artifact-only package consumer verifies the released-style
closure without a source checkout or either Trellis executable.

Use the cheapest real boundary. SQLite outbox commit/rollback belongs in the
ordinary Node package suite, not a live server test. Generated consumers should
compile and run; do not inspect generated source strings, AST shape, helper
selection, internal call counts, or test inventories. Keep realistic negative
security and data-loss tests. Exact vectors belong in
`integration/fixtures/protocol/` only where independent implementations must
agree on public bytes.

The separate CI demo job runs the checked-in demos' normal commands documented
in `demos/README.md`. Demos are consumer acceptance, not copied test fixtures.
