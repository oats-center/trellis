#!/usr/bin/env bash
#
# Runs the complete Rust workspace test catalog from one self-contained setup:
#
#   scripts/run-rust-tests.sh [-- <cargo test filter and flags>]
#
# No environment variable has to be preset. The runner downloads the pinned
# NATS server and Dex release; locally it builds the CLI, server and Events
# fixture the tests launch, and then runs every workspace test, so no test fails
# merely because an executable was never provided. In CI the producer jobs stage
# the Trellis executables and set the variables; the runner then only checks
# that they exist. `scripts/check.sh` uses the same staged mode
# (TRELLIS_TEST_STAGED=1) after building once, and TRELLIS_TEST_BUILD_ONLY=1
# builds the executables and test binaries without running them.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
[[ "${1:-}" == "--" ]] && shift
features="trellis-rs/event-store-sqlite,trellis-rs/event-store-postgres"

nats_version="$(jq -r '.["nats-server"].version' crates/local-nats/nats-binaries.json)"
nats_bin="${TRELLIS_TEST_NATS_BIN:-${TRELLIS_NATS_SERVER_BIN:-$root/target/test-nats/$nats_version/nats-server}}"
dex_bin="${TRELLIS_TEST_DEX_BIN:-$root/target/test-identity/$(bash scripts/acquire-test-dex.sh --print-version)/dex}"
cli_bin="${TRELLIS_TEST_CLI_BIN:-$root/target/debug/trellis}"
server_bin="${TRELLIS_TEST_SERVER_BIN:-$root/target/debug/trellis-server}"
events_bin="${TRELLIS_TEST_EVENTS_BIN:-$root/target/debug/fixtures/events}"

# Released dependencies are downloaded and verified, never built.
[[ -x "$nats_bin" ]] || bash scripts/acquire-test-nats.sh --dest "$(dirname "$nats_bin")"
[[ -x "$dex_bin" ]] || bash scripts/acquire-test-dex.sh --dest "$(dirname "$dex_bin")"
if [[ -n "${CI:-}${TRELLIS_TEST_STAGED:-}" ]]; then
  for bin in "$cli_bin" "$server_bin" "$events_bin"; do
    [[ -x "$bin" ]] || { echo "missing staged executable $bin" >&2; exit 1; }
  done
else
  # Cargo is incremental, so rebuilding every run is cheap and never stale.
  cargo build -p trellis-cli --bin trellis -p trellis-server --bin trellis-server
  CARGO_TARGET_DIR="$root/target" cargo build --bin events \
    --manifest-path integration/fixtures/runtime/Cargo.toml \
    --config "patch.crates-io.trellis-rs.path=\"$root/crates/trellis\"" \
    --config "patch.crates-io.trellis-protocol.path=\"$root/crates/protocol\"" \
    --config "patch.crates-io.trellis-models.path=\"$root/crates/models\""
  mkdir -p "$(dirname "$events_bin")"
  cp "$root/target/debug/events" "$events_bin"
fi

export TRELLIS_TEST_NATS_BIN="$nats_bin" TRELLIS_NATS_SERVER_BIN="$nats_bin" NATS_SERVER_BIN="$nats_bin"
export TRELLIS_TEST_DEX_BIN="$dex_bin" TRELLIS_TEST_CLI_BIN="$cli_bin"
export TRELLIS_TEST_SERVER_BIN="$server_bin" TRELLIS_TEST_EVENTS_BIN="$events_bin"
export TRELLIS_CACHE_DIR="${TRELLIS_CACHE_DIR:-$root/target/trellis-nats-cache}"

if [[ -n "${TRELLIS_TEST_BUILD_ONLY:-}" ]]; then
  if cargo nextest --version >/dev/null 2>&1; then
    cargo nextest run --workspace --features "$features" --no-run
  else
    cargo test --workspace --features "$features" --no-run
  fi
  exit 0
fi

# Documentation examples run even when the main catalog fails.
status=0
if cargo nextest --version >/dev/null 2>&1; then
  cargo nextest run --workspace --features "$features" --no-fail-fast "$@" || status=$?
else
  cargo test --workspace --features "$features" --no-fail-fast "$@" || status=$?
fi
cargo test --workspace --features "$features" --doc || status=$?
exit "$status"
