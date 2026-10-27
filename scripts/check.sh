#!/usr/bin/env bash
#
# The verification commands of every pipeline job. CI and local runs use
# this one script, so they cannot drift apart:
#
#   scripts/check.sh <job> [args]   run one job; each pipeline job calls this
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
fast_jobs=(lint clippy rust typescript-checks typescript-tests testkit-tests demos examples browser)
heavy_jobs=(rust-testkit typescript-testkit images)

# Packages are built once per runner; staged local jobs share one build.
build_packages() {
  [[ -n "${TRELLIS_TEST_STAGED:-}" ]] || npm run build:packages
}

# The native CLI and server archives, one per binary with its checksum, that
# packaging jobs consume. CI downloads the release build's into dist/native;
# locally pack_native packs the debug build, once per run when the jobs are
# staged.
native_version() {
  grep -m1 '^version = ' Cargo.toml | cut -d '"' -f 2
}

pack_native() {
  local version binary archive
  version="$(native_version)"
  mkdir -p dist/native
  for binary in trellis trellis-server; do
    archive="${binary}-${version}-x86_64-unknown-linux-gnu.tar.gz"
    tar -C "${1:-target/debug}" -czf "dist/native/$archive" "$binary"
    (cd dist/native && sha256sum "$archive" \
      > "checksum-${version}-x86_64-unknown-linux-gnu-${binary}.sha256")
  done
}

# Extracts both native binaries into the directory $1.
native_archive() {
  [[ -n "${CI:-}${TRELLIS_TEST_STAGED:-}" ]] || pack_native
  local binary archive
  for binary in trellis trellis-server; do
    archive="dist/native/${binary}-$(native_version)-x86_64-unknown-linux-gnu.tar.gz"
    [[ -s "$archive" ]] || { echo "missing native archive $archive" >&2; exit 1; }
    [[ -z "${1:-}" ]] || tar -xzf "$archive" -C "$1"
  done
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
  # Prettier honors nested .gitignore files too, so local build
  # output a package ignores is not checked.
  local ignores
  ignores="--ignore-path .gitignore$(find ts/packages ts/tools -name .gitignore \
    -not -path '*/node_modules/*' -printf ' --ignore-path %p')"
  each \
    "cargo fmt --manifest-path Cargo.toml --all --check" \
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
  # rustfmt silently leaves statements it cannot fit unformatted. xtask is
  # already compiled here, unlike in the uncached formatting job.
  cargo xtask rust-width
}

job_rust() {
  bash scripts/run-rust-tests.sh "$@"
}

job_typescript_checks() {
  build_packages
  each \
    "npm run check --workspace @oatscenter/trellis" \
    "npm run check --workspace @oatscenter/trellis-testkit" \
    "npm run check:benchmarks" \
    "npm run check:integration" \
    "npm run check --workspace @trellis/console" \
    "npm run check --workspace @trellis/oauth-portal"
}

# Integration files, plus the packages' own suites. A CI shard runs only its
# share of the integration files; the first shard also runs the package suites.
job_typescript_tests() {
  if [[ "${TRELLIS_TEST_SHARD:-1/1}" == 1/* ]]; then
    build_packages
    bash scripts/run-sdk-tests.sh
  fi
  bash scripts/run-ts-integration.sh "$@"
}

job_testkit_tests() {
  bash scripts/run-testkit-tests.sh "$@"
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
# Each public crate's Rust API must fit its version bump against the latest
# stable release on crates.io; a release candidate is never the baseline.
semver_checks() {
  local crate baseline
  for crate in trellis-protocol trellis-models trellis-rs trellis-testkit; do
    baseline="$(curl -fsSL -A "trellis-check (https://github.com/oats-center/trellis)" \
      "https://crates.io/api/v1/crates/$crate/versions" |
      jq -r '[.versions[] | select(.yanked | not) | .num | select(contains("-") | not)][0] // empty')"
    if [[ -z "$baseline" ]]; then
      echo "$crate has no stable release to compare against"
      continue
    fi
    cargo semver-checks check-release -p "$crate" --baseline-version "$baseline"
  done
}

job_rust_testkit() {
  local output="${1:-$temp/rust-testkit-bundle.tar.gz}" version bundle
  semver_checks
  version="$(grep -m1 '^version = ' Cargo.toml | cut -d '"' -f 2)"
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
  # Exactly the versions just packaged; target/package keeps earlier ones.
  cargo metadata --format-version 1 --no-deps |
    jq -r '.packages[] | select(.name | IN("trellis-protocol", "trellis-models",
      "trellis-rs", "trellis-testkit")) | "target/package/\(.name)-\(.version).crate"' |
    xargs cp -t "$bundle/packages/"
  native_archive "$bundle/native"
  cp -R integration/fixtures/testkit/package-consumer \
    integration/fixtures/testkit/tests integration/fixtures/testkit/crates "$bundle/consumer/"
  cp -R integration/fixtures/compiler-portable "$bundle/consumer/compiler-project"
  bash scripts/acquire-test-nats.sh --dest "$bundle/nats"
  cp scripts/verify-rust-test-package.sh "$bundle/"
  cargo xtask release stage-testkit-bundle \
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
  bash "$bundle/verify-rust-test-package.sh" --bundle "$bundle"
  rm -rf "$bundle"
}

job_typescript_testkit() {
  # CI uses exactly the packages the demos job verified and uploaded.
  [[ -n "${CI:-}" ]] || build_packages
  # Outside the repository, which concurrent image builds copy as their context.
  local runtime
  mkdir -p "$temp"
  runtime="$(mktemp -d "$temp/testkit-runtime.XXXXXX")"
  native_archive "$runtime"
  TRELLIS_TEST_CLI_BIN="$runtime/trellis" TRELLIS_TEST_SERVER_BIN="$runtime/trellis-server" \
    node ts/tools/verify_testkit_package.ts ts/packages/trellis-testkit/npm \
    ts/packages/result/npm ts/packages/trellis/npm
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
    # same container release build the pipeline uses, not the host's debug build.
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
  declare -F "$function" >/dev/null || { echo "unknown pipeline job: $job" >&2; exit 2; }
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
  TRELLIS_TEST_BUILD_ONLY=1 bash scripts/run-testkit-tests.sh
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
