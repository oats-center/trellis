#!/usr/bin/env bash
#
# Runs the TypeScript live integration suite from one self-contained setup.
#
# This is the single entrypoint for the suite locally and in CI:
#
#   npm run test:integration [-- <node test flags and test paths>]
#
# No environment variable has to be preset and the tests never compile Rust while
# they run. For CI parallelism TRELLIS_TEST_SHARD=1/4 through 4/4 selects a
# Node-native file shard from exactly the same complete test-file list.
# Locally the runner builds every Rust executable the tests launch under
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
nats_version="$(node --input-type=module -e 'import pin from "./crates/local-nats/nats-binaries.json" with {type:"json"}; console.log(pin["nats-server"].version)')"
nats_bin="${TRELLIS_TEST_NATS_BIN:-${TRELLIS_NATS_SERVER_BIN:-$root/target/test-nats/$nats_version/nats-server}}"
if [[ ! -x "$nats_bin" ]]; then
  if [[ -n "${TRELLIS_TEST_NATS_BIN:-}${TRELLIS_NATS_SERVER_BIN:-}" ]]; then
    echo "missing supplied NATS executable $nats_bin" >&2
    exit 1
  fi
  bash scripts/acquire-test-nats.sh --dest "$(dirname "$nats_bin")"
fi
export TRELLIS_TEST_NATS_BIN="$nats_bin"
export TRELLIS_NATS_SERVER_BIN="$nats_bin"

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
  live_application_versions
  empty_watch
  finite_rpc
  rpc_distribution
  operation_target
  operation_storage_upload
  operation_lease_loss
  device_companion
  transport_growth_subject
  transport_growth_operation
  transfer_generation
  stream_transfer
  jobs
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
    --config "patch.crates-io.trellis-rs.path=\"$root/crates/trellis\"" \
    --config "patch.crates-io.trellis-protocol.path=\"$root/crates/protocol\"" \
    --config "patch.crates-io.trellis-models.path=\"$root/crates/models\""
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
    --config "patch.crates-io.trellis-models.path=\"$root/crates/models\"" \
    --config "patch.crates-io.trellis-rs.path=\"$root/crates/trellis\"" \
    --config "patch.crates-io.trellis-testkit.path=\"$root/crates/trellis-testkit\"" \
    --test live \
    --message-format=json-render-diagnostics > "$json"
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
if [[ $# -eq 0 ]]; then
  set -- ts/integration/*_test.ts ts/integration/*.test.ts ts/packages/trellis/native/*.live_test.ts
fi

# Every shard receives the full test-file list; Node divides it into disjoint
# file partitions. Ordinary non-sharded local execution is unchanged.
shard_args=()
if [[ -n "${TRELLIS_TEST_SHARD:-}" ]]; then
  shard_args=("--test-shard=${TRELLIS_TEST_SHARD}")
  echo "Running TypeScript integration shard ${TRELLIS_TEST_SHARD} from $# test files"
fi
exec node --experimental-transform-types --test \
  --test-timeout=600000 \
  --test-concurrency="${TRELLIS_TEST_CONCURRENCY:-4}" \
  "${shard_args[@]}" "$@"
