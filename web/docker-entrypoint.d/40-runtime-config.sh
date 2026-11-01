#!/bin/sh

set -eu

# The Console signs in with Trellis, which registered it at the origin this
# container serves (`trellis-server init --console-origin`).
: "${TRELLIS_ORIGIN:?set TRELLIS_ORIGIN to the Trellis origin, such as https://trellis.example.com}"
: "${CONSOLE_ORIGIN:?set CONSOLE_ORIGIN to the origin this container is reached at}"

js_escape() {
  printf '%s' "${1%/}" | sed 's/\\/\\\\/g; s/"/\\"/g'
}

cat > /usr/share/nginx/html/assets/web/runtime-config.js <<SCRIPT
globalThis.__TRELLIS_RUNTIME_CONFIG__ = {
  authUrl: "$(js_escape "$TRELLIS_ORIGIN")",
  clientId: "trellis.console",
  callbackUri: "$(js_escape "$CONSOLE_ORIGIN")/console/login"
};
SCRIPT
