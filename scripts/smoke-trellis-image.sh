#!/usr/bin/env bash
# Smoke the Trellis runtime image against an already-loaded local image, the
# way it runs in production: NATS in its own container from the image's
# pinned nats-server, made by `trellis-server nats new`, and Trellis in
# another on a network only they share. Checks non-root operation, read-only
# root filesystem and configuration, a writable state volume, account
# creation on first start, readiness, clean shutdown, and state placement.
#
# Run/job-specific container, network and volume names let several runner
# services share one Docker daemon without colliding; only this invocation's
# resources are removed.
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
nats_container="trellis-smoke-nats-${suffix}"
network="trellis-smoke-${suffix}"
bundle_volume="trellis-bundle-${suffix}"
state_volume="trellis-state-${suffix}"

work="$(mktemp -d)"

cleanup() {
  rm -rf "${work}"
  docker rm -f "${container}" "${nats_container}" >/dev/null 2>&1 || true
  docker network rm "${network}" >/dev/null 2>&1 || true
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

# 2. Certificates for localhost, a one-server NATS cluster and its system
#    user and Trellis signing key, all in the state volume and owned by the
#    image's non-root user. Production serves certificates clients already
#    trust; the smoke issues its own and trusts that issuer.
docker network create "${network}" >/dev/null
docker volume create "${bundle_volume}" >/dev/null
docker volume create "${state_volume}" >/dev/null
for volume in "${state_volume}:/var/lib/trellis" "${bundle_volume}:/etc/trellis"; do
  docker run --rm --user root --entrypoint chown -v "${volume}" \
    "${image}" -R 10001:10001 "${volume#*:}"
done
in_state() {
  docker run --rm --user 10001:10001 -v "${state_volume}":/var/lib/trellis "$@"
}
in_state --entrypoint trellis-server "${image}" \
  tls issue --out /var/lib/trellis/certificates --host localhost --host 127.0.0.1
in_state --entrypoint trellis-server "${image}" \
  nats new --servers 1 --listen 0.0.0.0 --out /var/lib/trellis/nats
docker run --detach --name "${nats_container}" --network "${network}" \
  --network-alias nats --read-only --user 10001:10001 \
  --volume "${state_volume}":/var/lib/trellis:rw,Z \
  --entrypoint nats-server "${image}" -c /var/lib/trellis/nats/nats-1.conf >/dev/null

# 3. The configuration a person writes, from `trellis-server config example`.
#    NATS is plaintext on the network only these two containers share.
docker run --rm -i --user 10001:10001 -v "${bundle_volume}":/etc/trellis \
  --entrypoint sh "${image}" -c 'cat > /etc/trellis/config.toml' <<'TOML'
[http]
public_origin = "https://localhost:3000"
port = 3000
bind_address = "0.0.0.0"

[http.transport]
mode = "tls"
certificate_chain = "/var/lib/trellis/certificates/server.crt"
private_key = "/var/lib/trellis/certificates/server.key"

[nats]
servers = "nats://nats:4222"
private_network = true
system_credentials = "/var/lib/trellis/nats/system.creds"
operator_signing_key = "/var/lib/trellis/nats/trellis-operator.nk"

[client]
nats_servers = []
TOML
# The first administrator, which also creates the installation's keys in
# secrets.toml. Later runs mount the configuration read-only.
docker run --rm --user 10001:10001 \
  -v "${bundle_volume}":/etc/trellis:Z -v "${state_volume}":/var/lib/trellis:Z \
  "${image}" admin --system --username admin --password smoke-admin-password >/dev/null
test "$(docker run --rm --entrypoint stat -v "${bundle_volume}":/etc/trellis:ro "${image}" \
  -c %a /etc/trellis/secrets.toml)" = 600
# Probes trust exactly that issuer instead of disabling verification.
docker run --rm --entrypoint cat -v "${state_volume}":/var/lib/trellis:ro "${image}" \
  /var/lib/trellis/certificates/ca.crt > "${work}/ca.crt"

# Non-root user, read-only rootfs, read-only configuration, writable state
# volume. Private runtime and log directories are created inside portable
# sticky tmpfs parents by the non-root service user.
docker run --detach --name "${container}" --network "${network}" --read-only --tmpfs /tmp \
  --tmpfs /run:mode=1777 \
  --tmpfs /var/log:mode=1777 \
  --user 10001:10001 \
  --publish 127.0.0.1::3000 \
  --volume "${bundle_volume}":/etc/trellis:ro,Z \
  --volume "${state_volume}":/var/lib/trellis:rw,Z \
  --entrypoint sh "${image}" -c \
    'set -eu
     for directory in /run/trellis /var/log/trellis; do
       if [ ! -e "$directory" ] && [ ! -L "$directory" ]; then
         mkdir -m 0700 "$directory"
       fi
       if [ -L "$directory" ] || [ ! -d "$directory" ] ||
          [ "$(stat -c %u:%a "$directory")" != "$(id -u):700" ]; then
         echo "unsafe private directory: $directory (requires current uid and mode 0700)" >&2
         exit 1
       fi
     done
     exec trellis-server --system' >/dev/null

# 4. Wait for runtime readiness on the published port.
runtime_port="$(docker port "${container}" 3000/tcp | cut -d: -f2)"
probe() {
  curl --fail --silent --cacert "${work}/ca.crt" \
    --resolve "localhost:${runtime_port}:127.0.0.1" \
    "https://localhost:${runtime_port}$1" >/dev/null
}
ready=""
for _ in $(seq 1 90); do
  if probe /readyz 2>/dev/null; then
    ready=1
    break
  fi
  if [ "$(docker inspect --format '{{.State.Running}}' "${container}")" != "true" ]; then
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

# Startup/write diagnostics belong to startup, not deliberate broker teardown.
startup_logs="$(docker logs "${container}" 2>&1)"
if grep -Ei 'read-only|readonly|permission denied|failed to (write|create|open)' <<<"${startup_logs}"; then
  echo "unexpected write/startup error before readiness" >&2
  printf '%s\n' "${startup_logs}" >&2
  exit 1
fi
probe /healthz

# 5. Graceful stop: SIGTERM, wait up to 30s, then check the exit code.
docker stop --time 30 "${container}" >/dev/null
exit_code="$(docker wait "${container}")"
if [ "${exit_code}" != "0" ]; then
  echo "${container} exited with ${exit_code} instead of 0" >&2
  docker logs "${container}" >&2 || true
  exit 1
fi

# 6. No logical recovery after the host reports that ownership has stopped.
# Use the runtime's structured diagnostics: a shutdown connection refusal must
# not be mislabeled as a startup write failure, but a new generation after
# terminal ownership is a real lifecycle defect even if the process exits zero.
# A group may stand by and take its work back while the process runs, so the
# terminal point is the process's own shutdown line, logged once every group
# has stopped.
logs="$(docker logs "${container}" 2>&1)"
if grep -Ei 'read-only|readonly|permission denied|failed to (write|create)' <<<"${logs}"; then
  echo "unexpected filesystem/write error during runtime operation or shutdown" >&2
  printf '%s\n' "${logs}" >&2
  exit 1
fi
if ! jq -Rse '
  split("\n") | map(fromjson? | select(type == "object")) |
  (map(select(.fields.message == "runtime shutdown finished; flushing NATS")) | first | .timestamp) as $stopped |
  $stopped != null and
  all(.[]; .timestamp <= $stopped or
    (.fields.event != "transport_generation.opening" and
     .fields.event != "transport_generation.open_failed" and
     .fields.message != "transport generation reconciliation failed"))
' <<<"${logs}" >/dev/null; then
  echo "runtime ownership did not stop before broker teardown" >&2
  printf '%s\n' "${logs}" >&2
  exit 1
fi

# 7. NATS holds the system account and the two Trellis created on its first
#    start; the configuration volume stayed untouched; the runtime's SQLite
#    databases landed in the system data root.
docker run --rm --entrypoint sh \
  -v "${bundle_volume}":/etc/trellis:ro,Z \
  -v "${state_volume}":/var/lib/trellis:rw,Z "${image}" -c '
    set -eu
    test "$(ls /var/lib/trellis/nats/data/nats-1/jwt/*.jwt | wc -l)" -ge 3
    test -f /var/lib/trellis/platform.sqlite
    test ! -e /etc/trellis/var
  '
echo "Trellis image smoke passed"
