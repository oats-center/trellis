#!/usr/bin/env bash
#
# Runs the Playwright browser suite from one self-contained setup:
#
#   npm run test:browser [-- <node test flags and test paths>]
#
# No environment variable has to be preset. Locally the runner builds the CLI
# and server the tests launch; the testkit acquires NATS itself, and the runner
# downloads the pinned Dex release the OIDC sign-in tests start and the pinned
# OCI registry the published-package tests publish to. With
# TRELLIS_TEST_STAGED=1 (as set by `scripts/check.sh`) or in CI it only checks
# that they exist. TRELLIS_BROWSER_CONCURRENCY overrides the number of
# concurrent test files.
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

# Released dependencies are downloaded and verified, never built.
dex_bin="${TRELLIS_TEST_DEX_BIN:-$root/target/test-identity/$(bash scripts/acquire-test-dex.sh --print-version)/dex}"
[[ -x "$dex_bin" ]] || bash scripts/acquire-test-dex.sh --dest "$(dirname "$dex_bin")"
registry_bin="${TRELLIS_TEST_REGISTRY_BIN:-$root/target/test-registry/$(bash scripts/acquire-test-registry.sh --print-version)/registry}"
[[ -x "$registry_bin" ]] || bash scripts/acquire-test-registry.sh --dest "$(dirname "$registry_bin")"

export TRELLIS_TEST_CLI_BIN="$cli_bin" TRELLIS_TEST_SERVER_BIN="$server_bin"
export TRELLIS_TEST_DEX_BIN="$dex_bin" TRELLIS_TEST_REGISTRY_BIN="$registry_bin"
export TRELLIS_CACHE_DIR="${TRELLIS_CACHE_DIR:-$root/target/trellis-nats-cache}"

if [[ $# -eq 0 ]]; then
  # The browser tests and the unit tests of the harness they share.
  set -- ts/browser/*_test.ts
fi
cores="$(nproc)"
# Each file drives Chromium as well as a server, so one file per two cores.
concurrency="${TRELLIS_BROWSER_CONCURRENCY:-$(( cores / 2 > 4 ? cores / 2 : 4 ))}"
echo "Running up to $concurrency browser test files at once on $cores cores"
exec node --test \
  --test-concurrency="$concurrency" \
  "$@"
