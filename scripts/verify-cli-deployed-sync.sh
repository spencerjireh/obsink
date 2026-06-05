#!/usr/bin/env bash

set -euo pipefail

: "${WORKER_URL:?WORKER_URL is required}"
: "${WORKER_API_KEY:?WORKER_API_KEY is required}"

PASSPHRASE="${OBSINK_TEST_PASSPHRASE:-obsink-test-passphrase}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-$REPO_ROOT/target}"
CLI_BIN="$TARGET_DIR/debug/obsink"
TMP_DIR="$(mktemp -d)"
HOME_ONE="$TMP_DIR/home-one"
HOME_TWO="$TMP_DIR/home-two"
VAULT_ONE="$TMP_DIR/vault-one"
VAULT_TWO="$TMP_DIR/vault-two"

mkdir -p "$HOME_ONE" "$HOME_TWO" "$VAULT_ONE" "$VAULT_TWO"

VAULT_ID=''

cleanup() {
  if [[ -n "$VAULT_ID" ]]; then
    security delete-generic-password -s obsink -a "$VAULT_ID" >/dev/null 2>&1 || true
  fi
  rm -rf "$TMP_DIR"
}

trap cleanup EXIT

# Build once under the real environment so rustup can resolve the toolchain
# (run_cli overrides HOME, which would hide ~/.rustup from cargo/rustup).
printf 'Building obsink CLI\n'
(cd "$REPO_ROOT" && cargo build -q -p obsink)

run_cli() {
  local home_dir="$1"
  shift
  # Isolate per-device config via OBSINK_HOME; leave HOME alone so the macOS
  # keychain (used for the derived key) resolves against the real login keychain.
  OBSINK_HOME="$home_dir" "$CLI_BIN" "$@"
}

printf 'Creating deployed test vault via CLI\n'
run_cli "$HOME_ONE" init \
  --worker-url "$WORKER_URL" \
  --api-key "$WORKER_API_KEY" \
  --vault-name "verify-cli-$(date +%s)" \
  --directory "$VAULT_ONE" \
  --passphrase "$PASSPHRASE"

CONFIG_ONE="$HOME_ONE/.obsink/config.toml"
VAULT_ID="$(perl -ne 'print "$1\n" if /^vault_id\s*=\s*"([^"]+)"/' "$CONFIG_ONE")"

if [[ -z "$VAULT_ID" ]]; then
  printf 'Failed to discover vault_id from %s\n' "$CONFIG_ONE" >&2
  exit 1
fi

printf 'Created vault %s\n' "$VAULT_ID"

printf 'hello from device one\n' > "$VAULT_ONE/note.md"
run_cli "$HOME_ONE" sync

printf 'Connecting second device\n'
run_cli "$HOME_TWO" connect \
  --worker-url "$WORKER_URL" \
  --api-key "$WORKER_API_KEY" \
  --vault-id "$VAULT_ID" \
  --directory "$VAULT_TWO" \
  --passphrase "$PASSPHRASE"

cmp -s "$VAULT_ONE/note.md" "$VAULT_TWO/note.md"

printf 'device two edit\n' > "$VAULT_TWO/note.md"
run_cli "$HOME_TWO" sync
run_cli "$HOME_ONE" sync

if [[ "$(cat "$VAULT_ONE/note.md")" != 'device two edit' ]]; then
  printf 'Device one did not receive synced update\n' >&2
  exit 1
fi

printf 'device one conflict\n' > "$VAULT_ONE/note.md"
printf 'device two conflict\n' > "$VAULT_TWO/note.md"
run_cli "$HOME_ONE" sync
printf '2\n' | OBSINK_HOME="$HOME_TWO" "$CLI_BIN" sync

if [[ "$(cat "$VAULT_TWO/note.md")" != 'device one conflict' ]]; then
  printf 'Conflict resolution did not keep remote content on device two\n' >&2
  exit 1
fi

printf 'CLI deployed sync verification passed for %s\n' "$VAULT_ID"
