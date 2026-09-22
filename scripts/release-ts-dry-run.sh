set -euo pipefail

npm_tag=latest
if [[ "${RELEASE_TAG:-}" == *-* ]]; then
  npm_tag=rc
fi
for package in \
  ts/packages/result/npm \
  ts/packages/trellis/npm \
  ts/packages/trellis-svelte/npm
do
  npm publish --dry-run --access public --tag "$npm_tag" "$package"
done

test -s ts/packages/trellis/auth/protocol_wasm/trellis_protocol_wasm_bg.wasm

for package in \
  ts/packages/result \
  ts/packages/trellis \
  ts/packages/trellis-test
do
  (cd "$package" && deno publish --dry-run --allow-slow-types --allow-dirty)
done
