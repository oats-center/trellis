#!/usr/bin/env bash
#
# Runs every test suite the Check workflow runs, as Check does: build once,
# then run the suites concurrently against those executables.
#
#   npm test
#
# The build phase regenerates contract artifacts and the embedded Console and
# Portal, then lets each suite's runner build what it launches. The suites then
# run in staged mode (TRELLIS_TEST_STAGED=1), so none of them compiles while
# another is running, and each writes its log under target/test-logs/. Within a
# suite, test files also run concurrently; TRELLIS_TEST_CONCURRENCY and
# TRELLIS_BROWSER_CONCURRENCY override the per-suite defaults sized from the
# host's cores. TRELLIS_TEST_SKIP_PREPARE=1 skips contract and UI regeneration
# when they are known to be current.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
logs="$root/target/test-logs"
# Start from empty logs so a failed run never shows an earlier run's results.
rm -rf "$logs"
mkdir -p "$logs"

suites=(rust integration sdk browser)
declare -A command=(
  [rust]="bash scripts/run-rust-tests.sh"
  [integration]="bash scripts/run-ts-integration.sh"
  [sdk]="bash scripts/run-sdk-tests.sh"
  [browser]="bash scripts/run-browser-tests.sh"
)

started=$SECONDS
if [[ -z "${TRELLIS_TEST_SKIP_PREPARE:-}" ]]; then
  echo "preparing generated contracts and embedded UIs"
  cargo xtask install > "$logs/prepare.log" 2>&1 \
    && npm run browser:embedded >> "$logs/prepare.log" 2>&1 \
    || { echo "preparation failed; see $logs/prepare.log" >&2; exit 1; }
fi
# Builds share Cargo's lock, so they run one runner at a time.
for suite in "${suites[@]}"; do
  echo "building for $suite"
  TRELLIS_TEST_BUILD_ONLY=1 ${command[$suite]} > "$logs/$suite.build.log" 2>&1 \
    || { echo "build for $suite failed; see $logs/$suite.build.log" >&2; exit 1; }
done
echo "built in $(( SECONDS - started ))s; running ${suites[*]} concurrently"

declare -A pid
for suite in "${suites[@]}"; do
  TRELLIS_TEST_STAGED=1 ${command[$suite]} > "$logs/$suite.log" 2>&1 &
  pid[$suite]=$!
done
trap 'kill "${pid[@]}" 2>/dev/null || true' INT TERM

status=0
for suite in "${suites[@]}"; do
  if wait "${pid[$suite]}"; then
    echo "pass  $suite"
  else
    status=1
    echo "FAIL  $suite (see $logs/$suite.log)"
  fi
done
echo "finished in $(( SECONDS - started ))s"
exit "$status"
