#!/usr/bin/env bash
#
# Downloads the pinned nats-server for this host, verifies its SHA-256 against
# crates/local-nats/nats-binaries.json, and installs it executable into `--dest`.
#
# The producer calls this instead of scanning a shared cache, so a fresh runner
# without preexisting NATS still produces a verified bundle.
set -euo pipefail

dest=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --dest)
      dest="${2:-}"
      shift 2
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
done
[[ -n "$dest" ]] || { echo "--dest is required" >&2; exit 2; }
[[ -f crates/local-nats/nats-binaries.json ]] || {
  echo "run from the repository root: crates/local-nats/nats-binaries.json not found" >&2
  exit 1
}
mkdir -p "$dest"

python3 - crates/local-nats/nats-binaries.json "$dest" <<'PY'
import hashlib, json, os, shutil, sys, tarfile, tempfile, urllib.request

pin_path, dest = sys.argv[1], os.path.abspath(sys.argv[2])
data = json.load(open(pin_path))["nats-server"]
version = data["version"]

osname = {"Linux": "linux", "Darwin": "darwin"}.get(os.uname().sysname)
arch = {"x86_64": "amd64", "aarch64": "arm64", "arm64": "arm64"}.get(os.uname().machine)
if osname is None or arch is None:
    raise SystemExit(f"unsupported host for pinned NATS: {os.uname().sysname}/{os.uname().machine}")
asset = f"{osname}-{arch}"
expected = data["sha256"].get(asset)
if not expected:
    raise SystemExit(f"no pinned sha256 for asset {asset}")

url = (
    f"https://github.com/nats-io/nats-server/releases/download/"
    f"v{version}/nats-server-v{version}-{asset}.tar.gz"
)
with urllib.request.urlopen(url) as response:
    blob = response.read()
actual = hashlib.sha256(blob).hexdigest()
if actual != expected:
    raise SystemExit(f"checksum mismatch for {url}: expected {expected}, got {actual}")

tmp = tempfile.mkdtemp(dir=dest)
try:
    archive = os.path.join(tmp, "nats.tar.gz")
    with open(archive, "wb") as handle:
        handle.write(blob)
    with tarfile.open(archive) as tar:
        member = next(
            (m for m in tar.getmembers() if os.path.basename(m.name) == "nats-server"),
            None,
        )
        if member is None:
            raise SystemExit("nats-server binary not found in the pinned archive")
        member.name = "nats-server"
        tar.extract(member, tmp, filter="data")
    target = os.path.join(dest, "nats-server")
    shutil.move(os.path.join(tmp, "nats-server"), target)
    os.chmod(target, 0o755)
finally:
    shutil.rmtree(tmp, ignore_errors=True)

print(f"acquired nats-server {version} ({asset}) sha256={actual}")
PY
