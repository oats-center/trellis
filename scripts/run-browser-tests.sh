#!/usr/bin/env bash
#
# Runs the Playwright browser suite from one self-contained setup:
#
#   npm run test:browser [-- <node test flags and test paths>]
#
# No environment variable has to be preset. Locally the runner acquires the
# pinned NATS server and Dex identity provider and builds the CLI, server and
# Events fixture the tests launch. With TRELLIS_TEST_STAGED=1 (as set by
# `scripts/run-tests.sh`) or in CI it only checks that they exist.
# TRELLIS_BROWSER_CONCURRENCY overrides the number of concurrent test files.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

nats_version="$(python3 -c 'import json; print(json.load(open("crates/local-nats/nats-binaries.json"))["nats-server"]["version"])')"
nats_bin="${TRELLIS_TEST_NATS_BIN:-${TRELLIS_NATS_SERVER_BIN:-$root/target/test-nats/$nats_version/nats-server}}"
dex_bin="${TRELLIS_TEST_DEX_BIN:-$root/target/test-identity/$(bash scripts/acquire-test-dex.sh --print-commit)/dex}"
cli_bin="${TRELLIS_TEST_CLI_BIN:-$root/target/debug/trellis}"
server_bin="${TRELLIS_TEST_SERVER_BIN:-$root/target/debug/trellis-server}"
events_bin="${TRELLIS_TEST_EVENTS_BIN:-$root/target/debug/fixtures/events}"

if [[ -n "${CI:-}${TRELLIS_TEST_STAGED:-}" ]]; then
  for bin in "$nats_bin" "$dex_bin" "$cli_bin" "$server_bin" "$events_bin"; do
    [[ -x "$bin" ]] || { echo "missing staged executable $bin" >&2; exit 1; }
  done
else
  [[ -x "$nats_bin" ]] || bash scripts/acquire-test-nats.sh --dest "$(dirname "$nats_bin")"
  [[ -x "$dex_bin" ]] || bash scripts/acquire-test-dex.sh --dest "$(dirname "$dex_bin")"
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
[[ -z "${TRELLIS_TEST_BUILD_ONLY:-}" ]] || exit 0

export TRELLIS_TEST_NATS_BIN="$nats_bin" TRELLIS_NATS_SERVER_BIN="$nats_bin" NATS_SERVER_BIN="$nats_bin"
export TRELLIS_TEST_DEX_BIN="$dex_bin" TRELLIS_TEST_CLI_BIN="$cli_bin"
export TRELLIS_TEST_SERVER_BIN="$server_bin" TRELLIS_TEST_EVENTS_BIN="$events_bin"
export TRELLIS_CACHE_DIR="${TRELLIS_CACHE_DIR:-$root/target/trellis-nats-cache}"

if [[ $# -eq 0 ]]; then
  set -- ts/browser/*.browser_test.ts
fi
cores="$(nproc)"
exec node --experimental-transform-types --test \
  --test-concurrency="${TRELLIS_BROWSER_CONCURRENCY:-$(( cores / 4 > 4 ? cores / 4 : 4 ))}" \
  "$@"
