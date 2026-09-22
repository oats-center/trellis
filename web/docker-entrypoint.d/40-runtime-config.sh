#!/bin/sh

set -eu

auth_url=${VITE_TRELLIS_AUTH_URL:-http://localhost:3000}
nats_servers=${VITE_TRELLIS_NATS_SERVERS:-ws://localhost:8080}

js_escape() {
  printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'
}

cat > /usr/share/nginx/html/runtime-config.js <<EOF
window.__TRELLIS_RUNTIME_CONFIG__ = {
  authUrl: "$(js_escape "$auth_url")",
  natsServers: "$(js_escape "$nats_servers")"
};
EOF
