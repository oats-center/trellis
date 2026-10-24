#!/usr/bin/env bash
#
# Builds the pinned Dex identity provider the Rust upstream-OIDC test drives,
# and installs it executable as `<dest>/dex`.
#
# Dex publishes container images but no release binaries, so this builds the
# pinned source revision with Go. Check and local runs use this one script, so
# the pin lives in exactly one place. An existing binary built from the same
# revision is reused.
set -euo pipefail

DEX_COMMIT=05a275057112ba93e17db21f600b756f000f1c21

dest=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --dest)
      dest="${2:-}"
      shift 2
      ;;
    --print-commit)
      echo "$DEX_COMMIT"
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
done
[[ -n "$dest" ]] || { echo "--dest is required" >&2; exit 2; }
command -v go >/dev/null || { echo "Go is required to build the pinned Dex" >&2; exit 1; }
mkdir -p "$dest"
dest="$(cd "$dest" && pwd)"
if [[ -x "$dest/dex" && "$(cat "$dest/dex.commit" 2>/dev/null)" == "$DEX_COMMIT" ]]; then
  exit 0
fi

source_dir="$(mktemp -d "$dest/dex-source.XXXXXX")"
trap 'rm -rf "$source_dir"' EXIT
git -C "$source_dir" init --quiet
git -C "$source_dir" fetch --quiet --depth 1 https://github.com/dexidp/dex.git "$DEX_COMMIT"
git -C "$source_dir" checkout --quiet FETCH_HEAD
# Dex's API is an in-tree module replacement, so build the pinned tree itself.
(cd "$source_dir" && go build -mod=readonly -trimpath -o "$dest/dex" ./cmd/dex)
echo "$DEX_COMMIT" > "$dest/dex.commit"
echo "built dex $DEX_COMMIT"
