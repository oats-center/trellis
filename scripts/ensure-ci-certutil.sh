#!/usr/bin/env bash
# Browser suites trust the generated installation CA through Chromium's NSS
# database, which needs NSS certutil; Playwright's browser dependencies omit it.
set -euo pipefail

if ! command -v certutil >/dev/null 2>&1; then
  if ! command -v apt-get >/dev/null 2>&1; then
    echo "::error::NSS certutil is missing; install it in the self-hosted runner image." >&2
    exit 1
  fi
  sudo=()
  if [[ $(id -u) -ne 0 ]]; then
    if ! command -v sudo >/dev/null 2>&1; then
      echo "::error::NSS certutil is missing and the runner cannot install it without sudo." >&2
      exit 1
    fi
    sudo=(sudo -n)
  fi
  "${sudo[@]}" apt-get update -qq
  "${sudo[@]}" env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends libnss3-tools
fi
command -v certutil
