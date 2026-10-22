set -euo pipefail

npm_tag=latest
if [[ "${RELEASE_TAG:-}" == *-* ]]; then
  npm_tag=rc
fi
for package in \
  ts/packages/result/npm \
  ts/packages/trellis/npm \
  ts/packages/trellis-testkit/npm \
  ts/packages/trellis-svelte/npm
do
  npm publish --dry-run --access public --tag "$npm_tag" "$package"
done

test -s ts/packages/trellis/auth/protocol_wasm/client/trellis_protocol_wasm_bg.wasm
test -s ts/packages/trellis/auth/protocol_wasm/provider/trellis_protocol_wasm_bg.wasm

# Release stages testkit's verified native archive pins before publication.
