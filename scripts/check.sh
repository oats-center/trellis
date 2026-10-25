#!/usr/bin/env bash
#
# The verification commands of every Check and Lint job. CI and local runs use
# this one script, so they cannot drift apart:
#
#   scripts/check.sh <job> [args]   run one job; each Check and Lint job calls this
#   npm test                        local: build once, run every job concurrently
#   npm test -- --all               also the packaging and container-image jobs
#
# In CI each job receives producer-built executables (CI is set). The local run
# builds debug executables, generated artifacts, embedded UIs and npm packages
# once, then runs the jobs in staged mode (TRELLIS_TEST_STAGED=1) so none of
# them rebuilds what another is using. Each job logs to target/test-logs/.
# Locally, sharded or split CI jobs run whole, with concurrency sized from the
# host's cores; TRELLIS_TEST_CONCURRENCY and TRELLIS_BROWSER_CONCURRENCY
# override it. TRELLIS_TEST_SKIP_PREPARE=1 skips contract and UI regeneration.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
temp="${RUNNER_TEMP:-$root/target/check-tmp}"

# The fast jobs `npm test` runs by default, and the slower packaging and
# container-image jobs `--all` adds.
fast_jobs=(lint clippy rust typescript live-rust live-typescript demos examples browser)
heavy_jobs=(rust-testkit typescript-testkit images)

# Packages are built once per runner; staged local jobs share one build.
build_packages() {
  [[ -n "${TRELLIS_TEST_STAGED:-}" ]] || npm run build:packages
}

# The native CLI and server archive, with its checksum, that packaging jobs
# consume. CI downloads the release build's into dist/native; locally
# pack_native packs the debug build, once per run when the jobs are staged.
native_archive_name() {
  local version
  version="$(grep -m1 '^version = ' Cargo.toml | cut -d '"' -f 2)"
  echo "trellis-${version}-x86_64-unknown-linux-gnu"
}

pack_native() {
  local name version
  name="$(native_archive_name)"
  version="${name#trellis-}"
  version="${version%-x86_64-unknown-linux-gnu}"
  mkdir -p dist/native
  tar -C "${1:-target/debug}" -czf "dist/native/$name.tar.gz" trellis trellis-server
  sha256sum "dist/native/$name.tar.gz" \
    > "dist/native/checksum-${version}-x86_64-unknown-linux-gnu-trellis.sha256"
}

native_archive() {
  [[ -n "${CI:-}${TRELLIS_TEST_STAGED:-}" ]] || pack_native
  local archive
  archive="dist/native/$(native_archive_name).tar.gz"
  [[ -s "$archive" ]] || { echo "missing native archive $archive" >&2; exit 1; }
}

# Runs every step even when an earlier one fails, so each failure reports.
each() {
  local status=0 step
  for step in "$@"; do
    echo "+ $step"
    eval "$step" || status=1
  done
  return "$status"
}

job_lint() {
  # rustfmt silently leaves statements it cannot fit unformatted, hence the
  # width check. Prettier honors nested .gitignore files too, so local build
  # output a package ignores is not checked.
  local ignores
  ignores="--ignore-path .gitignore$(find ts/packages ts/tools -name .gitignore \
    -not -path '*/node_modules/*' -printf ' --ignore-path %p')"
  each \
    "cargo fmt --manifest-path Cargo.toml --all --check" \
    "python3 scripts/check-rust-width.py" \
    "npm exec -- prettier $ignores --check ts/packages ts/tools" \
    "node docs/scripts/build_llms_full.ts --check"
}

job_clippy() {
  # Locally, other jobs build concurrently; a separate target directory keeps
  # this lint from holding their Cargo lock.
  [[ -z "${TRELLIS_TEST_STAGED:-}" ]] || export CARGO_TARGET_DIR="$root/target/clippy"
  cargo clippy --manifest-path Cargo.toml --workspace --all-targets \
    --features trellis-rs/event-store-sqlite,trellis-rs/event-store-postgres -- -D warnings
  for features in "" native-oauth device telemetry-otlp; do
    cargo clippy --manifest-path Cargo.toml -p trellis-rs --lib --no-default-features \
      --features "$features" -- -D warnings
  done
}

job_rust() {
  bash scripts/run-rust-tests.sh "$@"
}

job_typescript() {
  build_packages
  each \
    "npm run check --workspace @oatscenter/trellis" \
    "npm run check --workspace @oatscenter/trellis-testkit" \
    "npm run check:benchmarks" \
    "npm run check:integration" \
    "npm run check --workspace @trellis/console" \
    "npm run check --workspace @trellis/oauth-portal" \
    "bash scripts/run-sdk-tests.sh"
}

job_live_rust() {
  # Use the staged broker (CI extracts one beside the binaries), else the
  # pinned one, rather than whatever nats-server is on PATH.
  local nats="${TRELLIS_TEST_NATS_BIN:-$root/target/debug/nats/nats-server}" version
  if [[ ! -x "$nats" ]]; then
    version="$(python3 -c 'import json; print(json.load(open("crates/local-nats/nats-binaries.json"))["nats-server"]["version"])')"
    nats="$root/target/test-nats/$version/nats-server"
    [[ -x "$nats" ]] || bash scripts/acquire-test-nats.sh --dest "$(dirname "$nats")"
  fi
  TRELLIS_TEST_NATS_BIN="$nats" TRELLIS_TEST_SERVER_BIN="${TRELLIS_TEST_SERVER_BIN:-$root/target/debug/trellis-server}" \
    TRELLIS_TEST_CLI_BIN="${TRELLIS_TEST_CLI_BIN:-$root/target/debug/trellis}" \
    cargo test --manifest-path Cargo.toml -p trellis-rs --features live-integration \
    --test integration -- --nocapture
}

job_live_typescript() {
  bash scripts/run-ts-integration.sh "$@"
}

job_demos() {
  build_packages
  node --experimental-transform-types ts/tools/package_build/demo_consumers.ts
  bash scripts/release-ts-dry-run.sh
  local demo
  for demo in service device; do
    (
      cd "demos/rust/$demo"
      PATH="$root/target/debug:$PATH" trellis generate
      cargo check --locked --config "$root/.cargo/publication.toml"
      # The generated crate builds from a copy outside this repository, with no
      # Trellis CLI or API cache, as a consumer would.
      consumer="$(mktemp -d)"
      trap 'rm -rf "$consumer"' EXIT
      cp -R Cargo.toml src trellis "$consumer/"
      export PATH="$(dirname "$(rustup which cargo)"):/usr/bin:/bin"
      export TRELLIS_CACHE="$consumer/empty-api-cache"
      cd "$consumer"
      cargo check --config "$root/.cargo/publication.toml"
    )
  done
}

job_examples() {
  build_packages
  TRELLIS_TEST_SERVER_BIN="${TRELLIS_TEST_SERVER_BIN:-$root/target/debug/trellis-server}" \
    node ts/tools/package_build/orders_example.ts
}

job_browser() {
  bash scripts/run-browser-tests.sh "$@"
}

# Packages the public crates and stages the artifact-only verification bundle
# at $1, then (locally, or when CI passes --consume) verifies it from the
# bundle alone, without this checkout.
job_rust_testkit() {
  local output="${1:-$temp/rust-testkit-bundle.tar.gz}" version bundle
  python3 scripts/tests/test_rust_crate_contents.py
  native_archive
  version="$(grep -m1 '^version = ' Cargo.toml | cut -d '"' -f 2)"
  # Locally the pins describe the debug archive; restore the generated pins
  # afterwards so later builds of this checkout are unaffected.
  local pins=crates/trellis-testkit/generated/trellis-binaries.json
  if [[ -z "${CI:-}" ]]; then
    mkdir -p "$temp"
    cp "$pins" "$temp/trellis-binaries.json"
    trap 'cp "$temp/trellis-binaries.json" "$root/crates/trellis-testkit/generated/trellis-binaries.json"' EXIT
  fi
  python3 scripts/prepare-rust-native.py dist/native "$pins" "$version"
  cargo package --manifest-path Cargo.toml --allow-dirty --locked --no-verify \
    -p trellis-protocol -p trellis-models -p trellis-rs -p trellis-testkit \
    --config .cargo/publication.toml
  mkdir -p "$temp"
  local publication
  publication="$(mktemp -d "$temp/rust-publication.XXXXXX")"
  bash scripts/prepare-rust-publication.sh target/package "$publication" \
    trellis-protocol trellis-models trellis-rs trellis-testkit
  rm -rf "$publication"
  bundle="$temp/testkit-bundle"
  rm -rf "$bundle"
  mkdir -p "$bundle/packages" "$bundle/nats" "$bundle/native" "$bundle/consumer"
  cp target/package/trellis-protocol-*.crate target/package/trellis-models-*.crate \
    target/package/trellis-rs-*.crate target/package/trellis-testkit-*.crate "$bundle/packages/"
  tar -xzf dist/native/trellis-*-x86_64-unknown-linux-gnu.tar.gz -C "$bundle/native"
  cp -R integration/fixtures/testkit/package-consumer \
    integration/fixtures/testkit/tests integration/fixtures/testkit/crates "$bundle/consumer/"
  cp -R integration/fixtures/compiler-portable "$bundle/consumer/compiler-project"
  bash scripts/acquire-test-nats.sh --dest "$bundle/nats"
  cp scripts/verify-rust-test-package.sh "$bundle/"
  bash scripts/stage-rust-test-bundle.sh \
    --bundle "$bundle" \
    --output "$output" \
    --source-sha "${GITHUB_SHA:-$(git rev-parse HEAD)}" \
    --target "$(rustc -vV | sed -n 's/^host: //p')" \
    --version "$version"
  rm -rf "$bundle"
  # CI verifies the bundle in a separate job that has no checkout.
  [[ -n "${CI:-}" ]] || job_rust_testkit_consumer "$output"
}

job_rust_testkit_consumer() {
  local archive="${1:?bundle archive}" bundle
  mkdir -p "$temp"
  bundle="$(mktemp -d "$temp/testkit-consumer.XXXXXX")"
  tar -xzf "$archive" -C "$bundle" --no-same-owner
  bash "$bundle/verify-rust-test-package.sh" --bundle "$bundle" --mode full
  rm -rf "$bundle"
}

job_typescript_testkit() {
  native_archive
  # CI stages exactly the packages the demos job verified and uploaded.
  [[ -n "${CI:-}" ]] || build_packages
  node ts/tools/stage_testkit.ts dist/native dist/testkit-npm
  npm publish --dry-run --access public --tag rc ./dist/testkit-npm
  rm -rf dist/runtime
  mkdir -p dist/runtime
  tar -xzf dist/native/trellis-*-x86_64-unknown-linux-gnu.tar.gz -C dist/runtime
  TRELLIS_TEST_CLI_BIN="$PWD/dist/runtime/trellis" TRELLIS_TEST_SERVER_BIN="$PWD/dist/runtime/trellis-server" \
    node ts/tools/verify_testkit_package.ts dist/testkit-npm ts/packages/result/npm ts/packages/trellis/npm
  tar -C dist/testkit-npm -czf dist/testkit-npm.tar.gz .
}

# Builds the runtime and Console images and smoke-tests the runtime image's
# managed NATS. CI builds the images with its Buildx actions and runs only the
# smoke (`images --smoke-only`); locally Docker or Podman builds them.
job_images() {
  local engine
  engine="$(command -v docker || command -v podman || true)"
  [[ -n "$engine" ]] || { echo "images needs docker or podman" >&2; exit 1; }
  if [[ "${1:-}" != "--smoke-only" ]]; then
    # The image runs the binaries on its Debian base, so they come from the
    # same container release build Check uses, not the host's debug build.
    mkdir -p "$temp"
    rm -rf "$temp/release" dist/native/amd64
    "$engine" build --file crates/runtime/Containerfile.build \
      --output "type=local,dest=$temp/release" .
    mkdir -p dist/native/amd64
    cp "$temp/release/trellis" "$temp/release/trellis-server" dist/native/amd64/
    "$engine" build --file web/Containerfile --tag trellis-console-check:local .
    "$engine" build --file crates/runtime/Containerfile.release --tag trellis-smoke:test .
    "$engine" image rm trellis-console-check:local >/dev/null
  fi
  # The smoke script drives the engine through the `docker` command.
  local shim
  mkdir -p "$temp"
  shim="$(mktemp -d "$temp/engine.XXXXXX")"
  ln -s "$engine" "$shim/docker"
  PATH="$shim:$PATH" bash scripts/smoke-trellis-image.sh --image trellis-smoke:test
  rm -rf "$shim"
}

run_job() {
  local job="$1"
  shift
  local function="job_${job//-/_}"
  declare -F "$function" >/dev/null || { echo "unknown Check job: $job" >&2; exit 2; }
  "$function" "$@"
}

if [[ $# -gt 0 && "$1" != "--all" ]]; then
  run_job "$@"
  exit
fi

# Local run: every job, built once and run concurrently.
jobs=("${fast_jobs[@]}")
all="${1:-}"
[[ "$all" != "--all" ]] || jobs+=("${heavy_jobs[@]}")
logs="$root/target/test-logs"
# Start from empty logs so a failed run never shows an earlier run's results.
rm -rf "$logs"
mkdir -p "$logs"
started=$SECONDS
if [[ -z "${TRELLIS_TEST_SKIP_PREPARE:-}" ]]; then
  echo "preparing generated contracts, embedded UIs and npm packages"
  {
    cargo xtask install && npm run browser:embedded && npm run build:packages
  } > "$logs/prepare.log" 2>&1 \
    || { echo "preparation failed; see $logs/prepare.log" >&2; exit 1; }
fi
# Builds share Cargo's lock, so the runners build one at a time.
echo "building executables and test binaries"
{
  TRELLIS_TEST_BUILD_ONLY=1 bash scripts/run-rust-tests.sh
  TRELLIS_TEST_BUILD_ONLY=1 bash scripts/run-ts-integration.sh
  TRELLIS_TEST_BUILD_ONLY=1 bash scripts/run-sdk-tests.sh
  TRELLIS_TEST_BUILD_ONLY=1 bash scripts/run-browser-tests.sh
  cargo test --manifest-path Cargo.toml -p trellis-rs --features live-integration \
    --test integration --no-run
  [[ "$all" != "--all" ]] || pack_native
} > "$logs/build.log" 2>&1 || { echo "build failed; see $logs/build.log" >&2; exit 1; }
echo "built in $(( SECONDS - started ))s; running ${jobs[*]} concurrently"

declare -A pid
for job in "${jobs[@]}"; do
  TRELLIS_TEST_STAGED=1 bash "$root/scripts/check.sh" "$job" > "$logs/$job.log" 2>&1 &
  pid[$job]=$!
done
trap 'kill "${pid[@]}" 2>/dev/null || true' INT TERM

status=0
for job in "${jobs[@]}"; do
  if wait "${pid[$job]}"; then
    echo "pass  $job"
  else
    status=1
    echo "FAIL  $job (see $logs/$job.log)"
  fi
done
echo "finished in $(( SECONDS - started ))s"
exit "$status"
