#!/bin/sh
# Installs the trellis CLI or trellis-server from a GitHub release.
#
#   curl -fsSL https://oats-center.github.io/trellis/install.sh | sh -s trellis
#   curl -fsSL https://oats-center.github.io/trellis/install.sh | sh -s trellis-server
#
# TRELLIS_VERSION picks a release (for example 0.100.0-rc.12); without it the
# newest stable release is used, or the newest prerelease before the first
# stable one. TRELLIS_INSTALL_DIR defaults to ~/.local/bin.
set -eu

bin=${1:-trellis}
case "$bin" in
trellis | trellis-server) ;;
*)
  echo "install.sh: expected trellis or trellis-server, got '$bin'" >&2
  exit 1
  ;;
esac

case "$(uname -s)/$(uname -m)" in
Linux/x86_64) target=x86_64-unknown-linux-gnu ;;
Linux/aarch64 | Linux/arm64) target=aarch64-unknown-linux-gnu ;;
*)
  echo "install.sh: no prebuilt $bin for $(uname -s) $(uname -m); see https://oats-center.github.io/trellis/guides/install-trellis" >&2
  exit 1
  ;;
esac

repo=https://github.com/oats-center/trellis
api=https://api.github.com/repos/oats-center/trellis/releases
version=${TRELLIS_VERSION:-}
if [ -z "$version" ]; then
  # The API does not list releases in version order, so compare versions here.
  versions=$(curl -fsSL "$api?per_page=100" | sed -n 's/^ *"tag_name": *"v\([^"]*\)".*/\1/p')
  version=$(printf '%s\n' "$versions" | sed '/-/d' | sort -V | tail -n 1)
  if [ -z "$version" ]; then
    version=$(printf '%s\n' "$versions" | sort -V | tail -n 1)
  fi
fi
version=${version#v}
if [ -z "$version" ]; then
  echo "install.sh: could not find a Trellis release" >&2
  exit 1
fi

dir=${TRELLIS_INSTALL_DIR:-$HOME/.local/bin}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

archive="$bin-$version-$target.tar.gz"
checksum="checksum-$version-$target-$bin.sha256"
echo "Downloading $bin $version"
curl -fsSL -o "$work/$archive" "$repo/releases/download/v$version/$archive"
curl -fsSL -o "$work/$checksum" "$repo/releases/download/v$version/$checksum"
(cd "$work" && sha256sum -c --quiet "$checksum")
tar -xzf "$work/$archive" -C "$work" "$bin"

mkdir -p "$dir"
install -m 755 "$work/$bin" "$dir/$bin"
echo "Installed $dir/$bin"
case ":$PATH:" in
*":$dir:"*) ;;
*) echo "Add $dir to your PATH to run $bin" ;;
esac
