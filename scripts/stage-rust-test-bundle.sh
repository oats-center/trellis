#!/usr/bin/env bash
#
# Builds the artifact-only testkit verification bundle from a populated staging
# directory and writes artifact-manifest.json with SHA-256 hashes, then tars the
# whole directory so executable modes survive the CI artifact transfer.
#
# Usage:
#   stage-rust-test-bundle.sh \
#     --bundle ABSOLUTE_STAGING_DIR --output ABSOLUTE_TARBALL \
#     --source-sha SHA --target TARGET --version VERSION
set -euo pipefail

bundle=""
output=""
source_sha=""
target=""
version=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --bundle) bundle="${2:-}"; shift 2 ;;
    --output) output="${2:-}"; shift 2 ;;
    --source-sha) source_sha="${2:-}"; shift 2 ;;
    --target) target="${2:-}"; shift 2 ;;
    --version) version="${2:-}"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
for required in bundle output source_sha target version; do
  [[ -n "${!required}" ]] || { echo "--${required//_/-} is required" >&2; exit 2; }
done
[[ -d "$bundle" ]] || { echo "staging directory not found: $bundle" >&2; exit 2; }
for dir in packages binaries nats consumer; do
  [[ -d "$bundle/$dir" ]] || { echo "missing staging directory: $bundle/$dir" >&2; exit 1; }
done
[[ -f "$bundle/verify-rust-test-package.sh" ]] || {
  echo "missing $bundle/verify-rust-test-package.sh" >&2
  exit 1
}

python3 - "$bundle" "$output" "$source_sha" "$target" "$version" <<'PY'
import hashlib, json, os, sys, tarfile

bundle, output, source_sha, target, version = (
    os.path.abspath(sys.argv[1]),
    sys.argv[2],
    sys.argv[3],
    sys.argv[4],
    sys.argv[5],
)


def sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


files = {}
for root, _dirs, names in os.walk(bundle):
    for name in names:
        if name == "artifact-manifest.json":
            continue
        full = os.path.join(root, name)
        files[os.path.relpath(full, bundle)] = sha256(full)

packages = {}
for name in os.listdir(os.path.join(bundle, "packages")):
    if not name.endswith(".crate"):
        continue
    stem = name[: -len(".crate")]
    for package in ("trellis-protocol", "trellis-rs", "trellis-testkit"):
        prefix = f"{package}-"
        if stem.startswith(prefix):
            packages[package] = stem[len(prefix):]
for package in ("trellis-protocol", "trellis-rs", "trellis-testkit"):
    if package not in packages:
        raise SystemExit(f"missing {package} .crate in packages/")

manifest = {
    "source_sha": source_sha,
    "target": target,
    "version": version,
    "packages": packages,
    "files": files,
    "executables": ["binaries/trellis", "binaries/trellis-server", "nats/nats-server"],
}
with open(os.path.join(bundle, "artifact-manifest.json"), "w") as handle:
    json.dump(manifest, handle, indent=2, sort_keys=True)
    handle.write("\n")

with tarfile.open(output, "w:gz") as tar:
    tar.add(bundle, arcname=".")
print(f"staged {output} for {target} at {source_sha}")
PY
