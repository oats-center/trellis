#!/usr/bin/env bash
# GitHub's Linux cache action silently falls back to gzip when zstd is missing.
# Run before actions/cache so both restoration and the post-job save use zstdmt.
set -euo pipefail

if ! command -v zstd >/dev/null 2>&1; then
  if ! command -v apt-get >/dev/null 2>&1; then
    echo "::error::zstd is missing; install it in the self-hosted runner image." >&2
    exit 1
  fi
  if [[ $(id -u) -eq 0 ]]; then
    apt-get update -qq
    env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends zstd
  else
    if ! command -v sudo >/dev/null 2>&1; then
      echo "::error::zstd is missing and runner cannot install it without sudo." >&2
      exit 1
    fi
    sudo -n apt-get update -qq
    sudo -n env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends zstd
  fi
fi

# Some distributions supply zstd without the zstdmt multicore alias, but the
# cache action invokes that exact program name when it compresses the archive.
if ! command -v zstdmt >/dev/null 2>&1; then
  tool_dir="${RUNNER_TEMP:?}/trellis-cache-tools"
  mkdir -p "$tool_dir"
  cat > "$tool_dir/zstdmt" <<'SH'
#!/usr/bin/env bash
exec zstd -T0 "$@"
SH
  chmod +x "$tool_dir/zstdmt"
  echo "$tool_dir" >> "${GITHUB_PATH:?}"
  export PATH="$tool_dir:$PATH"
fi

if [[ "$(tar --version | sed -n '1p')" != *"GNU tar"* ]]; then
  echo "::error::GNU tar is required for the Linux GitHub cache archive." >&2
  exit 1
fi
command -v zstd
command -v zstdmt
zstdmt --version
