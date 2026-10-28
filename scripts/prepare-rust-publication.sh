#!/usr/bin/env bash
# Prepare and verify publication without uploading. Call from the workspace root.
set -euo pipefail

accepted="$1"
target="$2"
shift 2
versions="$(cargo metadata --format-version 1 --no-deps --locked)"

for crate in "$@"; do
  # This name becomes a deletion path; reject selectors containing paths.
  case "$crate" in
    trellis-protocol|trellis-models|trellis-rs|trellis-testkit) ;;
    *) echo "Not a public Rust distribution package: $crate" >&2; exit 2 ;;
  esac
  version="$(printf '%s' "$versions" | jq -r --arg crate "$crate" '.packages[] | select(.name == $crate) | .version')"
  archive="$target/package/$crate-$version.crate"
  # Retain compiled dependencies, but never compare a previous package archive.
  rm -f -- "$archive"
  cargo package --manifest-path Cargo.toml --allow-dirty --locked --no-verify \
    --target-dir "$target" --config .cargo/publication.toml -p "$crate"
  cargo xtask release compare-crate \
    "$accepted/$crate-$version.crate" "$archive"
  cargo publish --allow-dirty --locked --dry-run \
    --target-dir "$target" --config .cargo/publication.toml -p "$crate"
done
