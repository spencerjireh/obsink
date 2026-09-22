#!/usr/bin/env bash
# Runs the desktop crate's ignored live tests (`live_tests::desktop_flows_live`
# and `account_flow_live`) against a running server: the exact command
# functions the Tauri UI invokes, end to end, with a sandboxed HOME and the
# file-backed keyring. Expects the local compose stack (or any server started
# with AUTH_DEV_RETURN_CODE=1 whose operator key you hold):
#
#   OBSINK_PORT=18080 docker compose up -d --wait
#   scripts/verify-desktop-live.sh
#
# OBSINK_SERVER_URL (default http://localhost:18080) and OBSINK_API_KEY
# (default dev-operator-key) select the server. The account test creates and
# deletes accounts, so a non-local URL is refused unless
# OBSINK_LIVE_ALLOW_REMOTE=1. Expect about two minutes: the account test waits
# out the server's 60 s email cooldown.
set -euo pipefail
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

server_url="${OBSINK_SERVER_URL:-http://localhost:18080}"
api_key="${OBSINK_API_KEY:-dev-operator-key}"

case "$server_url" in
    http://localhost:*|http://localhost|http://127.0.0.1:*|http://127.0.0.1) ;;
    *)
        if [ "${OBSINK_LIVE_ALLOW_REMOTE:-}" != "1" ]; then
            echo "refusing to run the live tests against $server_url (they create and delete accounts);" >&2
            echo "set OBSINK_LIVE_ALLOW_REMOTE=1 to override" >&2
            exit 1
        fi
        ;;
esac

if ! curl -fsS --max-time 5 "$server_url/healthz" >/dev/null 2>&1; then
    echo "no server at $server_url/healthz; start the local stack first:" >&2
    echo "  OBSINK_PORT=18080 docker compose up -d --wait" >&2
    exit 1
fi

# tauri::generate_context! refuses to compile without the frontend bundle.
if [ ! -f desktop/dist/index.html ]; then
    echo "==> Building the desktop frontend (desktop/dist is missing)"
    npm run build -w desktop
fi

echo "==> Live tests against $server_url"
OBSINK_TEST_SERVER_URL="$server_url" OBSINK_TEST_API_KEY="$api_key" \
    cargo test -p obsink-desktop live_tests -- --ignored --nocapture --test-threads=1
echo "PASS: desktop live tests against $server_url"
