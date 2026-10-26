#!/usr/bin/env bash
#
# Runs the in-repository Rust testkit suites that need this checkout, from one
# self-contained setup:
#
#   scripts/run-testkit-tests.sh [-- <test filter and libtest flags>]
#
# These are the testkit fixture's test targets other than `live`, which the
# published-package consumer job runs. Several start TypeScript providers or
# callers from this checkout, so they cannot run from that bundle. Locally the
# runner builds the CLI, server and test binaries; in CI (or with
# TRELLIS_TEST_STAGED=1) the producer has staged them under
# target/debug/testkit-tests and the runner only checks they exist.
# TRELLIS_TEST_BUILD_ONLY=1 builds without running.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
[[ "${1:-}" == "--" ]] && shift

targets=(availability device_authority download jobs_authority service_unavailable)
staged="${TRELLIS_TESTKIT_TESTS_DIR:-$root/target/debug/testkit-tests}"
cli_bin="${TRELLIS_TEST_CLI_BIN:-$root/target/debug/trellis}"
server_bin="${TRELLIS_TEST_SERVER_BIN:-$root/target/debug/trellis-server}"

if [[ -z "${CI:-}${TRELLIS_TEST_STAGED:-}" ]]; then
  # Cargo is incremental, so rebuilding every run is cheap and never stale.
  cargo build -p trellis-cli --bin trellis -p trellis-server --bin trellis-server
  args=()
  for target in "${targets[@]}"; do args+=(--test "$target"); done
  json="$(mktemp)"
  CARGO_TARGET_DIR="$root/target" cargo test --no-run \
    --manifest-path integration/fixtures/testkit/Cargo.toml \
    --config "patch.crates-io.trellis-protocol.path=\"$root/crates/protocol\"" \
    --config "patch.crates-io.trellis-models.path=\"$root/crates/models\"" \
    --config "patch.crates-io.trellis-rs.path=\"$root/crates/trellis\"" \
    --config "patch.crates-io.trellis-testkit.path=\"$root/crates/trellis-testkit\"" \
    "${args[@]}" --message-format=json-render-diagnostics > "$json"
  mkdir -p "$staged"
  jq -r 'select(.executable != null) | "\(.target.name) \(.executable)"' "$json" |
    while read -r name executable; do cp "$executable" "$staged/$name"; done
  rm -f "$json"
fi
for bin in "$cli_bin" "$server_bin" "${targets[@]/#/$staged/}"; do
  [[ -x "$bin" ]] || { echo "missing executable $bin" >&2; exit 1; }
done
[[ -z "${TRELLIS_TEST_BUILD_ONLY:-}" ]] || exit 0

nats_version="$(jq -r '.["nats-server"].version' crates/local-nats/nats-binaries.json)"
nats_bin="${TRELLIS_TEST_NATS_BIN:-$root/target/test-nats/$nats_version/nats-server}"
[[ -x "$nats_bin" ]] || bash scripts/acquire-test-nats.sh --dest "$(dirname "$nats_bin")"
export TRELLIS_TEST_NATS_BIN="$nats_bin" TRELLIS_NATS_SERVER_BIN="$nats_bin"
export TRELLIS_TEST_CLI_BIN="$cli_bin" TRELLIS_TEST_SERVER_BIN="$server_bin"
export TRELLIS_CACHE_DIR="${TRELLIS_CACHE_DIR:-$root/target/trellis-nats-cache}"

# Tests resolve the checkout from the package root, as `cargo test` runs them.
cd integration/fixtures/testkit
status=0
for target in "${targets[@]}"; do
  echo "running testkit $target"
  "$staged/$target" --test-threads="${TRELLIS_TEST_CONCURRENCY:-4}" "$@" || status=$?
done
exit "$status"
