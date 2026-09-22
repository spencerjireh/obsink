#!/usr/bin/env bash
# Tests site/install.sh against a fake GitHub release served from a local
# directory: the happy path installs, a wrong digest is refused, a missing
# checksum file is refused, and an unreachable API fails with a clear message.
# The checksum fixture is produced with the same command as release.yml so the
# two formats cannot drift apart unnoticed. macOS only, like the script.
#
#   scripts/test-install-sh.sh
#
# pass() always succeeds, so `check && pass || fail` is an if-else here.
# shellcheck disable=SC2015
set -euo pipefail
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

work=$(mktemp -d)
server_pid=""
cleanup() {
    [ -n "$server_pid" ] && kill "$server_pid" 2>/dev/null || true
    rm -rf "$work"
}
trap cleanup EXIT
failures=0
fail() { echo "FAIL: $*" >&2; failures=$((failures + 1)); }
pass() { echo "ok: $*"; }

# A release with a fake `obsink` that only prints its version.
name="obsink-v9.9.9-universal-apple-darwin.tar.gz"
mkdir -p "$work/stage" "$work/pub/repos/spencerjireh/obsink/releases" "$work/dest"
printf '#!/bin/sh\necho "obsink 9.9.9"\n' > "$work/stage/obsink"
chmod +x "$work/stage/obsink"
tar -C "$work/stage" -czf "$work/pub/$name" obsink
# Exactly what .github/workflows/release.yml runs for the CLI asset.
(cd "$work/pub" && shasum -a 256 "$name" > "$name.sha256")

port=$(python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')
base="http://127.0.0.1:$port"
cat > "$work/pub/repos/spencerjireh/obsink/releases/latest" <<EOF
{
  "tag_name": "v9.9.9",
  "assets": [
    {"name": "$name", "browser_download_url": "$base/$name"},
    {"name": "$name.sha256", "browser_download_url": "$base/$name.sha256"}
  ]
}
EOF
python3 -m http.server "$port" --bind 127.0.0.1 --directory "$work/pub" >/dev/null 2>&1 &
server_pid=$!
for _ in $(seq 1 50); do
    curl -fsS -o /dev/null "$base/$name.sha256" 2>/dev/null && break
    sleep 0.1
done

run_install() {
    # stdin closed, stdout a pipe: the same shape as `curl ... | sh` in a script.
    OBSINK_GITHUB_API="$base" OBSINK_INSTALL_DIR="$work/dest" sh site/install.sh </dev/null >"$work/out.txt" 2>&1
}

echo "==> Installs from a well-formed release"
if run_install; then
    [ -x "$work/dest/obsink" ] && pass "binary installed" || fail "binary missing"
    grep -q 'Installed obsink 9.9.9 at' "$work/out.txt" && pass "reports the installed version" || fail "no Installed line: $(cat "$work/out.txt")"
    grep -q 'obsink login --server-url' "$work/out.txt" && pass "prints the next step" || fail "no next-step hint"
else
    fail "install.sh exited $? : $(cat "$work/out.txt")"
fi

echo "==> Refuses a wrong digest"
rm -f "$work/dest/obsink"
cp "$work/pub/$name.sha256" "$work/good.sha256"
printf '%064d  %s\n' 0 "$name" > "$work/pub/$name.sha256"
if run_install; then
    fail "a wrong digest was accepted"
else
    grep -q 'Checksum mismatch' "$work/out.txt" && pass "refused with the checksum message" || fail "wrong message: $(cat "$work/out.txt")"
    [ ! -e "$work/dest/obsink" ] && pass "nothing installed" || fail "binary installed despite the mismatch"
fi
cp "$work/good.sha256" "$work/pub/$name.sha256"

echo "==> Refuses a release without a checksum file"
mv "$work/pub/$name.sha256" "$work/hidden.sha256"
if run_install; then
    fail "installed without a checksum file"
else
    grep -q 'no checksum' "$work/out.txt" && pass "refused with the missing-checksum message" || fail "wrong message: $(cat "$work/out.txt")"
fi
mv "$work/hidden.sha256" "$work/pub/$name.sha256"

echo "==> Explains an unreachable GitHub API"
if OBSINK_GITHUB_API="http://127.0.0.1:1" OBSINK_INSTALL_DIR="$work/dest" sh site/install.sh </dev/null >"$work/out.txt" 2>&1; then
    fail "succeeded without the API"
else
    grep -q 'Could not reach the GitHub API' "$work/out.txt" && pass "names the API failure" || fail "wrong message: $(cat "$work/out.txt")"
fi

echo "==> Refuses non-macOS hosts"
if uname_out=$(PATH="$work/fakebin:$PATH" sh -c 'mkdir -p "$0/fakebin" && printf "#!/bin/sh\necho Linux\n" > "$0/fakebin/uname" && chmod +x "$0/fakebin/uname" && PATH="$0/fakebin:$PATH" sh site/install.sh 2>&1' "$work"); then
    fail "ran on a fake Linux"
else
    printf '%s\n' "$uname_out" | grep -q 'macOS only' && pass "refuses with the macOS-only message" || fail "wrong message: $uname_out"
fi

if [ "$failures" -gt 0 ]; then
    echo "$failures check(s) failed" >&2
    exit 1
fi
echo "install.sh checks passed"
