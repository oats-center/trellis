#!/usr/bin/env bash
#
# Artifact-only verification for the public Rust testkit.
#
# Consumes the verification bundle produced by the release/check pipeline — a
# gzipped tarball (or an already-extracted directory) containing the complete
# four public Rust crates, verified CLI/server/NATS
# executables, the consumer fixture, and artifact-manifest.json. It validates
# the manifest, hashes, and executable modes, extracts the
# archives with path-safety checks, patches the staged artifacts into
# a fresh isolated consumer build, audits the full declared dependency graph, and
# runs the fixture's live target under `--locked`. No Trellis source checkout is
# required or used.
#
# Usage:
#   verify-rust-test-package.sh --bundle ABSOLUTE_BUNDLE_DIR_OR_TARBALL --mode smoke
#   verify-rust-test-package.sh --bundle ABSOLUTE_BUNDLE_DIR_OR_TARBALL --mode full
set -euo pipefail

bundle=""
mode=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --bundle)
      bundle="${2:-}"
      shift 2
      ;;
    --mode)
      mode="${2:-}"
      shift 2
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
done
[[ -n "$bundle" ]] || { echo "--bundle is required" >&2; exit 2; }
[[ "$mode" == "smoke" || "$mode" == "full" ]] || { echo "--mode must be smoke or full" >&2; exit 2; }
for tool in cargo jq tar sha256sum; do
  command -v "$tool" >/dev/null 2>&1 || { echo "required tool not found: $tool" >&2; exit 1; }
done

# Extract an archive after rejecting absolute or parent-relative entries and
# links that point outside the destination.
safe_extract() {
  local archive="$1" dest="$2" name line target
  while IFS= read -r name; do
    if [[ "$name" == /* || "/$name/" == */../* ]]; then
      echo "unsafe path in $(basename "$archive"): $name" >&2
      exit 1
    fi
  done < <(tar -tzf "$archive")
  while IFS= read -r line; do
    target="${line##* -> }"
    [[ "$line" != *" link to "* ]] || target="${line##* link to }"
    if [[ "$target" == /* || "/$target/" == */../* ]]; then
      echo "unsafe link in $(basename "$archive"): $target" >&2
      exit 1
    fi
  done < <(tar -tvzf "$archive" | grep -E '^[lh]' || true)
  tar -xzf "$archive" -C "$dest" --no-same-owner
}

work="$(mktemp -d)"
extracted=""
cleanup() { rm -rf "$work" ${extracted:+"$extracted"}; }
trap cleanup EXIT

# 0. Accept either a tarball or an already-extracted bundle directory.
if [[ -f "$bundle" ]]; then
  extracted="$(mktemp -d)"
  safe_extract "$bundle" "$extracted"
  bundle="$extracted"
fi
[[ -d "$bundle" ]] || { echo "bundle not found: $bundle" >&2; exit 2; }
bundle="$(cd "$bundle" && pwd)"

packages_dir="$bundle/packages"
consumer_src="$bundle/consumer"
manifest="$bundle/artifact-manifest.json"
[[ -d "$packages_dir" ]] || { echo "missing $packages_dir" >&2; exit 1; }
[[ -d "$consumer_src" ]] || { echo "missing $consumer_src" >&2; exit 1; }
[[ -f "$manifest" ]] || { echo "missing $manifest" >&2; exit 1; }

# 1. Validate the artifact manifest, every recorded hash and mode, and the
#    identity of the author-facing roots.
jq -e . "$manifest" >/dev/null 2>&1 || { echo "invalid artifact-manifest.json" >&2; exit 1; }
for key in source_sha target version packages files executables; do
  jq -e --arg key "$key" 'has($key)' "$manifest" >/dev/null ||
    { echo "artifact-manifest.json is missing '$key'" >&2; exit 1; }
done
[[ -n "$(jq -r '.source_sha // ""' "$manifest")" && -n "$(jq -r '.version // ""' "$manifest")" ]] ||
  { echo "artifact-manifest.json has an empty source_sha or version" >&2; exit 1; }
for package in trellis-protocol trellis-models trellis-rs trellis-testkit; do
  [[ -n "$(jq -r --arg package "$package" '.packages[$package] // ""' "$manifest")" ]] ||
    { echo "artifact-manifest.json is missing the $package version" >&2; exit 1; }
done
while IFS=$'\t' read -r relative expected; do
  [[ -f "$bundle/$relative" ]] || { echo "artifact-manifest lists a missing file: $relative" >&2; exit 1; }
  actual="$(sha256sum "$bundle/$relative" | cut -d ' ' -f 1)"
  [[ "$actual" == "$expected" ]] ||
    { echo "hash mismatch for $relative: expected $expected, got $actual" >&2; exit 1; }
done < <(jq -r '.files | to_entries[] | "\(.key)\t\(.value)"' "$manifest")
while IFS= read -r relative; do
  [[ -f "$bundle/$relative" ]] ||
    { echo "artifact-manifest lists a missing executable: $relative" >&2; exit 1; }
  [[ -x "$bundle/$relative" ]] ||
    { echo "bundle executable is not executable: $relative" >&2; exit 1; }
done < <(jq -r '.executables[]' "$manifest")
echo "artifact manifest verified: $(jq -r '"source=\(.source_sha) version=\(.version) target=\(.target)"' "$manifest")"

# 2. Require the bundle's own executables; never inherit ambient discovery.
unset TRELLIS_TEST_CLI_BIN TRELLIS_TEST_SERVER_BIN TRELLIS_TEST_NATS_BIN
export TRELLIS_TEST_NATS_BIN="$bundle/nats/nats-server"
export TRELLIS_TEST_CLI_BIN="$bundle/native/trellis"
export TRELLIS_TEST_SERVER_BIN="$bundle/native/trellis-server"
if [[ "$mode" == smoke ]]; then
  unset TRELLIS_TEST_CLI_BIN TRELLIS_TEST_SERVER_BIN TRELLIS_TEST_NATS_BIN
  export TRELLIS_TEST_CACHE_DIR="$work/native-cache"
fi

# 3. Extract the staged archives with traversal/absolute-path checks.
mkdir -p "$work/packages"
shopt -s nullglob
crates=("$packages_dir"/*.crate)
shopt -u nullglob
(( ${#crates[@]} )) || { echo "no staged .crate files" >&2; exit 1; }
for crate in "${crates[@]}"; do
  safe_extract "$crate" "$work/packages"
done
echo "extracted ${crates[*]##*/}"

# 4. Isolate the build and patch every extracted artifact.
#    Run from a directory with no producer Cargo configuration so nothing leaks in.
cp -R "$consumer_src" "$work/consumer"
config="$work/cargo-config.toml"
printf '[patch.crates-io]\n' > "$config"
declare -A staged=()
for root in "$work/packages"/*; do
  [[ -d "$root" ]] || { echo "unexpected archive root: $root" >&2; exit 1; }
  IFS=$'\t' read -r name version < <(
    cargo metadata --no-deps --offline --format-version 1 --manifest-path "$root/Cargo.toml" |
      jq -r '.packages[0] | "\(.name)\t\(.version)"'
  )
  [[ "$(jq -r --arg name "$name" '.packages[$name] // ""' "$manifest")" == "$version" ]] ||
    { echo "archive identity does not match artifact manifest: $name" >&2; exit 1; }
  [[ -z "${staged[$name]:-}" ]] || { echo "duplicate staged package: $name" >&2; exit 1; }
  staged[$name]="$root"
  printf '%s = { path = %s }\n' "$(jq -Rn --arg value "$name" '$value')" \
    "$(jq -Rn --arg value "$root" '$value')" >> "$config"
done

export CARGO_HOME="$work/cargo-home"
export CARGO_TARGET_DIR="$work/target"
mkdir -p "$CARGO_HOME"
cd "$work"
consumer_manifest="$work/consumer/package-consumer/Cargo.toml"

# Resolve the staged lockfile once, then require it to stay locked.
cargo generate-lockfile --manifest-path "$consumer_manifest" --config "$config"
cargo metadata --manifest-path "$consumer_manifest" --format-version 1 \
  --locked --config "$config" > "$work/metadata.json"

# 5. Audit the full declared dependency graph: every manifest is a registry
#    package from crates.io, one of the staged artifacts, or the
#    consumer's own application/generated fixture. Traverse normal, build, dev,
#    optional, and target-scoped declarations.
packages_root="$(cd "$work/packages" && pwd -P)/"
consumer_root="$(cd "$work/consumer" && pwd -P)/"
jq -r --arg packages "$packages_root" --arg consumer "$consumer_root" '
  def allowed: startswith($packages) or startswith($consumer);
  def registry($context):
    if . == null then empty
    elif startswith("git+") then "git dependency in the testkit closure: \($context) (\(.))"
    elif (startswith("registry+https://github.com/rust-lang/crates.io-index")
      or startswith("registry+https://index.crates.io/")) | not
    then "non-crates.io registry dependency: \($context) (\(.))"
    else empty end;
  .packages[] as $package
  | (if ($package.name | startswith("trellis-")) and $package.source != null
      then "Trellis dependency was not staged: \($package.name)" else empty end),
    (if $package.source == null
      then (if $package.manifest_path | allowed then empty
        else "local package outside the allowed roots: \($package.name) (\($package.manifest_path))" end)
      else ($package.source | registry($package.name)) end),
    ($package.dependencies[] as $dependency
      | (if $dependency.path != null and ($dependency.path | allowed | not)
          then "path dependency outside the allowed roots: \($package.name) -> \($dependency.path)"
          else empty end),
        ($dependency.source | registry("\($package.name) -> \($dependency.name)")))
' "$work/metadata.json" > "$work/audit.txt"
if [[ -s "$work/audit.txt" ]]; then
  head -n1 "$work/audit.txt" >&2
  exit 1
fi
echo "dependency audit passed for $(jq '.packages | length' "$work/metadata.json") packages"

# Generate and execute a fresh Rust consumer with the distributed CLI. This job
# has no source checkout, including the compiler producer's built-in IDL tree.
if [[ "$mode" == full ]]; then
  cp -R "$consumer_src/compiler-project" "$work/compiler-project"
  (
    cd "$work/compiler-project"
    export TRELLIS_CACHE="$work/compiler-cache"
    "$TRELLIS_TEST_CLI_BIN" update
    "$TRELLIS_TEST_CLI_BIN" generate
    cargo run --config "$config"
  )
fi

# 6. Run the consumer's live target under the frozen lockfile.
#
# Independent cases run four at a time, as the in-repo testkit suite does.
cargo test --manifest-path "$consumer_manifest" --locked --config "$config" \
  --test live --no-run --message-format=json-render-diagnostics > "$work/build.json"
mkdir -p "$work/runtime-path"
executable="$(jq -r 'select(.reason == "compiler-artifact" and .executable != null) | .executable' \
  "$work/build.json" | tail -n1)"
[[ -n "$executable" ]] || { echo "the live consumer executable was not produced" >&2; exit 1; }
# No tools on PATH. Full acceptance uses verified explicit executables; release
# smoke acquires public archives into a fresh cache and then proves offline reuse.
arguments=(--test-threads=4)
[[ "$mode" != smoke ]] ||
  arguments+=(--exact bare_builder_serves_embedded_apps_and_installs_a_participant)
PATH="$work/runtime-path" "$executable" "${arguments[@]}"
if [[ "$mode" == smoke ]]; then
  proxy=http://127.0.0.1:9
  local_hosts=127.0.0.1,localhost
  HTTP_PROXY="$proxy" HTTPS_PROXY="$proxy" ALL_PROXY="$proxy" NO_PROXY="$local_hosts" \
    http_proxy="$proxy" https_proxy="$proxy" all_proxy="$proxy" no_proxy="$local_hosts" \
    PATH="$work/runtime-path" "$executable" "${arguments[@]}"
fi
