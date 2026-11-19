#!/usr/bin/env bash
#
# Installs the pinned CNCF Distribution registry the published-package tests
# publish to and pull from, as `<dest>/registry`.
#
# Distribution publishes a static binary per release; this fetches the archive,
# checks its SHA-256 and the binary's, and installs the binary. The pipeline
# and local runs use this one script, so the pin lives in exactly one place.
set -euo pipefail

REGISTRY_VERSION=3.0.0
declare -A ARCHIVE_SHA256=(
  [amd64]=61c9a2c0d5981a78482025b6b69728521fbc78506d68b223d4a2eb825de5ca3d
)
declare -A BINARY_SHA256=(
  [amd64]=7e293ee23c59f5f0e5ba162e483af1ce1bca074891d769302bd2cffefbe4363b
)

dest=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --dest)
      dest="${2:-}"
      shift 2
      ;;
    --print-version)
      echo "$REGISTRY_VERSION"
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
done
[[ -n "$dest" ]] || { echo "--dest is required" >&2; exit 2; }
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) arch=amd64 ;;
  *) echo "the pinned registry is available for Linux x86-64 only" >&2; exit 1 ;;
esac
mkdir -p "$dest"
dest="$(cd "$dest" && pwd)"
if [[ -x "$dest/registry" ]] && sha256sum "$dest/registry" | grep -q "^${BINARY_SHA256[$arch]} "; then
  exit 0
fi

work="$(mktemp -d "$dest/registry-archive.XXXXXX")"
trap 'rm -rf "$work"' EXIT
archive="registry_${REGISTRY_VERSION}_linux_${arch}.tar.gz"
# Only curl's own --output starts a retry over, so the response lands in a file.
curl -fsSL --retry 10 --retry-all-errors --retry-delay 10 --retry-max-time 300 \
  --output "$work/$archive" \
  "https://github.com/distribution/distribution/releases/download/v${REGISTRY_VERSION}/${archive}"
actual="$(sha256sum "$work/$archive" | cut -d ' ' -f 1)"
[[ "$actual" == "${ARCHIVE_SHA256[$arch]}" ]] ||
  { echo "registry $REGISTRY_VERSION archive checksum mismatch: $actual" >&2; exit 1; }
tar -xzf "$work/$archive" -C "$work" registry
actual="$(sha256sum "$work/registry" | cut -d ' ' -f 1)"
[[ "$actual" == "${BINARY_SHA256[$arch]}" ]] ||
  { echo "registry $REGISTRY_VERSION checksum mismatch: $actual" >&2; exit 1; }
install -m 0755 "$work/registry" "$dest/registry"
echo "installed registry $REGISTRY_VERSION"
