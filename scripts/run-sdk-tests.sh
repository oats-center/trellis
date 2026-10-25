#!/usr/bin/env bash
#
# Runs the TypeScript package unit suites from one self-contained setup:
#
#   npm run test:sdk
#
# Some package tests start a real runtime through the testkit. CI supplies the
# producer-built CLI and server through TRELLIS_TEST_CLI_BIN and
# TRELLIS_TEST_SERVER_BIN; anywhere else, unset variables default to
# `target/debug` and the runner builds both first. Cargo is incremental, so the
# build is cheap and never leaves a stale executable behind.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

if [[ -z "${TRELLIS_TEST_CLI_BIN:-}" || -z "${TRELLIS_TEST_SERVER_BIN:-}" ]]; then
  [[ -z "${CI:-}" ]] || {
    echo "CI must supply TRELLIS_TEST_CLI_BIN and TRELLIS_TEST_SERVER_BIN" >&2
    exit 1
  }
  # Staged runs (scripts/check.sh) have already built both.
  [[ -n "${TRELLIS_TEST_STAGED:-}" ]] \
    || cargo build -p trellis-cli --bin trellis -p trellis-server --bin trellis-server
  export TRELLIS_TEST_CLI_BIN="${TRELLIS_TEST_CLI_BIN:-$root/target/debug/trellis}"
  export TRELLIS_TEST_SERVER_BIN="${TRELLIS_TEST_SERVER_BIN:-$root/target/debug/trellis-server}"
fi
for bin in "$TRELLIS_TEST_CLI_BIN" "$TRELLIS_TEST_SERVER_BIN"; do
  [[ -x "$bin" ]] || { echo "missing executable $bin" >&2; exit 1; }
done

[[ -z "${TRELLIS_TEST_BUILD_ONLY:-}" ]] || exit 0
npm test \
  --workspace @oatscenter/result \
  --workspace @oatscenter/trellis \
  --workspace @oatscenter/trellis-testkit \
  --workspace @oatscenter/trellis-svelte \
  --workspace @trellis/console
node ts/tools/run_unit_tests.ts ts/tools
npm run test:benchmarks
