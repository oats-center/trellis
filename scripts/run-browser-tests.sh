#!/usr/bin/env bash
#
# Runs the Playwright browser suite from one self-contained setup:
#
#   npm run test:browser [-- <node test flags and test paths>]
#
# No environment variable has to be preset. Locally the runner builds the CLI
# and server the tests launch; the testkit acquires NATS itself. With
# TRELLIS_TEST_STAGED=1 (as set by `scripts/check.sh`) or in CI it only checks
# that they exist. TRELLIS_TEST_SHARD=<index>/<total> selects a Node-native
# file shard of the complete list, and TRELLIS_BROWSER_CONCURRENCY overrides
# the number of concurrent test files.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

cli_bin="${TRELLIS_TEST_CLI_BIN:-$root/target/debug/trellis}"
server_bin="${TRELLIS_TEST_SERVER_BIN:-$root/target/debug/trellis-server}"
if [[ -z "${CI:-}${TRELLIS_TEST_STAGED:-}" ]]; then
  # Cargo is incremental, so rebuilding every run is cheap and never stale.
  cargo build -p trellis-cli --bin trellis -p trellis-server --bin trellis-server
fi
for bin in "$cli_bin" "$server_bin"; do
  [[ -x "$bin" ]] || { echo "missing executable $bin" >&2; exit 1; }
done
[[ -z "${TRELLIS_TEST_BUILD_ONLY:-}" ]] || exit 0

export TRELLIS_TEST_CLI_BIN="$cli_bin" TRELLIS_TEST_SERVER_BIN="$server_bin"
export TRELLIS_CACHE_DIR="${TRELLIS_CACHE_DIR:-$root/target/trellis-nats-cache}"

if [[ $# -eq 0 ]]; then
  set -- ts/browser/*.browser_test.ts
fi
shard_args=()
if [[ -n "${TRELLIS_TEST_SHARD:-}" ]]; then
  shard_args=("--test-shard=${TRELLIS_TEST_SHARD}")
  echo "Running browser shard ${TRELLIS_TEST_SHARD} from $# test files"
fi
cores="$(nproc)"
exec node --experimental-transform-types --test \
  --test-concurrency="${TRELLIS_BROWSER_CONCURRENCY:-$(( cores / 4 > 4 ? cores / 4 : 4 ))}" \
  "${shard_args[@]}" "$@"
