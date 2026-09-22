#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"

if [[ "${1:-}" == "--" ]]; then
  shift
fi

if [[ -n "${TRELLIS_CLI_BIN:-}" ]]; then
  exec "$TRELLIS_CLI_BIN" "$@"
fi

if [[ -x "$repo_root/target/debug/trellis" ]]; then
  exec "$repo_root/target/debug/trellis" "$@"
fi

if [[ -x "$repo_root/target/release/trellis" ]]; then
  exec "$repo_root/target/release/trellis" "$@"
fi

exec cargo run --manifest-path "$repo_root/Cargo.toml" -p trellis-cli --bin trellis -- "$@"
