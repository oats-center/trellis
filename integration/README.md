# Live Integration

Tests protect observable behavior, security/data integrity, exact wire
compatibility, and real regressions. Executable Cargo and Deno tests are the
catalog; there is no matrix, shared-runtime assignment, or custom scheduler.

Prepare generated artifacts and build the two binaries once:

```sh
cargo xtask install
cargo build --manifest-path rust/Cargo.toml -p trellis-server -p trellis-cli
export TRELLIS_TEST_SERVER_BIN="$PWD/rust/target/debug/trellis-server"
export TRELLIS_TEST_CLI_BIN="$PWD/rust/target/debug/trellis"
```

Use ordinary discovery and native filters:

```sh
cargo test --manifest-path rust/Cargo.toml -p trellis-rs \
  --features live-integration --test integration -- --test-threads=1 --nocapture
deno test -A -c ts/integration/deno.json ts/integration
```

Deno runs test modules serially by default; do not add `--parallel`. Each live
test owns and stops its real NATS and Trellis processes. First use downloads
pinned NATS tools. The generated `fixtures/runtime/contract.trellis` project
exercises one cross-language RPC plus authorization/revocation, operation,
event, state restart, job retry, transfer integrity, and feed cancellation.

Use the cheapest real boundary. SQLite outbox commit/rollback belongs in the
ordinary Deno package suite, not a live server test. Generated consumers should
compile and run; do not inspect generated source strings, AST shape, helper
selection, internal call counts, or test inventories. Keep realistic negative
security and data-loss tests. Exact vectors belong in `conformance/` only where
independent implementations must agree on public bytes.

The separate CI demo job runs the checked-in demos' normal commands documented
in `demos/README.md`. Demos are consumer acceptance, not copied test fixtures.
