#!/usr/bin/env bash

set -euo pipefail

: "${WORKER_URL:?WORKER_URL is required}"
: "${WORKER_API_KEY:?WORKER_API_KEY is required}"

BASE_URL="${WORKER_URL%/}"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

LAST_BODY=''
LAST_STATUS=''

request_json() {
  local method="$1"
  local url="$2"
  local body="${3:-}"
  LAST_BODY="$TMP_DIR/body.json"

  local -a args=(
    -sS
    -X "$method"
    -H "Authorization: Bearer $WORKER_API_KEY"
    -H "Content-Type: application/json"
    -o "$LAST_BODY"
    -w '%{http_code}'
  )

  if [[ -n "$body" ]]; then
    args+=(--data "$body")
  fi

  LAST_STATUS="$(curl "${args[@]}" "$url")"
}

request_bytes() {
  local method="$1"
  local url="$2"
  local body="$3"
  shift 3
  LAST_BODY="$TMP_DIR/body.bin"

  local -a args=(
    -sS
    -X "$method"
    -H "Authorization: Bearer $WORKER_API_KEY"
    -o "$LAST_BODY"
    -w '%{http_code}'
    --data-binary "$body"
  )

  while (($#)); do
    args+=(-H "$1")
    shift
  done

  LAST_STATUS="$(curl "${args[@]}" "$url")"
}

assert_status() {
  local expected="$1"
  if [[ "$LAST_STATUS" != "$expected" ]]; then
    printf 'Expected HTTP %s, got %s\n' "$expected" "$LAST_STATUS" >&2
    cat "$LAST_BODY" >&2
    exit 1
  fi
}

json_eval() {
  local expression="$1"
  shift
  node -e '
    const fs = require("fs");
    const data = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
    const args = process.argv.slice(3);
    const value = Function("data", "args", `return (${process.argv[2]});`)(data, args);
    if (typeof value === "object") {
      process.stdout.write(JSON.stringify(value));
    } else {
      process.stdout.write(String(value));
    }
  ' "$LAST_BODY" "$expression" "$@"
}

assert_json() {
  local expression="$1"
  local expected="$2"
  shift 2
  local actual
  actual="$(json_eval "$expression" "$@")"
  if [[ "$actual" != "$expected" ]]; then
    printf 'Assertion failed for %s\nExpected: %s\nActual:   %s\n' "$expression" "$expected" "$actual" >&2
    cat "$LAST_BODY" >&2
    exit 1
  fi
}

printf 'Verifying Worker at %s\n' "$BASE_URL"

VAULT_NAME="verify-$(date +%s)"
request_json POST "$BASE_URL/vaults" "{\"name\":\"$VAULT_NAME\",\"max_file_size\":1024}"
assert_status 201
VAULT_ID="$(json_eval 'data.vault.id')"
printf 'Created vault %s\n' "$VAULT_ID"

request_json GET "$BASE_URL/vaults"
assert_status 200
assert_json 'data.some((vault) => vault.id === args[0])' true "$VAULT_ID"

request_json GET "$BASE_URL/vaults/$VAULT_ID/manifest"
assert_status 200
assert_json 'Object.keys(data).length' 0

request_bytes PUT "$BASE_URL/vaults/$VAULT_ID/files/note.md" 'hello worker' 'X-Content-Hash: hash-1'
assert_status 200

request_json GET "$BASE_URL/vaults/$VAULT_ID/manifest"
assert_status 200
assert_json 'data["note.md"].hash' hash-1

LAST_BODY="$TMP_DIR/file.bin"
LAST_STATUS="$(curl -sS -X GET -H "Authorization: Bearer $WORKER_API_KEY" -o "$LAST_BODY" -w '%{http_code}' "$BASE_URL/vaults/$VAULT_ID/files/note.md")"
assert_status 200
if [[ "$(cat "$LAST_BODY")" != 'hello worker' ]]; then
  printf 'Unexpected file payload\n' >&2
  exit 1
fi

request_bytes PUT "$BASE_URL/vaults/$VAULT_ID/files/note.md" 'stale write' 'X-Parent-Hash: stale' 'X-Content-Hash: hash-2'
assert_status 409
assert_json 'data.current.hash' hash-1

FRESH_BASE64="$(printf 'fresh' | base64 | tr -d '\n')"
SECOND_BASE64="$(printf 'second' | base64 | tr -d '\n')"
request_json POST "$BASE_URL/vaults/$VAULT_ID/batch" "{\"operations\":[{\"action\":\"put\",\"path\":\"note.md\",\"parentHash\":\"stale\",\"contentHash\":\"hash-2\",\"content\":\"$SECOND_BASE64\"},{\"action\":\"put\",\"path\":\"fresh.md\",\"contentHash\":\"hash-3\",\"content\":\"$FRESH_BASE64\"}]}"
assert_status 200
assert_json 'data.results.map((result) => result.status).join(",")' 409,200

LAST_BODY="$TMP_DIR/delete.json"
LAST_STATUS="$(curl -sS -X DELETE -H "Authorization: Bearer $WORKER_API_KEY" -H 'X-Parent-Hash: hash-1' -o "$LAST_BODY" -w '%{http_code}' "$BASE_URL/vaults/$VAULT_ID/files/note.md")"
assert_status 200

request_json GET "$BASE_URL/vaults/$VAULT_ID/manifest"
assert_status 200
assert_json 'String(data["note.md"].deleted)' true
assert_json 'data["fresh.md"].hash' hash-3

printf 'Worker verification passed for %s\n' "$VAULT_ID"
