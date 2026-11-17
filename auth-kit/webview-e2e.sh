#!/usr/bin/env bash
# End-to-end check of the handshake in a real webview: launches a sidecar
# app with Porthole (which picks the port and token), and waits for the
# backend to log an authenticated `GET /api/ping 200`. That request comes
# from the page's own fetch(), so it only succeeds if the webview followed
# the 302, stored the HttpOnly cookie and sent it back.
#
#   auth-kit/webview-e2e.sh <porthole binary> <app.toml> <app id>
#
# Needs a display: run it under `xvfb-run -a` on headless Linux.
set -euo pipefail
porthole=$1 app_toml=$2 app_id=$3

data=$(mktemp -d)
trap 'kill "$pid" 2>/dev/null || true; rm -rf "$data"' EXIT
export XDG_DATA_HOME=$data PORTHOLE_NO_DIALOGS=1
log=$data/$app_id/logs/sidecar.log

"$porthole" run "$app_toml" & pid=$!
for _ in $(seq 1 100); do
  if grep -q "GET /api/ping 200" "$log" 2>/dev/null; then
    echo "ok: the page made an authenticated API call"
    grep -E "^(GET|POST)" "$log"
    exit 0
  fi
  kill -0 "$pid" 2>/dev/null || { echo "porthole exited early"; cat "$log" 2>/dev/null; exit 1; }
  sleep 0.2
done
echo "no authenticated API call within 20s; sidecar.log:"
cat "$log" 2>/dev/null || true
exit 1
