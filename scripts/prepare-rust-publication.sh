#!/usr/bin/env bash
# Prepare and verify publication without uploading. Call from the workspace root.
set -euo pipefail

accepted="$1"
target="$2"
shift 2
version="$(python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml", "rb"))["workspace"]["package"]["version"])')"

for crate in "$@"; do
  # This name becomes a deletion path; reject selectors containing paths.
  [[ "$crate" =~ ^trellis-[a-z0-9-]+$ ]] || { echo "Invalid crate name: $crate" >&2; exit 2; }
  archive="$target/package/$crate-$version.crate"
  # Retain compiled dependencies, but never compare a previous package archive.
  rm -f -- "$archive"
  cargo package --manifest-path Cargo.toml --allow-dirty --locked --no-verify \
    --target-dir "$target" --config .cargo/publication.toml -p "$crate"
  python3 scripts/verify-rust-crate-contents.py \
    "$accepted/$crate-$version.crate" "$archive"
  cargo publish --allow-dirty --locked --dry-run \
    --target-dir "$target" --config .cargo/publication.toml -p "$crate"
done
