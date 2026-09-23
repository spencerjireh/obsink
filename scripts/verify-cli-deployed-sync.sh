#!/usr/bin/env bash
#
# Two-device CLI sync against a running server as one account (wire format
# v3): device one creates a vault, device two downloads it, both sync, then
# resolve a conflict; deletes the vault afterwards.
#   set -a; . ./.env.deploy; set +a; scripts/verify-cli-deployed-sync.sh
#
# OBSINK_BEARER is a session of the harness account (minted once with
# `obsink login`) and OBSINK_PASSPHRASE its passphrase. Device one seeds the
# session into its own file keyring and unlocks with the passphrase. Device
# two does the same on a server that hands out no codes (production: one
# session is one device, so both homes act as the harness device there), or,
# with OBSINK_HARNESS_EMAIL set against a dev server (the compose stack,
# `AUTH_DEV_RETURN_CODE=1`), signs in as its own device so the device list
# and the attachments are checked too.

set -euo pipefail

: "${OBSINK_SERVER_URL:?OBSINK_SERVER_URL is required}"
: "${OBSINK_BEARER:?OBSINK_BEARER is required (a session token from obsink login)}"
: "${OBSINK_PASSPHRASE:?OBSINK_PASSPHRASE is required (the harness account passphrase)}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-$REPO_ROOT/target}"
CLI_BIN="$TARGET_DIR/debug/obsink"
TMP_DIR="$(mktemp -d)"
HOME_ONE="$TMP_DIR/home-one"
HOME_TWO="$TMP_DIR/home-two"
VAULT_ONE="$TMP_DIR/vault-one"
VAULT_TWO="$TMP_DIR/vault-two"
SERVER_URL="${OBSINK_SERVER_URL%/}"

mkdir -p "$HOME_ONE/keyring" "$HOME_TWO/keyring" "$VAULT_ONE" "$VAULT_TWO"

VAULT_ID=''

cleanup() {
  if [[ -n "$VAULT_ID" ]]; then
    curl -sS -o /dev/null -X DELETE -H "Authorization: Bearer $OBSINK_BEARER" \
      "$SERVER_URL/vaults/$VAULT_ID" || true
  fi
  rm -rf "$TMP_DIR"
}

trap cleanup EXIT

# Build once under the real environment so rustup can resolve the toolchain.
printf 'Building obsink CLI\n'
(cd "$REPO_ROOT" && cargo build -q -p obsink)

# The file keyring names entries after the keychain account with `/` and `:`
# flattened (core `keychain::keyring_file`); the bearer goes where a sign-in
# would have put it.
seed_session() {
  local home_dir="$1"
  local canonical
  canonical="$(printf '%s' "$SERVER_URL" | tr '/:' '__')"
  printf '%s' "$OBSINK_BEARER" > "$home_dir/keyring/bearer_$canonical"
  chmod 600 "$home_dir/keyring/bearer_$canonical"
}

run_cli() {
  local home_dir="$1"
  local device="$2"
  shift 2
  # Per-device config (OBSINK_HOME), keyring (OBSINK_KEYRING_DIR) and device id.
  OBSINK_HOME="$home_dir" OBSINK_KEYRING_DIR="$home_dir/keyring" OBSINK_DEVICE_ID="$device" \
    OBSINK_SERVER_URL="$SERVER_URL" "$CLI_BIN" "$@"
}

seed_session "$HOME_ONE"
seed_session "$HOME_TWO"

printf 'Device one: unlock, create the vault\n'
run_cli "$HOME_ONE" verify-cli-one unlock
run_cli "$HOME_ONE" verify-cli-one init \
  --vault-name "verify-cli-$(date +%s)" \
  --directory "$VAULT_ONE"

CONFIG_ONE="$HOME_ONE/.obsink/config.toml"
VAULT_ID="$(perl -ne 'print "$1\n" if /^vault_id\s*=\s*"([^"]+)"/' "$CONFIG_ONE")"

if [[ -z "$VAULT_ID" ]]; then
  printf 'Failed to discover vault_id from %s\n' "$CONFIG_ONE" >&2
  exit 1
fi

printf 'Created vault %s\n' "$VAULT_ID"

printf 'hello from device one\n' > "$VAULT_ONE/note.md"
run_cli "$HOME_ONE" verify-cli-one sync

if [[ -n "${OBSINK_HARNESS_EMAIL:-}" ]]; then
  printf 'Device two: sign in as its own device, download the vault\n'
  run_cli "$HOME_TWO" verify-cli-two login --email "$OBSINK_HARNESS_EMAIL" --device-name "verify-cli-two"
  EXPECT_DEVICES=2
else
  printf 'Device two: unlock (the same session, so the same device on the server), download the vault\n'
  run_cli "$HOME_TWO" verify-cli-two unlock
  EXPECT_DEVICES=1
fi
run_cli "$HOME_TWO" verify-cli-two vaults | grep -q "$VAULT_ID"
run_cli "$HOME_TWO" verify-cli-two download \
  --vault-id "$VAULT_ID" \
  --directory "$VAULT_TWO"

cmp -s "$VAULT_ONE/note.md" "$VAULT_TWO/note.md"

# The devices that hold the vault are attached and reported their
# checkpoints (spec §4.3).
if [[ "$EXPECT_DEVICES" = 2 ]]; then
  run_cli "$HOME_ONE" verify-cli-one devices | grep -q "verify-cli-two"
fi
run_cli "$HOME_ONE" verify-cli-one vaults | grep "$VAULT_ID" | grep -q "$EXPECT_DEVICES device(s)"

printf 'device two edit\n' > "$VAULT_TWO/note.md"
run_cli "$HOME_TWO" verify-cli-two sync
run_cli "$HOME_ONE" verify-cli-one sync

if [[ "$(cat "$VAULT_ONE/note.md")" != 'device two edit' ]]; then
  printf 'Device one did not receive synced update\n' >&2
  exit 1
fi

# Both devices edit the same base: device one uploads, device two hits a
# three-way conflict and picks "keep remote" (choice 2).
printf 'device one conflict\n' > "$VAULT_ONE/note.md"
printf 'device two conflict\n' > "$VAULT_TWO/note.md"
run_cli "$HOME_ONE" verify-cli-one sync
printf '2\n' | run_cli "$HOME_TWO" verify-cli-two sync

if [[ "$(cat "$VAULT_TWO/note.md")" != 'device one conflict' ]]; then
  printf 'Conflict resolution did not keep remote content on device two\n' >&2
  exit 1
fi

# History: the overwrites archived versions; restoring one comes back as an
# upload on the next sync (spec §8.2).
VERSION="$(run_cli "$HOME_TWO" verify-cli-two history note.md | head -1 | awk '{print $1}')"
if [[ -z "$VERSION" || "$VERSION" == "no" ]]; then
  printf 'No archived version of note.md to restore\n' >&2
  exit 1
fi
run_cli "$HOME_TWO" verify-cli-two restore note.md --version "$VERSION"
# Captured, not piped: `grep -q` would close the pipe under the CLI mid-write.
RESTORE_SYNC="$(run_cli "$HOME_TWO" verify-cli-two sync 2>&1)"
grep -q "1 uploaded" <<<"$RESTORE_SYNC"

printf 'CLI deployed sync verification passed for %s\n' "$VAULT_ID"
