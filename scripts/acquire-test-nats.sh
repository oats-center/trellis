#!/usr/bin/env bash
#
# Downloads the pinned nats-server for this host, verifies its SHA-256 against
# crates/local-nats/nats-binaries.json, and installs it executable into `--dest`.
#
# The producer calls this instead of scanning a shared cache, so a fresh runner
# without preexisting NATS still produces a verified bundle.
set -euo pipefail

dest=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --dest)
      dest="${2:-}"
      shift 2
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
done
[[ -n "$dest" ]] || { echo "--dest is required" >&2; exit 2; }
[[ -f crates/local-nats/nats-binaries.json ]] || {
  echo "run from the repository root: crates/local-nats/nats-binaries.json not found" >&2
  exit 1
}
mkdir -p "$dest"

pins=crates/local-nats/nats-binaries.json
version="$(jq -r '.["nats-server"].version' "$pins")"
case "$(uname -s)" in Linux) os=linux ;; Darwin) os=darwin ;; *) os="" ;; esac
case "$(uname -m)" in x86_64) arch=amd64 ;; aarch64 | arm64) arch=arm64 ;; *) arch="" ;; esac
[[ -n "$os" && -n "$arch" ]] || {
  echo "unsupported host for pinned NATS: $(uname -s)/$(uname -m)" >&2
  exit 1
}
asset="$os-$arch"
expected="$(jq -r --arg asset "$asset" '.["nats-server"].sha256[$asset] // empty' "$pins")"
[[ -n "$expected" ]] || { echo "no pinned sha256 for asset $asset" >&2; exit 1; }
url="https://github.com/nats-io/nats-server/releases/download/v$version/nats-server-v$version-$asset.tar.gz"
dest="$(cd "$dest" && pwd)"
work="$(mktemp -d "$dest/.nats.XXXXXX")"
trap 'rm -rf "$work"' EXIT
curl -fsSL --retry 3 --output "$work/nats.tar.gz" "$url"
actual="$(sha256sum "$work/nats.tar.gz" | cut -d ' ' -f 1)"
[[ "$actual" == "$expected" ]] || {
  echo "checksum mismatch for $url: expected $expected, got $actual" >&2
  exit 1
}
member="$(tar -tzf "$work/nats.tar.gz" | grep -E '(^|/)nats-server$' | head -n1)"
[[ -n "$member" ]] || { echo "nats-server binary not found in the pinned archive" >&2; exit 1; }
tar -xzf "$work/nats.tar.gz" -C "$work" "$member"
install -m 0755 "$work/$member" "$dest/nats-server"
echo "acquired nats-server $version ($asset) sha256=$actual"
