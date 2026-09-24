#!/usr/bin/env bash
# Smoke the managed-NATS Trellis runtime image against an already-loaded local
# image: non-root operation, read-only root filesystem and configuration, a
# writable state volume, readiness, clean shutdown, and actual state placement.
#
# Run/job-specific container and volume names let several runner services share
# one Docker daemon without colliding; only this invocation's resources are
# removed.
#
# Usage: smoke-trellis-image.sh --image <ref> [--pin <nats-binaries.json>]
set -euo pipefail

image=""
pin="crates/local-nats/nats-binaries.json"
while [ "$#" -gt 0 ]; do
  case "$1" in
    --image) image="${2:-}"; shift 2 ;;
    --pin) pin="${2:-}"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
[ -n "${image}" ] || { echo "--image is required" >&2; exit 2; }
[ -f "${pin}" ] || { echo "NATS pin not found: ${pin}" >&2; exit 2; }

suffix="${GITHUB_RUN_ID:-local}-${GITHUB_JOB:-smoke}-$$"
container="trellis-smoke-${suffix}"
bundle_volume="trellis-bundle-${suffix}"
state_volume="trellis-state-${suffix}"

cleanup() {
  docker rm -f "${container}" >/dev/null 2>&1 || true
  docker volume rm "${bundle_volume}" >/dev/null 2>&1 || true
  docker volume rm "${state_volume}" >/dev/null 2>&1 || true
  docker image rm "${image}" >/dev/null 2>&1 || true
}
trap cleanup EXIT

# 1. Baked nats-server matches the pinned version; both binaries run.
nats_version="$(docker run --rm --entrypoint nats-server "${image}" --version)"
expected_nats_version="$(jq -r '.["nats-server"].version' "${pin}")"
case "${nats_version}" in *"v${expected_nats_version}"*) ;; *)
  echo "nats-server version mismatch: got '${nats_version}', expected v${expected_nats_version}" >&2
  exit 1
  ;;
esac
trellis_version="$(docker run --rm --entrypoint trellis "${image}" --version)"
test -n "${trellis_version}"
server_version="$(docker run --rm --entrypoint trellis-server "${image}" --version)"
test -n "${server_version}"

# 2. Generate a bundle into the state volume, owned by the image's non-root
#    user, then copy it into its own named volume. System path defaults keep
#    mutable SQLite state out of the read-only bundle.
docker volume create "${bundle_volume}" >/dev/null
docker volume create "${state_volume}" >/dev/null
docker run --rm --user root --entrypoint chown -v "${state_volume}":/var/lib/trellis \
  "${image}" -R 10001:10001 /var/lib/trellis
docker run --rm --user 10001:10001 --entrypoint trellis \
  -v "${state_volume}":/var/lib/trellis "${image}" \
  init config --out /var/lib/trellis/bundle
docker run --rm --user root --entrypoint cp \
  -v "${state_volume}":/from:ro -v "${bundle_volume}":/to "${image}" \
  -a /from/bundle/. /to/
# The bundle's creds/secrets are 0o600 and generated as 10001, but the copy
# helper runs as root and the bundle volume root is root-owned: the run
# container uid must own the bundle contents to read them.
docker run --rm --user root --entrypoint chown \
  -v "${bundle_volume}":/to "${image}" \
  -R 10001:10001 /to

# 3. Managed-mode run: non-root user, read-only rootfs, read-only bundle volume,
#    writable state volume. All mutable NATS files go to /var/lib/trellis/nats
#    via the system profile and the runtime's SQLite stores to
#    /var/lib/trellis/data. Private runtime and log directories are created
#    inside portable sticky tmpfs parents by the non-root service user.
docker run --detach --name "${container}" --read-only --tmpfs /tmp \
  --tmpfs /run:mode=1777 \
  --tmpfs /var/log:mode=1777 \
  --user 10001:10001 \
  --publish 127.0.0.1::3000 \
  --volume "${bundle_volume}":/etc/trellis:ro,Z \
  --volume "${state_volume}":/var/lib/trellis:rw,Z \
  --entrypoint sh "${image}" -c \
    'mkdir -m 0700 /run/trellis /var/log/trellis; exec trellis-server --system all --local-nats=/usr/local/bin/nats-server'

# 4. Wait for runtime readiness on the published port.
runtime_port="$(docker port "${container}" 3000/tcp | cut -d: -f2)"
ready=""
for _ in $(seq 1 90); do
  if curl --fail --silent "http://127.0.0.1:${runtime_port}/readyz" >/dev/null 2>&1; then
    ready=1
    break
  fi
  if ! docker inspect "${container}" >/dev/null 2>&1; then
    echo "${container} exited during startup" >&2
    docker logs "${container}" >&2 || true
    exit 1
  fi
  sleep 2
done
if [ -z "${ready}" ]; then
  echo "timed out waiting for runtime readiness" >&2
  docker logs "${container}" >&2 || true
  exit 1
fi

# 5. Graceful stop: SIGTERM, wait up to 30s, then check the exit code.
docker stop --time 30 "${container}" >/dev/null
exit_code="$(docker wait "${container}")"
if [ "${exit_code}" != "0" ]; then
  echo "${container} exited with ${exit_code} instead of 0" >&2
  docker logs "${container}" >&2 || true
  exit 1
fi

# 6. No startup or write errors in the logs.
logs="$(docker logs "${container}" 2>&1)"
if grep -Ei 'read-only|readonly|permission denied|failed to (write|create|open)' <<<"${logs}"; then
  echo "unexpected write/startup error in the smoke logs" >&2
  printf '%s\n' "${logs}" >&2
  exit 1
fi

# 7. Mutable NATS state landed in the state volume; the pid file was removed on
#    shutdown; the bundle volume stayed untouched; the runtime's SQLite
#    databases landed in the system data root.
docker run --rm --entrypoint sh \
  -v "${bundle_volume}":/etc/trellis:ro,Z \
  -v "${state_volume}":/var/lib/trellis:rw,Z "${image}" -c '
    set -eu
    test -f /var/lib/trellis/nats/nats.conf
    test -f /var/lib/trellis/nats/jwt.conf
    test -d /var/lib/trellis/nats/data/jwt
    test -d /var/lib/trellis/nats/data/jetstream
    test ! -e /var/lib/trellis/nats/nats-server.pid
    test ! -e /etc/trellis/nats/nats.local.conf
    test ! -e /etc/trellis/nats/jwt.local.conf
    test -f /var/lib/trellis/platform.sqlite
    test ! -e /etc/trellis/platform.sqlite
  '
echo "managed NATS image smoke passed"
