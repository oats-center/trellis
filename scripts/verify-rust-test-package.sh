#!/usr/bin/env bash
#
# Artifact-only verification for the public Rust testkit.
#
# Consumes a verification bundle (produced by the release/check pipeline) that
# contains the three unmodified public `.crate` archives, matching CLI/server
# binaries, a verified NATS executable, and the consumer fixture. It extracts the
# archives, patches only those three public artifacts into a fresh consumer
# build, audits the resolved dependency graph, and runs the fixture's live
# target. No Trellis source checkout is required or used.
#
# Usage:
#   verify-rust-test-package.sh --bundle ABSOLUTE_BUNDLE_DIR --mode smoke
#   verify-rust-test-package.sh --bundle ABSOLUTE_BUNDLE_DIR --mode full
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
[[ -d "$bundle" ]] || { echo "bundle directory not found: $bundle" >&2; exit 2; }

bundle="$(cd "$bundle" && pwd)"
packages_dir="$bundle/packages"
consumer_src="$bundle/consumer"
[[ -d "$packages_dir" ]] || { echo "missing $packages_dir" >&2; exit 1; }
[[ -d "$consumer_src" ]] || { echo "missing $consumer_src" >&2; exit 1; }

for tool in cargo python3 tar; do
  command -v "$tool" >/dev/null 2>&1 || { echo "required tool not found: $tool" >&2; exit 1; }
done

work="$(mktemp -d)"
cleanup() { rm -rf "$work"; }
trap cleanup EXIT

# 1. Extract the three public archives with traversal/absolute-path checks.
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
test_dir="$(find "$work/packages" -maxdepth 1 -type d -name 'trellis-test-*' | head -1)"
[[ -n "$protocol_dir" && -n "$rs_dir" && -n "$test_dir" ]] || {
  echo "missing one of the extracted public crates" >&2
  exit 1
}

# The projected generated administration source must ship inside the testkit.
if ! tar tzf "$packages_dir"/trellis-test-*.crate | grep 'src/runtime_api/lib.rs' > /dev/null; then
  echo "trellis-test.crate is missing the generated administration projection" >&2
  exit 1
fi

# 2. Isolate the build and patch only the three extracted public artifacts.
cp -R "$consumer_src" "$work/consumer"
config="$work/cargo-config.toml"
cat > "$config" <<EOF
[patch.crates-io]
trellis-protocol = { path = "$protocol_dir" }
trellis-rs = { path = "$rs_dir" }
trellis-test = { path = "$test_dir" }
EOF

export CARGO_HOME="$work/cargo-home"
export CARGO_TARGET_DIR="$work/target"
mkdir -p "$CARGO_HOME"

cargo metadata --manifest-path "$work/consumer/Cargo.toml" --format-version 1 \
  --config "$config" > "$work/metadata.json"

# 3. Audit every resolved manifest: registry, the three allowed public roots, or
#    the consumer's own application/generated roots. No forbidden private crate.
python3 - "$work/metadata.json" "$work/packages" "$work/consumer" <<'PY'
import json, os, sys
metadata, packages_root, consumer_root = sys.argv[1], os.path.abspath(sys.argv[2]), os.path.abspath(sys.argv[3])
forbidden = {
    "trellis-bootstrap", "trellis-local-bootstrap", "trellis-local-nats",
    "trellis-runtime", "trellis-runtime-apis", "trellis-events-runtime",
    "trellis-jobs-runtime", "trellis-idl", "trellis-codegen-rust",
    "trellis-codegen-ts", "trellis-cli", "trellis-server", "xtask",
}
data = json.load(open(metadata))
for package in data["packages"]:
    name = package["name"]
    source = package.get("source")
    manifest = os.path.abspath(package["manifest_path"])
    if name in forbidden:
        raise SystemExit(f"forbidden package in the testkit closure: {name}")
    if source is None and not (
        manifest.startswith(packages_root + os.sep) or manifest.startswith(consumer_root + os.sep)
    ):
        raise SystemExit(f"non-registry package outside the allowed roots: {name} ({manifest})")
print("dependency audit passed for", len(data["packages"]), "packages")
PY

# 4. Resolve matching executables from the bundle.
if [[ -d "$bundle/binaries" ]]; then
  cli="$(find "$bundle/binaries" -type f -name 'trellis' | head -1 || true)"
  server="$(find "$bundle/binaries" -type f -name 'trellis-server' | head -1 || true)"
  [[ -n "$cli" ]] && export TRELLIS_TEST_CLI_BIN="$cli"
  [[ -n "$server" ]] && export TRELLIS_TEST_SERVER_BIN="$server"
fi
if [[ -d "$bundle/nats" ]]; then
  nats="$(find "$bundle/nats" -type f -name 'nats-server*' | head -1 || true)"
  [[ -n "$nats" ]] && export TRELLIS_TEST_NATS_BIN="$nats"
fi

# 5. Run the consumer's live target.
if [[ "$mode" == "smoke" ]]; then
  cargo test --manifest-path "$work/consumer/Cargo.toml" --config "$config" \
    --test live -- --skip t07_eight_concurrent_runtimes_are_isolated
else
  cargo test --manifest-path "$work/consumer/Cargo.toml" --config "$config" --test live
fi
