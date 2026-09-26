#!/usr/bin/env bash
#
# Runs the TypeScript live integration suite from one self-contained setup.
#
# This is the single entrypoint for the suite locally and in CI:
#
#   deno task -c ts/deno.json test:integration [-- <deno test args>]
#
# No environment variable has to be preset and the tests never compile Rust while
# they run. Locally the runner builds every Rust executable the tests launch under
# `target/debug`; in CI the producer job has already staged them there, so the
# runner only checks that they exist. NATS is resolved by the pinned
# auto-download on both paths.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

fixture_dir="${TRELLIS_TEST_FIXTURE_BIN_DIR:-$root/target/debug/fixtures}"
testkit_live="${TRELLIS_TESTKIT_LIVE_BIN:-$root/target/debug/trellis-testkit-live}"
server_bin="${TRELLIS_TEST_SERVER_BIN:-$root/target/debug/trellis-server}"
cli_bin="${TRELLIS_TEST_CLI_BIN:-$root/target/debug/trellis}"
health_watch_bin="${TRELLIS_TEST_HEALTH_WATCH_BIN:-$root/target/debug/examples/health_watch}"

for generated in \
  ts/packages/trellis/generated.ts \
  ts/packages/trellis/auth/protocol_wasm; do
  [[ -e "$generated" ]] || {
    echo "missing generated artifact $generated; run 'cargo xtask install' first" >&2
    exit 1
  }
done

fixture_bins=(
  trellis-runtime-acceptance
  caller
  resources
  events
  live_probe
  live_probe_caller
  empty_watch
)

# CI stages the producer-built executables; a missing one is a setup failure.
# Anywhere else, build what is needed so a developer never has to prepare the
# environment by hand.
in_ci="${CI:-}"

require_or_build() {
  local path="$1"
  local build="$2"
  if [[ -n "$in_ci" ]]; then
    [[ -x "$path" ]] || {
      echo "missing staged executable $path" >&2
      exit 1
    }
    return
  fi
  # Locally every build runs: cargo is incremental, so this is cheap and can
  # never leave a stale executable behind.
  eval "$build"
}

build_fixtures() {
  echo "building integration fixture helpers"
  CARGO_TARGET_DIR="$root/target" cargo build --bins \
    --manifest-path integration/fixtures/runtime/Cargo.toml \
    --config "patch.crates-io.trellis-rs.path=\"$root/crates/trellis\""
  mkdir -p "$fixture_dir"
  for bin in "${fixture_bins[@]}"; do
    cp "$root/target/debug/$bin" "$fixture_dir/$bin"
  done
}

if [[ -n "$in_ci" ]]; then
  for bin in "${fixture_bins[@]}"; do
    [[ -x "$fixture_dir/$bin" ]] || {
      echo "missing staged fixture $fixture_dir/$bin" >&2
      exit 1
    }
  done
else
  build_fixtures
fi

build_testkit_live() {
  echo "building the testkit live test binary"
  local json
  json="$(mktemp)"
  CARGO_TARGET_DIR="$root/target" cargo test --no-run \
    --manifest-path integration/fixtures/testkit/Cargo.toml \
    --config "patch.crates-io.trellis-protocol.path=\"$root/crates/protocol\"" \
    --config "patch.crates-io.trellis-rs.path=\"$root/crates/trellis\"" \
    --config "patch.crates-io.trellis-testkit.path=\"$root/crates/trellis-testkit\"" \
    --test live \
    --message-format=json > "$json"
  local executable
  executable="$(python3 - "$json" <<'PY'
import json, sys
path = ""
for line in open(sys.argv[1]):
    try:
        message = json.loads(line)
    except ValueError:
        continue
    if message.get("executable"):
        path = message["executable"]
if not path:
    raise SystemExit("failed to locate the testkit live test binary")
print(path)
PY
)"
  mkdir -p "$(dirname "$testkit_live")"
  cp "$executable" "$testkit_live"
}

build_workspace_bins() {
  echo "building the trellis server and CLI"
  CARGO_TARGET_DIR="$root/target" cargo build -p trellis-server -p trellis-cli
}

build_health_watch() {
  echo "building the health_watch example"
  CARGO_TARGET_DIR="$root/target" cargo build -p trellis-runtime --example health_watch
}

require_or_build "$testkit_live" build_testkit_live
require_or_build "$server_bin" build_workspace_bins
require_or_build "$cli_bin" build_workspace_bins
require_or_build "$health_watch_bin" build_health_watch

export TRELLIS_TEST_FIXTURE_BIN_DIR="$fixture_dir"
export TRELLIS_TESTKIT_LIVE_BIN="$testkit_live"
export TRELLIS_TEST_SERVER_BIN="$server_bin"
export TRELLIS_TEST_CLI_BIN="$cli_bin"
export TRELLIS_TEST_HEALTH_WATCH_BIN="$health_watch_bin"

exec deno test --parallel -A -c ts/integration/deno.json ts/integration "$@"
