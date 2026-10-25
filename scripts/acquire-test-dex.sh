#!/usr/bin/env bash
#
# Installs the pinned Dex release the Rust upstream-OIDC test drives as
# `<dest>/dex`.
#
# Dex publishes its releases as container images, not archives. Its binary is
# static, so this fetches the release image's layer straight from the registry
# (no container engine) and verifies the binary's SHA-256. The pipeline and
# local runs use this one script, so the pin lives in exactly one place.
set -euo pipefail

DEX_VERSION=v2.46.1
declare -A DEX_SHA256=(
  [amd64]=e73fe3d2c9efc03551c62fcc5694acec8db1f36a79110f30070d2dc84c2951d1
)

dest=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --dest)
      dest="${2:-}"
      shift 2
      ;;
    --print-version)
      echo "$DEX_VERSION"
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
  *) echo "the pinned Dex is available for Linux x86-64 only" >&2; exit 1 ;;
esac
mkdir -p "$dest"
dest="$(cd "$dest" && pwd)"
if [[ -x "$dest/dex" ]] && sha256sum "$dest/dex" | grep -q "^${DEX_SHA256[$arch]} "; then
  exit 0
fi

work="$(mktemp -d "$dest/dex-image.XXXXXX")"
trap 'rm -rf "$work"' EXIT
registry=https://ghcr.io/v2/dexidp/dex
token="$(curl -fsSL "https://ghcr.io/token?scope=repository:dexidp/dex:pull" | jq -r .token)"
get() {
  curl -fsSL -H "Authorization: Bearer $token" -H "Accept: $1" "$2"
}
digest="$(get "application/vnd.oci.image.index.v1+json" "$registry/manifests/$DEX_VERSION" |
  jq -r --arg arch "$arch" \
    '.manifests[] | select(.platform.os == "linux" and .platform.architecture == $arch) | .digest' |
  head -n1)"
[[ -n "$digest" ]] || { echo "Dex $DEX_VERSION has no linux/$arch image" >&2; exit 1; }
# The binary is in the top layers; search from the last one.
for layer in $(get "application/vnd.oci.image.manifest.v1+json" "$registry/manifests/$digest" |
  jq -r '.layers | reverse | .[].digest'); do
  get "*/*" "$registry/blobs/$layer" > "$work/layer.tar.gz"
  if tar -xzf "$work/layer.tar.gz" -C "$work" usr/local/bin/dex 2>/dev/null; then
    break
  fi
done
[[ -f "$work/usr/local/bin/dex" ]] || { echo "Dex $DEX_VERSION image has no /usr/local/bin/dex" >&2; exit 1; }
actual="$(sha256sum "$work/usr/local/bin/dex" | cut -d ' ' -f 1)"
[[ "$actual" == "${DEX_SHA256[$arch]}" ]] ||
  { echo "Dex $DEX_VERSION checksum mismatch: $actual" >&2; exit 1; }
install -m 0755 "$work/usr/local/bin/dex" "$dest/dex"
echo "installed dex $DEX_VERSION"
