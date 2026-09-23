#!/usr/bin/env bash
#
# Simulator E2E for the Mac↔iOS checklist (OBS-29–34) on wire format v3.
#
# "Device A" is the CLI (the reference client) driven from this script against
# a running server; "device B" is the ObSink app + File Provider on an iOS
# simulator, driven through the XCUITest phases in ios/UITests/SyncE2ETests.swift.
# Both are devices of one account: A signs it up (setting the passphrase) and
# creates the vault; B signs in through the UI, unlocks with the passphrase
# and downloads the vault, then the later phases run with A's session and the
# account key seeded so each one is independent. On-disk state on the iOS
# side is verified straight through the app-group container
# (`simctl get_app_container … groups`), which the host can read.
#
# Requires: .env.deploy at the repo root (OBSINK_SERVER_URL, DEVELOPMENT_TEAM,
# and OBSINK_INVITE_CODE when the server already has accounts), a server
# reachable at OBSINK_SERVER_URL started with AUTH_DEV_RETURN_CODE=1 (the
# local compose stack; the app fills the emailed code in by itself), a built
# xcframework + generated project (scripts/build-ios.sh), and Xcode.
#
# Usage: scripts/verify-ios-sim-e2e.sh [work-dir]
# The work dir (default: mktemp) holds device A's vault, config, keyring and all logs.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# The newest available iPhone simulator unless OBSINK_SIM_NAME says otherwise
# (the same selection as ci.yml, so the two cannot drift at an Xcode bump).
SIM_NAME="${OBSINK_SIM_NAME:-$(xcrun simctl list -j devices available \
    | jq -r '[.devices[][] | select(.isAvailable and (.name | test("^iPhone [0-9]+( Pro)?$")))] | sort_by((.name | capture("(?<n>[0-9]+)").n | tonumber), .name) | last | .name')}"
[ -n "$SIM_NAME" ] && [ "$SIM_NAME" != "null" ] || { echo "no iPhone simulator available; set OBSINK_SIM_NAME"; exit 1; }
WORK="${1:-$(mktemp -d /tmp/obsink-ios-e2e.XXXXXX)}"
DERIVED="$REPO_ROOT/ios/build/DerivedData"
APP_BUNDLE_ID="com.obsink.ios"

mkdir -p "$WORK"
exec > >(tee "$WORK/run.log") 2>&1
# Explicit variables win over .env.deploy (which usually names production).
EXPLICIT_SERVER_URL="${OBSINK_SERVER_URL:-}"
EXPLICIT_INVITE_CODE="${OBSINK_INVITE_CODE:-}"
if [ -f "$REPO_ROOT/.env.deploy" ]; then
    set -a; . "$REPO_ROOT/.env.deploy"; set +a
fi
export DEVELOPMENT_TEAM="${DEVELOPMENT_TEAM:-}"
OBSINK_INVITE_CODE="${EXPLICIT_INVITE_CODE:-${OBSINK_INVITE_CODE:-}}"
# Baked into the app by xcodegen below; the UI tests also pass it at launch
# (OBSINK_UITEST_SERVER_URL), so the phases do not depend on the baked value.
export OBSINK_SERVER_URL="${EXPLICIT_SERVER_URL:-${OBSINK_SERVER_URL:-http://localhost:18080}}"
SERVER_URL="${OBSINK_SERVER_URL%/}"

VAULT_NAME="e2e-sim-$(date +%s)"
EMAIL="ios-e2e-$(date +%s)@example.test"
PASSPHRASE="e2e-$(openssl rand -hex 8)"
A_HOME="$WORK/deviceA"
A_VAULT="$A_HOME/vault"
A_KEYRING="$A_HOME/keyring"
mkdir -p "$A_VAULT" "$A_KEYRING"

PASS_COUNT=0
FAIL_COUNT=0
step() { printf '\n==> %s\n' "$*"; }
pass() { printf 'PASS: %s\n' "$*"; PASS_COUNT=$((PASS_COUNT+1)); }
fail() { printf 'FAIL: %s\n' "$*"; FAIL_COUNT=$((FAIL_COUNT+1)); }

# Device A: its own config home, file keyring, device id and the passphrase.
cli() {
    OBSINK_HOME="$A_HOME" OBSINK_KEYRING_DIR="$A_KEYRING" OBSINK_DEVICE_ID="e2e-cli" \
        OBSINK_PASSPHRASE="$PASSPHRASE" OBSINK_SERVER_URL="$SERVER_URL" \
        "$REPO_ROOT/target/debug/obsink" "$@"
}

# The file keyring names entries after the keychain account with `/` and `:`
# flattened (core `keychain::keyring_file`).
keyring_entry() {
    cat "$A_KEYRING/$(printf '%s' "$1" | tr '/:' '__')"
}

# Run one XCUITest phase; extra env for the app goes via TEST_RUNNER_*.
RUN_N=0
run_test() {
    local test_name="$1"; shift
    RUN_N=$((RUN_N+1))
    local log="$WORK/xcuitest-$(printf '%02d' "$RUN_N")-$test_name.log"
    if env "$@" \
        TEST_RUNNER_OBSINK_TEST_SERVER_URL="$SERVER_URL" \
        TEST_RUNNER_OBSINK_TEST_EMAIL="$EMAIL" \
        TEST_RUNNER_OBSINK_TEST_PASSPHRASE="$PASSPHRASE" \
        TEST_RUNNER_OBSINK_TEST_BEARER="$BEARER" \
        TEST_RUNNER_OBSINK_TEST_USER_ID="$USER_ID" \
        TEST_RUNNER_OBSINK_TEST_ACCOUNT_KEY="$ACCOUNT_KEY" \
        TEST_RUNNER_OBSINK_TEST_ACCOUNT_KEY_ID="$ACCOUNT_KEY_ID" \
        TEST_RUNNER_OBSINK_TEST_SEED_ACCOUNT="$SEED_ACCOUNT" \
        TEST_RUNNER_OBSINK_TEST_VAULT_ID="$VAULT_ID" \
        TEST_RUNNER_OBSINK_TEST_VAULT_NAME="$VAULT_NAME" \
        TEST_RUNNER_OBSINK_TEST_VAULT_KEY="$VAULT_KEY" \
        xcodebuild test-without-building \
            -xctestrun "$XCTESTRUN" \
            -destination "platform=iOS Simulator,name=$SIM_NAME" \
            -only-testing:"ObSinkUITests/SyncE2ETests/$test_name" \
            >"$log" 2>&1; then
        return 0
    else
        tail -30 "$log"
        return 1
    fi
}

# App-group container inside the simulator, readable from the host. The
# vault's files live under Vault/<vault id>/ (one directory per vault).
app_vault_dir() {
    xcrun simctl get_app_container "$SIM_NAME" "$APP_BUNDLE_ID" groups \
        | awk -F'\t' '/group\.com\.obsink\.shared/ {print $2}'
}

step "Booting simulator '$SIM_NAME'"
xcrun simctl boot "$SIM_NAME" 2>/dev/null || true
xcrun simctl bootstatus "$SIM_NAME" -b >/dev/null

step "Building the CLI, the app and the UI tests for testing (signed, team=${DEVELOPMENT_TEAM:-none})"
( cd "$REPO_ROOT" && cargo build -q -p obsink )
( cd "$REPO_ROOT/ios" && xcodegen generate >/dev/null )
xcodebuild build-for-testing \
    -project "$REPO_ROOT/ios/ObSink.xcodeproj" -scheme ObSink \
    -destination "platform=iOS Simulator,name=$SIM_NAME" \
    -derivedDataPath "$DERIVED" >"$WORK/build.log" 2>&1 \
    || { tail -30 "$WORK/build.log"; exit 1; }
XCTESTRUN="$(ls -t "$DERIVED"/Build/Products/*.xctestrun | head -1)"

step "Fresh app install"
xcrun simctl uninstall "$SIM_NAME" "$APP_BUNDLE_ID" 2>/dev/null || true
xcrun simctl install "$SIM_NAME" "$DERIVED/Build/Products/Debug-iphonesimulator/ObSink.app"

step "Device A: sign up '$EMAIL' (sets the passphrase), init vault '$VAULT_NAME' with starter files"
INVITE_ARGS=()
[ -n "${OBSINK_INVITE_CODE:-}" ] && INVITE_ARGS=(--invite-code "$OBSINK_INVITE_CODE")
cli login --email "$EMAIL" --device-name "CLI (device A)" "${INVITE_ARGS[@]}" >"$WORK/cli-login.log" 2>&1 \
    || { cat "$WORK/cli-login.log"; exit 1; }
LOGIN_AT="$(date +%s)"
echo "# Hello from Mac" > "$A_VAULT/hello.md"
mkdir -p "$A_VAULT/notes"
echo "note one" > "$A_VAULT/notes/note1.md"
cli init --vault-name "$VAULT_NAME" --directory "$A_VAULT" >"$WORK/cli-init.log" 2>&1 \
    || { cat "$WORK/cli-init.log"; exit 1; }
VAULT_ID="$(sed -n 's/^vault_id = "\(.*\)"/\1/p' "$A_HOME/.obsink/config.toml")"
[ -n "$VAULT_ID" ] || { echo "no vault id"; exit 1; }
echo "vault: $VAULT_ID"

# What device B's seeded phases need, straight from A's keyring (spec §6.3).
BEARER="$(keyring_entry "bearer:$SERVER_URL")"
USER_ID="$(keyring_entry "user:$SERVER_URL")"
ACCOUNT_ENTRY="$(keyring_entry "account:$USER_ID")"
ACCOUNT_KEY="${ACCOUNT_ENTRY%%:*}"
ACCOUNT_KEY_ID="${ACCOUNT_ENTRY#*:}"
VAULT_KEY="$(keyring_entry "$VAULT_ID")"
[ ${#ACCOUNT_KEY} -eq 64 ] && [ ${#VAULT_KEY} -eq 64 ] || { echo "keyring entries not found"; exit 1; }

# The phases after the sign-in one reuse the phone's own session; when that
# phase fails, device A's session is seeded so the rest still runs.
SEED_ACCOUNT=0

# The vault is deleted at the end whatever happens.
cleanup() {
    if [ -n "${VAULT_ID:-}" ]; then
        curl -s -o /dev/null -w "cleanup: DELETE vault $VAULT_ID -> %{http_code}\n" -X DELETE \
            "$SERVER_URL/vaults/$VAULT_ID" -H "Authorization: Bearer $BEARER" || true
    fi
}
trap cleanup EXIT

# ---------- Spec §12.1 / §12.3: sign in, unlock and download through the app UI ----------
step "Spec §12: device B signs in, unlocks with the passphrase and downloads the vault"
# The server refuses a second code for one address within 60 s of device A's.
WAIT=$((LOGIN_AT + 61 - $(date +%s)))
[ "$WAIT" -gt 0 ] && { echo "waiting ${WAIT}s for the email cooldown"; sleep "$WAIT"; }
if run_test testSignInUnlockAndDownload; then
    B_VAULT="$(app_vault_dir)/Vault/$VAULT_ID"
    if [ "$(cat "$B_VAULT/hello.md" 2>/dev/null)" = "# Hello from Mac" ] \
        && [ "$(cat "$B_VAULT/notes/note1.md" 2>/dev/null)" = "note one" ]; then
        pass "Spec §12: sign in, unlock, download; OBS-29: Mac → server → iOS propagation"
    else
        fail "OBS-29: files missing/incorrect in app container ($B_VAULT)"
    fi
else
    fail "Spec §12: sign in / unlock / download flow"
    SEED_ACCOUNT=1
fi
B_VAULT="$(app_vault_dir)/Vault/$VAULT_ID"

# ---------- Spec §15.3: the Devices tab ----------
step "Spec §15.3: the Devices tab lists device A and this phone"
if run_test testDevicesTabListsBothDevices; then
    pass "Spec §15.3: both devices listed"
else
    fail "Spec §15.3: devices tab"
fi
if cli devices | grep -q "e2e-sim"; then
    pass "Spec §4.1: device A sees device B on the account"
else
    fail "Spec §4.1: device B missing from the CLI's device list"
fi

# ---------- OBS-19 (sim half) / OBS-29: Files app shows the vault ----------
# Known limitation: on the iOS 26 simulator, fileproviderd never instantiates
# third-party replicated FP extensions (libxpc assertion; the listing hangs at
# LOADING). The extension logic is unit-tested; the visual Files/Obsidian check
# is the on-device half of OBS-19. Reported as WARN, not FAIL.
step "OBS-19-sim: ObSink location in the Files app"
if run_test testFilesAppShowsVault TEST_RUNNER_OBSINK_TEST_EXPECT_FILE=hello.md; then
    pass "OBS-19-sim: File Provider domain visible in Files"
else
    echo "WARN: OBS-19-sim: Files listing unavailable on simulator (replicated FP not instantiated by fileproviderd); verify on device (OBS-19)"
fi

# ---------- OBS-30: edit on iOS → sync → appears on Mac ----------
step "OBS-30: edit on iOS propagates to Mac"
echo "edited on iOS" > "$B_VAULT/hello.md"
if run_test testSyncNow; then
    cli sync >"$WORK/cli-obs30.log" 2>&1
    if [ "$(cat "$A_VAULT/hello.md")" = "edited on iOS" ]; then
        pass "OBS-30: iOS → server → Mac propagation"
    else
        fail "OBS-30: Mac copy not updated"
    fi
else
    fail "OBS-30: app sync failed"
fi

# ---------- OBS-33 / OBS-107: server ahead on open → auto-sync pulls it ----------
step "OBS-107: launch auto-sync pulls a file made while iOS was away"
echo "made while iOS away" > "$A_VAULT/notes/late.md"
cli sync >"$WORK/cli-obs33.log" 2>&1
if run_test testAutoSyncPullsRemote \
    && [ "$(cat "$B_VAULT/notes/late.md" 2>/dev/null)" = "made while iOS away" ]; then
    pass "OBS-107: auto-sync on launch pulled the server's file"
else
    fail "OBS-107: auto-sync on launch did not pull the server's file"
fi

# ---------- OBS-32: deletion propagates (both directions) ----------
step "OBS-32: delete on Mac → gone on iOS"
rm "$A_VAULT/notes/late.md"
cli sync >"$WORK/cli-obs32a.log" 2>&1
if run_test testSyncNow && [ ! -f "$B_VAULT/notes/late.md" ]; then
    pass "OBS-32: Mac deletion propagated to iOS"
else
    fail "OBS-32: Mac deletion did not propagate"
fi

step "OBS-32: delete on iOS → gone on Mac"
rm "$B_VAULT/notes/note1.md"
if run_test testSyncNow; then
    cli sync >"$WORK/cli-obs32b.log" 2>&1
    if [ ! -f "$A_VAULT/notes/note1.md" ]; then
        pass "OBS-32: iOS deletion propagated to Mac"
    else
        fail "OBS-32: iOS deletion did not propagate"
    fi
else
    fail "OBS-32: app sync failed"
fi

# ---------- OBS-31: conflict resolution, all three choices ----------
conflict_round() {
    local choice="$1" tag="$2"
    step "OBS-31 ($choice)"
    # Rebaseline both sides on the same content.
    echo "BASE-$tag" > "$A_VAULT/hello.md"
    cli sync >"$WORK/cli-obs31-$tag-base.log" 2>&1
    run_test testSyncNow || { fail "OBS-31 ($choice): rebaseline sync"; return; }
    # Diverge: A pushes REMOTE, iOS edits LOCAL without syncing. Both sides
    # changed since the shared base, so the three-way diff flags a conflict.
    echo "REMOTE-$tag" > "$A_VAULT/hello.md"
    cli sync >"$WORK/cli-obs31-$tag-remote.log" 2>&1
    echo "LOCAL-$tag" > "$B_VAULT/hello.md"
    if ! run_test testResolveConflict TEST_RUNNER_OBSINK_TEST_CHOICE="$choice"; then
        fail "OBS-31 ($choice): resolve flow"
        return
    fi
    cli sync >"$WORK/cli-obs31-$tag-verify.log" 2>&1
    case "$choice" in
        "Keep local")
            [ "$(cat "$A_VAULT/hello.md")" = "LOCAL-$tag" ] \
                && pass "OBS-31 ($choice): local won on both sides" \
                || fail "OBS-31 ($choice): expected LOCAL-$tag on Mac, got '$(cat "$A_VAULT/hello.md")'"
            ;;
        "Keep remote")
            [ "$(cat "$B_VAULT/hello.md")" = "REMOTE-$tag" ] \
                && pass "OBS-31 ($choice): remote won on iOS" \
                || fail "OBS-31 ($choice): expected REMOTE-$tag on iOS, got '$(cat "$B_VAULT/hello.md")'"
            ;;
        "Keep both")
            [ -f "$B_VAULT/hello.conflict.md" ] && [ -f "$A_VAULT/hello.conflict.md" ] \
                && pass "OBS-31 ($choice): conflict copy on both sides" \
                || fail "OBS-31 ($choice): hello.conflict.md missing"
            ;;
    esac
}
conflict_round "Keep remote" kr
conflict_round "Keep local"  kl
conflict_round "Keep both"   kb

# ---------- OBS-34: realistic Obsidian vault ----------
step "OBS-34: real-vault shape (.obsidian config, image, nesting)"
mkdir -p "$A_VAULT/.obsidian" "$A_VAULT/daily/2026/08"
printf '{"theme":"obsidian","livePreview":true}' > "$A_VAULT/.obsidian/app.json"
printf '{"main":{"id":"w1"}}' > "$A_VAULT/.obsidian/workspace.json"
echo "- [ ] task" > "$A_VAULT/daily/2026/08/25.md"
# A real binary attachment (non-UTF8): 64x64 PNG.
python3 - "$A_VAULT/attachment.png" <<'EOF'
import struct, sys, zlib
w = h = 64
raw = b"".join(b"\x00" + bytes([x*4, y*4, 128] * 1) * w for y in range(h) for x in [1])
def chunk(t, d): c = t + d; return struct.pack(">I", len(d)) + c + struct.pack(">I", zlib.crc32(c))
png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)) \
    + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b"")
open(sys.argv[1], "wb").write(png)
EOF
cli sync >"$WORK/cli-obs34.log" 2>&1
if run_test testSyncNow; then
    # Compare files only: manifests don't track directories, so an empty dir
    # left behind by a deletion is expected to differ between devices; the
    # workspace files never sync (the built-in ignore list).
    ( cd "$A_VAULT" && find . -type f ! -path "./.obsink/*" ! -name .DS_Store ! -name 'workspace*.json' | sort ) >"$WORK/obs34-a.txt"
    ( cd "$B_VAULT" && find . -type f ! -path "./.obsink/*" ! -name .DS_Store ! -name 'workspace*.json' | sort ) >"$WORK/obs34-b.txt"
    OBS34_OK=1
    diff "$WORK/obs34-a.txt" "$WORK/obs34-b.txt" >"$WORK/obs34-diff.log" 2>&1 || OBS34_OK=0
    if [ "$OBS34_OK" = 1 ]; then
        while IFS= read -r f; do
            cmp -s "$A_VAULT/$f" "$B_VAULT/$f" || { echo "content differs: $f" >>"$WORK/obs34-diff.log"; OBS34_OK=0; }
        done <"$WORK/obs34-a.txt"
    fi
    if [ "$OBS34_OK" = 1 ]; then
        pass "OBS-34: all files byte-identical (incl. .obsidian + binary attachment)"
    else
        fail "OBS-34: files differ — see $WORK/obs34-diff.log"
    fi
else
    fail "OBS-34: app sync failed"
fi

# ===== OBS-100: Remove from this device =====
# The vault leaves the phone; its cache directory, item database, and File
# Provider location go with it, the server drops this device from the vault,
# and the card offers Download again.
step "OBS-100: remove the vault from the device"
if run_test testRemoveVaultFromDevice; then
    APP_GROUP="$(app_vault_dir)"
    LEFT=""
    [ -e "$APP_GROUP/Vault/$VAULT_ID" ] && LEFT="$LEFT Vault/$VAULT_ID"
    [ -e "$APP_GROUP/items-$VAULT_ID.sqlite" ] && LEFT="$LEFT items-$VAULT_ID.sqlite"
    if [ -z "$LEFT" ]; then
        pass "OBS-100: vault removed from the device (cache dir and item DB gone)"
    else
        fail "OBS-100: leftovers after removal:$LEFT"
    fi
    if cli vaults | grep "$VAULT_ID" | grep -q "1 device(s)"; then
        pass "Spec §4.3: the server no longer lists device B on the vault"
    else
        fail "Spec §4.3: device B still attached after removal: $(cli vaults | grep "$VAULT_ID")"
    fi
else
    fail "OBS-100: remove-vault UI phase failed"
fi

printf '\n==== Result: %d passed, %d failed. Logs: %s ====\n' "$PASS_COUNT" "$FAIL_COUNT" "$WORK"
[ "$FAIL_COUNT" -eq 0 ]
