#!/usr/bin/env bash
#
# The verification commands of every pipeline job. CI and local runs use
# this one script, so they cannot drift apart:
#
#   scripts/check.sh <job> [args]   run one job; each pipeline job calls this
#   scripts/check.sh quick          local: CI's fast checking jobs, in order
#   npm test                        local: build once, run every job concurrently
#   npm test -- --all               also the packaging and container-image jobs
#
# In CI each job receives producer-built executables (CI is set). The local run
# builds debug executables, generated artifacts, embedded UIs and npm packages
# once, copies the executables to target/check-bin, then runs the jobs in
# staged mode (TRELLIS_TEST_STAGED=1) against those copies, as CI jobs run the
# producer's: the rust job's Cargo still re-links target/debug, so no job
# executes from there. Each job logs to target/test-logs/.
# Test files run concurrently, sized from the host's cores;
# TRELLIS_TEST_CONCURRENCY and TRELLIS_BROWSER_CONCURRENCY
# override it. TRELLIS_TEST_SKIP_PREPARE=1 skips contract and UI regeneration.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
temp="${RUNNER_TEMP:-$root/target/check-tmp}"
# Temporary files never go to the system /tmp: every job and the scripts it
# runs use a caller's TMPDIR, else this run's temp directory.
export TMPDIR="${TMPDIR:-$temp}"
mkdir -p "$TMPDIR"

# The fast jobs `npm test` runs by default, and the slower packaging, upgrade
# and container-image jobs `--all` adds.
fast_jobs=(lint clippy rust typescript-checks typescript-tests testkit-tests demos examples browser)
heavy_jobs=(rust-testkit typescript-testkit upgrade images)

# Packages are built once per runner; staged local jobs share one build.
# Every job that uses them first makes a worktree resolve its own packages.
build_packages() {
  node ts/tools/link_checkout_modules.ts
  [[ -n "${TRELLIS_TEST_STAGED:-}" ]] || npm run build:packages
}

# The native CLI and server archives, one per binary with its checksum, that
# packaging jobs consume. CI downloads the producer's archives into dist/native;
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
    "cargo deny check" \
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

# The integration fixture helpers, the health_watch example and the testkit
# test binaries, built into target/debug for the jobs that run them.
job_build_fixtures() {
  mkdir -p "$temp"
  CARGO_TARGET_DIR="$root/target" cargo build --bins --example health_watch --message-format=json-render-diagnostics \
    --manifest-path integration/fixtures/runtime/Cargo.toml \
    --config "patch.crates-io.trellis-rs.path=\"$root/crates/trellis\"" \
    --config "patch.crates-io.trellis-protocol.path=\"$root/crates/protocol\"" \
    --config "patch.crates-io.trellis-models.path=\"$root/crates/models\"" > "$temp/runtime-fixtures.json"
  if [[ -n "${1:-}" ]]; then
    mkdir -p "$1"
    while IFS= read -r executable; do
      cp "$executable" "$1/"
    done < <(jq -r 'select(.executable != null and .target.kind == ["bin"]) | .executable' "$temp/runtime-fixtures.json")
  fi
  CARGO_TARGET_DIR="$root/target" cargo test --no-run \
    --manifest-path integration/fixtures/testkit/Cargo.toml \
    --config "patch.crates-io.trellis-protocol.path=\"$root/crates/protocol\"" \
    --config "patch.crates-io.trellis-models.path=\"$root/crates/models\"" \
    --config "patch.crates-io.trellis-rs.path=\"$root/crates/trellis\"" \
    --config "patch.crates-io.trellis-testkit.path=\"$root/crates/trellis-testkit\"" \
    --tests \
    --message-format=json > "$temp/testkit-tests.json"
  mkdir -p target/debug/testkit-tests
  local name executable
  while read -r name executable; do
    if [[ "$name" == live ]]; then
      cp "$executable" target/debug/trellis-testkit-live
    else
      cp "$executable" "target/debug/testkit-tests/$name"
    fi
  done < <(jq -r 'select(.executable != null and .target.kind[0] == "test") |
    "\(.target.name) \(.executable)"' "$temp/testkit-tests.json")
  [[ -x target/debug/trellis-testkit-live ]] || {
    echo "failed to locate the testkit live test binary" >&2
    exit 1
  }
}

# CI installs the current stable Rust; another local compiler can pass lints
# that CI rejects, or the reverse. This warns rather than fails: the repository
# supports current stable, and a local toolchain may lag it.
rust_toolchain_warning() {
  local stable local_version
  stable="$(curl -fsS --max-time 10 https://static.rust-lang.org/dist/channel-rust-stable.toml \
    2>/dev/null | sed -n '/^\[pkg\.rust\]/,/^version/s/^version = "\([0-9.]*\).*/\1/p')" || true
  local_version="$(rustc --version | cut -d ' ' -f 2)"
  if [[ -z "$stable" ]]; then
    echo "warning: could not resolve the current stable Rust that CI uses; this run uses rustc $local_version" >&2
  elif [[ "$stable" != "$local_version" ]]; then
    echo "warning: CI uses Rust $stable (current stable) but this run uses rustc $local_version, so lints can differ." >&2
    echo "warning: rerun with: RUSTUP_TOOLCHAIN=$stable bash scripts/check.sh quick" >&2
    echo "warning: (install it with: rustup toolchain install $stable --target wasm32-unknown-unknown)" >&2
  fi
}

# CI's fast checking jobs, after the same preparation CI's install job runs.
# Each job runs as its own process and reports even when an earlier one fails.
job_quick() {
  rust_toolchain_warning
  echo "+ cargo xtask install && npm run browser:embedded"
  cargo xtask install
  npm run browser:embedded
  local job status=0 results=()
  for job in lint clippy typescript-checks build-fixtures; do
    echo "== $job"
    if bash "$root/scripts/check.sh" "$job"; then
      results+=("pass  $job")
    else
      status=1
      results+=("FAIL  $job")
    fi
  done
  printf '%s\n' "${results[@]}"
  rust_toolchain_warning
  return "$status"
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

# Integration files, plus the packages' own suites.
job_typescript_tests() {
  build_packages
  bash scripts/run-sdk-tests.sh
  bash scripts/run-ts-integration.sh "$@"
}

job_testkit_tests() {
  bash scripts/run-testkit-tests.sh "$@"
}

# Starts the candidate server on an installation the previous published release
# made, and moves such an installation into the candidate with
# `trellis-server export` and `import`. CI passes the producer-built server, NATS and test binary; elsewhere the
# server is built here and the test as the build-fixtures job builds it. The
# test lists releases through the GitHub API; GITHUB_TOKEN lifts its rate limit.
job_upgrade() {
  local tests="${TRELLIS_TESTKIT_TESTS_DIR:-$root/target/debug/testkit-tests}"
  if [[ -z "${CI:-}${TRELLIS_TEST_STAGED:-}" ]]; then
    cargo build -p trellis-server
    job_build_fixtures
  fi
  local nats_version nats_bin
  nats_version="$(jq -r '.["nats-server"].version' crates/local-nats/nats-binaries.json)"
  nats_bin="${TRELLIS_TEST_NATS_BIN:-$root/target/test-nats/$nats_version/nats-server}"
  [[ -x "$nats_bin" ]] || bash scripts/acquire-test-nats.sh --dest "$(dirname "$nats_bin")"
  export TRELLIS_TEST_NATS_BIN="$nats_bin"
  export TRELLIS_TEST_SERVER_BIN="${TRELLIS_TEST_SERVER_BIN:-$root/target/debug/trellis-server}"
  export TRELLIS_TEST_CACHE_DIR="${TRELLIS_TEST_CACHE_DIR:-$temp/trellis-testkit-cache}"
  mkdir -p -m 700 "$TRELLIS_TEST_CACHE_DIR"
  "$tests/upgrade" --test-threads=2 "$@"
  "$tests/installation_move" --test-threads=2 "$@"
}

job_demos() {
  build_packages
  node ts/tools/package_build/demo_consumers.ts
  bash scripts/release-ts-dry-run.sh
  local demo
  for demo in service device; do
    (
      # Both source trees stay independent and Cargo fingerprints their paths.
      # Reuse registry dependencies across locked and fresh consumer builds.
      export CARGO_TARGET_DIR="$root/target/demo-consumers"
      cd "demos/rust/$demo"
      PATH="$(dirname "${TRELLIS_TEST_CLI_BIN:-$root/target/debug/trellis}"):$PATH" trellis generate
      cargo check --locked --config "$root/.cargo/publication.toml"
      # The generated crate builds from a copy outside this repository, with no
      # Trellis CLI or API cache, as a consumer would.
      mkdir -p "$temp"
      consumer="$(mktemp -d "$temp/demo-consumer.XXXXXX")"
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
  # CI installs exactly the tarballs the demos job packed and uploaded.
  local packages
  if [[ -n "${CI:-}" ]]; then
    packages=(dist/npm/oatscenter-trellis-testkit-*.tgz dist/npm/oatscenter-result-*.tgz
      dist/npm/oatscenter-trellis-[0-9]*.tgz)
  else
    build_packages
    packages=(ts/packages/trellis-testkit/npm ts/packages/result/npm ts/packages/trellis/npm)
  fi
  # Outside the repository, which concurrent image builds copy as their context.
  local runtime
  mkdir -p "$temp"
  runtime="$(mktemp -d "$temp/testkit-runtime.XXXXXX")"
  native_archive "$runtime"
  TRELLIS_TEST_CLI_BIN="$runtime/trellis" TRELLIS_TEST_SERVER_BIN="$runtime/trellis-server" \
    node ts/tools/verify_testkit_package.ts "${packages[@]}"
}

# Builds the runtime and Console images and smoke-tests the runtime image
# beside a NATS container. CI builds the images with its Buildx actions and runs only the
# smoke (`images --smoke-only`); locally Docker or Podman builds them.
job_images() {
  local engine
  engine="$(command -v docker || command -v podman || true)"
  [[ -n "$engine" ]] || { echo "images needs docker or podman" >&2; exit 1; }
  if [[ "${1:-}" != "--smoke-only" ]]; then
    # Exercise the same bookworm development binaries as ordinary CI.
    local goarch
    case "$(uname -m)" in x86_64) goarch=amd64 ;; aarch64 | arm64) goarch=arm64 ;; *) goarch="$(uname -m)" ;; esac
    mkdir -p "$temp"
    rm -rf "$temp/release" "dist/native/$goarch"
    "$engine" build --file crates/runtime/Containerfile.build \
      --build-arg BUILD_PROFILE=dev --build-arg CARGO_INCREMENTAL=1 \
      --output "type=local,dest=$temp/release" .
    mkdir -p "dist/native/$goarch"
    cp "$temp/release/trellis" "$temp/release/trellis-server" "dist/native/$goarch/"
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

job_prepare_tests() {
  local staged="${1:-$root/target/debug}"
  cargo build -p trellis-cli --bin trellis -p trellis-server --bin trellis-server
  job_build_fixtures "$staged/fixtures"
  TRELLIS_TEST_EVENTS_BIN="$staged/fixtures/events" TRELLIS_TEST_STAGED=1 TRELLIS_TEST_BUILD_ONLY=1 bash scripts/run-rust-tests.sh
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
echo "building executables and test binaries"
staged="$root/target/check-bin"
rm -rf "$staged"
mkdir -p "$staged/examples"
bash "$root/scripts/check.sh" prepare-tests "$staged" > "$logs/build.log" 2>&1 \
  || { echo "build failed; see $logs/build.log" >&2; exit 1; }
# The jobs run copies, as CI jobs run the producer's executables: the rust job
# still builds into target/debug and re-links what it finds stale there.
cp target/debug/trellis target/debug/trellis-server target/debug/trellis-testkit-live "$staged/"
cp target/debug/examples/health_watch "$staged/examples/"
cp -R target/debug/testkit-tests "$staged/"
export TRELLIS_TEST_CLI_BIN="$staged/trellis" TRELLIS_TEST_SERVER_BIN="$staged/trellis-server"
export TRELLIS_TEST_EVENTS_BIN="$staged/fixtures/events" TRELLIS_TEST_FIXTURE_BIN_DIR="$staged/fixtures"
export TRELLIS_TESTKIT_LIVE_BIN="$staged/trellis-testkit-live" TRELLIS_TESTKIT_TESTS_DIR="$staged/testkit-tests"
export TRELLIS_TEST_HEALTH_WATCH_BIN="$staged/examples/health_watch"
[[ "$all" != "--all" ]] || pack_native "$staged"
echo "built in $(( SECONDS - started ))s; running ${jobs[*]} concurrently"

# Concurrent local suites share a host. Nested consumer builds and Chromium
# also start workers of their own. Standalone jobs keep their usual parallelism.
jobs_per_suite=$(( ($(nproc) + 7) / 8 ))
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-$jobs_per_suite}"
export NEXTEST_TEST_THREADS="${NEXTEST_TEST_THREADS:-$jobs_per_suite}"
export TRELLIS_BROWSER_CONCURRENCY="${TRELLIS_BROWSER_CONCURRENCY:-$jobs_per_suite}"
integration_jobs="${TRELLIS_TEST_CONCURRENCY:-$(( ($(nproc) + 3) / 4 ))}"
export TRELLIS_TEST_CONCURRENCY="${TRELLIS_TEST_CONCURRENCY:-$jobs_per_suite}"
# Live integration spends much of its wall time waiting for broker events and
# authorization lifetimes. Give it more slots than CPU-heavy SDK and Rust jobs.
export TRELLIS_INTEGRATION_CONCURRENCY="${TRELLIS_INTEGRATION_CONCURRENCY:-$integration_jobs}"

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
