#!/usr/bin/env bash
# Checks the web container (web/Dockerfile + web/Caddyfile + site/) without a
# backend: the Caddyfile parses and is formatted, the image builds, and the
# routes answer as documented in the Caddyfile header. Proxied paths answer
# 502 here because there is no `server`; that still proves they were matched
# and proxied rather than served from disk.
#
#   scripts/test-web-container.sh            # builds obsink-web:ci and tests it
#   OBSINK_WEB_IMAGE=obsink-web:dev scripts/test-web-container.sh   # reuse an image
# pass() always succeeds, so `check && pass || fail` is an if-else here.
# shellcheck disable=SC2015
set -euo pipefail
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

IMAGE="${OBSINK_WEB_IMAGE:-}"
PORT="${OBSINK_WEB_TEST_PORT:-18081}"
CADDY_IMAGE="caddy:2-alpine"
NAME="obsink-web-test-$$"
failures=0

fail() { echo "FAIL: $*" >&2; failures=$((failures + 1)); }
pass() { echo "ok: $*"; }

echo "==> Caddyfile parses and is formatted"
docker run --rm -v "$REPO_ROOT/web/Caddyfile:/etc/caddy/Caddyfile:ro" "$CADDY_IMAGE" \
    caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile >/dev/null 2>&1 \
    || fail "web/Caddyfile does not validate"
# `caddy fmt --diff` prints context lines even when nothing differs; only
# lines that add or remove something mean the file is not formatted.
fmt_diff=$(docker run --rm -v "$REPO_ROOT/web/Caddyfile:/etc/caddy/Caddyfile:ro" "$CADDY_IMAGE" \
    caddy fmt --diff /etc/caddy/Caddyfile 2>/dev/null || true)
if printf '%s\n' "$fmt_diff" | grep -qE '^[+-]'; then
    printf '%s\n' "$fmt_diff" | grep -E '^[+-]'
    fail "web/Caddyfile is not caddy-fmt formatted (docker run --rm -v \$PWD/web/Caddyfile:/etc/caddy/Caddyfile caddy:2-alpine caddy fmt --overwrite /etc/caddy/Caddyfile)"
else
    pass "Caddyfile"
fi

echo "==> Static checks on the site"
for needle in 'href="/app/"' 'https://github.com/spencerjireh/obsink' '/install.sh'; do
    grep -qF -- "$needle" site/index.html && pass "index.html links $needle" || fail "index.html lacks $needle"
done
# The page picks release assets by suffix; the release workflow must produce them.
for suffix in '-universal.dmg' '-universal.dmg.sha256' '-universal-apple-darwin.tar.gz'; do
    grep -qF -- "$suffix" site/index.html || fail "index.html no longer matches $suffix"
    grep -qF -- "${suffix#-}" .github/workflows/release.yml || fail "release.yml no longer produces $suffix"
done
head -1 site/install.sh | grep -q '^#!/bin/sh' && pass "install.sh shebang" || fail "install.sh shebang"

if [ -z "$IMAGE" ]; then
    echo "==> Building the image"
    IMAGE="obsink-web:ci"
    docker build -q -f web/Dockerfile -t "$IMAGE" . >/dev/null
fi

echo "==> Running $IMAGE on :$PORT with a stub API"
# The Caddyfile proxies to `server:8080`. A stub that answers every path with
# one JSON body stands in for the API: enough to prove which paths are proxied
# and that the proxy keeps the headers the Caddyfile adds.
NET="$NAME-net"
docker network create "$NET" >/dev/null
trap 'docker rm -f "$NAME" "$NAME-server" >/dev/null 2>&1; docker network rm "$NET" >/dev/null 2>&1 || true' EXIT
docker run -d --rm --name "$NAME-server" --network "$NET" --network-alias server "$CADDY_IMAGE" \
    caddy respond --listen :8080 --header 'Content-Type: application/json' '{"stub":true}' >/dev/null
docker run -d --rm --name "$NAME" --network "$NET" -p "$PORT:80" "$IMAGE" >/dev/null
base="http://localhost:$PORT"
for _ in $(seq 1 30); do
    curl -fsS -o /dev/null -H 'Accept: text/html' "$base/" 2>/dev/null && break
    sleep 0.5
done

# status <expected> <url> [curl args...]
status() {
    local expected="$1" url="$2"; shift 2
    local got
    got=$(curl -sS -o /dev/null -w '%{http_code}' "$@" "$url")
    [ "$got" = "$expected" ] && pass "$url -> $got" || fail "$url -> $got (expected $expected)"
}
# header <url> <header> <substring> [curl args...]
header() {
    local url="$1" name="$2" want="$3"; shift 3
    local got
    got=$(curl -sSI "$@" "$url" | tr -d '\r' | awk -F': ' -v n="$name" 'tolower($1)==tolower(n){print substr($0, length(n)+3)}' | head -1)
    case "$got" in
        *"$want"*) pass "$url $name: $got" ;;
        *) fail "$url $name is '$got' (expected to contain '$want')" ;;
    esac
}

echo "==> Landing page"
status 200 "$base/" -H 'Accept: text/html'
curl -fsS -H 'Accept: text/html' "$base/" | grep -q 'open ObSink in the browser' && pass "landing page body" || fail "landing page body"
header "$base/" Content-Type text/html -H 'Accept: text/html'
header "$base/" Vary Accept -H 'Accept: text/html'
header "$base/" Cache-Control no-cache -H 'Accept: text/html'
header "$base/" X-Content-Type-Options nosniff -H 'Accept: text/html'
header "$base/" Referrer-Policy strict-origin -H 'Accept: text/html'
header "$base/" Strict-Transport-Security max-age -H 'Accept: text/html'
header "$base/" Content-Security-Policy frame-ancestors -H 'Accept: text/html'

echo "==> API paths reach the stub server"
proxied() {
    local url="$1"; shift
    local body
    body=$(curl -sS "$@" "$url")
    case "$body" in
        *'"stub":true'*) pass "$url is proxied" ;;
        *) fail "$url was not proxied (body: ${body:0:60})" ;;
    esac
}
proxied "$base/" -H 'Accept: */*'
proxied "$base/" -H 'Accept: application/json'
proxied "$base/"   # curl's default Accept is */*, like reqwest and URLSession
header "$base/" Content-Type application/json -H 'Accept: application/json'
header "$base/" Vary Accept -H 'Accept: application/json'
header "$base/" Cache-Control no-store -H 'Accept: application/json'
header "$base/" X-Content-Type-Options nosniff -H 'Accept: application/json'
proxied "$base/vaults"
proxied "$base/vaults/abc/manifest"
proxied "$base/auth/email/start" -X POST
proxied "$base/healthz"
status 404 "$base/vaultsx"   # only the exact API paths are proxied

echo "==> Browser client"
status 301 "$base/app"
header "$base/app" Location /app/
status 200 "$base/app/"
curl -fsS "$base/app/" | grep -q '<div id="root">' && pass "app index" || fail "app index"
header "$base/app/" Cache-Control no-cache
status 200 "$base/app/vaults/some-id"
curl -fsS "$base/app/vaults/some-id" | grep -q '<div id="root">' && pass "SPA fallback" || fail "SPA fallback"
status 404 "$base/app/assets/does-not-exist.js"
asset=$(curl -fsS "$base/app/" | sed -n 's/.*src="\(\/app\/assets\/[^"]*\.js\)".*/\1/p' | head -1)
[ -n "$asset" ] && pass "index references $asset" || fail "no hashed asset in the app index"
header "$base$asset" Cache-Control immutable
header "$base$asset" Content-Type javascript
wasm=$(docker exec "$NAME" sh -c 'ls /srv/app/assets/*.wasm 2>/dev/null | head -1 | xargs -n1 basename' || true)
if [ -n "$wasm" ]; then
    header "$base/app/assets/$wasm" Content-Type application/wasm
fi

echo "==> Static files"
status 200 "$base/install.sh"
curl -fsS "$base/install.sh" | head -1 | grep -q '^#!/bin/sh' && pass "install.sh body" || fail "install.sh body"
header "$base/install.sh" Cache-Control no-cache
status 200 "$base/icon.svg"
status 301 "$base/favicon.ico"
status 200 "$base/robots.txt"
status 404 "$base/no-such-page"
curl -sS "$base/no-such-page" | grep -q 'Back to ObSink' && pass "404 page" || fail "404 page"

if [ "$failures" -gt 0 ]; then
    echo "$failures check(s) failed" >&2
    exit 1
fi
echo "Web container checks passed"
