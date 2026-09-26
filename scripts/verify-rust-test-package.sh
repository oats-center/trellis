#!/usr/bin/env bash
#
# Artifact-only verification for the public Rust testkit.
#
# Consumes the verification bundle produced by the release/check pipeline — a
# gzipped tarball (or an already-extracted directory) containing the three
# unmodified public `.crate` archives, matching CLI/server binaries, a verified
# NATS executable, the consumer fixture, and artifact-manifest.json. It validates
# the manifest, hashes, executable modes, and exact binary versions, extracts the
# archives with path-safety checks, patches only those three public artifacts into
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
for tool in cargo python3 tar; do
  command -v "$tool" >/dev/null 2>&1 || { echo "required tool not found: $tool" >&2; exit 1; }
done

work="$(mktemp -d)"
extracted=""
cleanup() { rm -rf "$work" ${extracted:+"$extracted"}; }
trap cleanup EXIT

# 0. Accept either a tarball or an already-extracted bundle directory.
if [[ -f "$bundle" ]]; then
  extracted="$(mktemp -d)"
  tar -xzf "$bundle" -C "$extracted" --no-same-owner
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
#    identity of the three public packages.
python3 - "$manifest" "$bundle" <<'PY'
import hashlib, json, os, sys

manifest_path, bundle = sys.argv[1], os.path.abspath(sys.argv[2])
try:
    manifest = json.load(open(manifest_path))
except json.JSONDecodeError as error:
    raise SystemExit(f"invalid artifact-manifest.json: {error}")

for key in ("source_sha", "target", "version", "packages", "files", "executables"):
    if key not in manifest:
        raise SystemExit(f"artifact-manifest.json is missing '{key}'")
if not manifest["source_sha"] or not manifest["version"]:
    raise SystemExit("artifact-manifest.json has an empty source_sha or version")
for package in ("trellis-protocol", "trellis-rs", "trellis-testkit"):
    if not manifest["packages"].get(package):
        raise SystemExit(f"artifact-manifest.json is missing the {package} version")


def sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


for relative, expected in manifest["files"].items():
    full = os.path.join(bundle, relative)
    if not os.path.isfile(full):
        raise SystemExit(f"artifact-manifest lists a missing file: {relative}")
    actual = sha256(full)
    if actual != expected:
        raise SystemExit(f"hash mismatch for {relative}: expected {expected}, got {actual}")

for relative in manifest["executables"]:
    full = os.path.join(bundle, relative)
    if not os.path.isfile(full):
        raise SystemExit(f"artifact-manifest lists a missing executable: {relative}")
    if os.stat(full).st_mode & 0o111 == 0:
        raise SystemExit(f"bundle executable is not executable: {relative}")

print(
    "artifact manifest verified:",
    f"source={manifest['source_sha']}",
    f"version={manifest['version']}",
    f"target={manifest['target']}",
)
PY

# 2. Require the bundle's own executables; never inherit ambient discovery.
unset TRELLIS_TEST_CLI_BIN TRELLIS_TEST_SERVER_BIN TRELLIS_TEST_NATS_BIN
export TRELLIS_TEST_CLI_BIN="$bundle/binaries/trellis"
export TRELLIS_TEST_SERVER_BIN="$bundle/binaries/trellis-server"
export TRELLIS_TEST_NATS_BIN="$bundle/nats/nats-server"

expected_version="$(python3 - "$manifest" <<'PY'
import json, sys
print(json.load(open(sys.argv[1]))["version"])
PY
)"
cli_version="$("$TRELLIS_TEST_CLI_BIN" --format json version | python3 -c 'import json,sys; print(json.load(sys.stdin)["version"])')"
server_version="$("$TRELLIS_TEST_SERVER_BIN" --version | awk '{print $NF}')"
if [[ "$cli_version" != "$expected_version" ]]; then
  echo "CLI version $cli_version does not match the bundle version $expected_version" >&2
  exit 1
fi
if [[ "$server_version" != "$expected_version" ]]; then
  echo "server version $server_version does not match the bundle version $expected_version" >&2
  exit 1
fi

# 3. Extract the three public archives with traversal/absolute-path checks.
mkdir -p "$work/packages"
python3 - "$packages_dir" "$work/packages" <<'PY'
import os, sys, tarfile
packages_dir, dest = sys.argv[1], sys.argv[2]
crates = sorted(f for f in os.listdir(packages_dir) if f.endswith(".crate"))
if len(crates) != 3:
    raise SystemExit(f"expected exactly three .crate files, found {crates}")
for name in crates:
    with tarfile.open(os.path.join(packages_dir, name)) as archive:
        for member in archive.getmembers():
            if member.name.startswith("/") or ".." in member.name.split("/"):
                raise SystemExit(f"unsafe path in {name}: {member.name}")
            if member.issym() or member.islnk():
                target = member.linkname
                if target.startswith("/") or ".." in target.split("/"):
                    raise SystemExit(f"unsafe link in {name}: {member.name} -> {target}")
        archive.extractall(dest, filter="data")
print("extracted", ", ".join(crates))
PY

protocol_dir="$(find "$work/packages" -maxdepth 1 -type d -name 'trellis-protocol-*' | head -1)"
rs_dir="$(find "$work/packages" -maxdepth 1 -type d -name 'trellis-rs-*' | head -1)"
test_dir="$(find "$work/packages" -maxdepth 1 -type d -name 'trellis-testkit-*' | head -1)"
[[ -n "$protocol_dir" && -n "$rs_dir" && -n "$test_dir" ]] || {
  echo "missing one of the extracted public crates" >&2
  exit 1
}

# The projected generated administration source must ship inside the testkit.
if ! tar tzf "$packages_dir"/trellis-testkit-*.crate | grep 'src/runtime_api/lib.rs' > /dev/null; then
  echo "trellis-testkit.crate is missing the generated administration projection" >&2
  exit 1
fi

# 4. Isolate the build and patch only the three extracted public artifacts.
#    Run from a directory with no producer Cargo configuration so nothing leaks in.
cp -R "$consumer_src" "$work/consumer"
config="$work/cargo-config.toml"
cat > "$config" <<EOF
[patch.crates-io]
trellis-protocol = { path = "$protocol_dir" }
trellis-rs = { path = "$rs_dir" }
trellis-testkit = { path = "$test_dir" }
EOF

export CARGO_HOME="$work/cargo-home"
export CARGO_TARGET_DIR="$work/target"
mkdir -p "$CARGO_HOME"
cd "$work"

# Resolve the staged lockfile once, then require it to stay locked.
cargo generate-lockfile --manifest-path "$work/consumer/Cargo.toml" --config "$config"
cargo metadata --manifest-path "$work/consumer/Cargo.toml" --format-version 1 \
  --locked --config "$config" > "$work/metadata.json"

# 5. Audit the full declared dependency graph: every manifest is a registry
#    package from crates.io, one of the three allowed public roots, or the
#    consumer's own application/generated fixture. Traverse normal, build, dev,
#    optional, and target-scoped declarations.
python3 - "$work/metadata.json" "$work/packages" "$work/consumer" <<'PY'
import json, os, sys
metadata, packages_root, consumer_root = sys.argv[1], os.path.abspath(sys.argv[2]), os.path.abspath(sys.argv[3])
forbidden = {
    "trellis-bootstrap", "trellis-local-bootstrap", "trellis-local-nats",
    "trellis-runtime", "trellis-runtime-apis", "trellis-events-runtime",
    "trellis-jobs-runtime", "trellis-idl", "trellis-codegen-rust",
    "trellis-codegen-ts", "trellis-cli", "trellis-server", "xtask",
}
crates_io = ("registry+https://github.com/rust-lang/crates.io-index", "registry+https://index.crates.io/")


def allowed_root(path):
    path = os.path.abspath(path)
    return path.startswith(packages_root + os.sep) or path.startswith(consumer_root + os.sep)


def check_source(context, source):
    if source is None:
        return
    if source.startswith("git+"):
        raise SystemExit(f"git dependency in the testkit closure: {context} ({source})")
    if not any(source.startswith(root) for root in crates_io):
        raise SystemExit(f"non-crates.io registry dependency: {context} ({source})")


data = json.load(open(metadata))
for package in data["packages"]:
    name = package["name"]
    manifest = os.path.abspath(package["manifest_path"])
    if name in forbidden:
        raise SystemExit(f"forbidden package in the testkit closure: {name}")
    source = package.get("source")
    if source is None:
        if not allowed_root(manifest):
            raise SystemExit(f"local package outside the allowed roots: {name} ({manifest})")
    else:
        check_source(name, source)
    for dependency in package.get("dependencies", []):
        declared_path = dependency.get("path")
        if declared_path is not None and not allowed_root(declared_path):
            raise SystemExit(f"path dependency outside the allowed roots: {name} -> {declared_path}")
        check_source(f"{name} -> {dependency.get('name')}", dependency.get("source"))
print("dependency audit passed for", len(data["packages"]), "packages")
PY

# 6. Run the consumer's live target under the frozen lockfile.
#
# Run the tests one at a time: a small hosted runner (e.g. ubuntu-latest, four
# CPUs) cannot host several tests' runtimes at once without starving the
# servers, and their admission deadlines then fire.
#
# t07's eight concurrent runtimes are skipped here regardless of mode. They need
# more CPU than a small hosted runner provides, and they still run under normal
# parallel execution in the `Live integration` job, which uses the same
# packaged sources.
cargo test --manifest-path "$work/consumer/Cargo.toml" --locked --config "$config" \
  --test live -- --test-threads=1 --skip t07_eight_concurrent_runtimes_are_isolated
