#!/usr/bin/env bash
#
# Packs the published npm packages into dist/npm, records each tarball's
# integrity in dist/npm/packages.json (in publication order), and runs npm's
# publication dry-run on those exact tarballs. The Pipeline publishes these
# tarballs, checked against the recorded integrity, without packing again.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
# shellcheck source=lib/tmpdir.sh
source "$root/scripts/lib/tmpdir.sh"

test -s ts/packages/trellis/auth/protocol_wasm/client/trellis_protocol_wasm_bg.wasm
test -s ts/packages/trellis/auth/protocol_wasm/provider/trellis_protocol_wasm_bg.wasm

npm_tag=latest
if [[ "${RELEASE_TAG:-}" == *-* ]]; then
  npm_tag=rc
fi
dest=dist/npm
rm -rf "$dest"
mkdir -p "$dest"
# Dependencies first: a package publishes only after what it depends on.
for package in result trellis trellis-svelte trellis-testkit; do
  npm pack --json --pack-destination "$dest" "ts/packages/$package/npm"
done | jq -s '[.[][] | {name, version, filename, integrity}]' > "$dest/packages.json"
for tarball in $(jq -r '.[].filename' "$dest/packages.json"); do
  npm publish --dry-run --access public --tag "$npm_tag" "./$dest/$tarball"
done
