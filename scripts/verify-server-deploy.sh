#!/usr/bin/env bash
#
# Contract check for a running ObSink server as a signed-in account (wire
# format v3): the protocol, the account key and device routes, the vault list
# shape, the vault lifecycle, manifest ETag / 304, conflict gating, multipart
# batch, soft delete, history and trash reads, invites. Needs curl, node and
# openssl.
#   set -a; . ./.env.deploy; set +a; scripts/verify-server-deploy.sh
#
# OBSINK_BEARER is a session of the harness account (minted once with
# `obsink login`), whose passphrase is set: the vault create sends a wrapped
# key, which the server only checks for shape.

set -euo pipefail

: "${OBSINK_SERVER_URL:?OBSINK_SERVER_URL is required}"
: "${OBSINK_BEARER:?OBSINK_BEARER is required (a session token from obsink login)}"

BASE_URL="${OBSINK_SERVER_URL%/}"
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
    -H "Authorization: Bearer $OBSINK_BEARER"
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
    -H "Authorization: Bearer $OBSINK_BEARER"
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

printf 'Verifying server at %s\n' "$BASE_URL"

# Spec §15.5: the wire format, and no operator bearer advertised.
LAST_BODY="$TMP_DIR/root.json"
LAST_STATUS="$(curl -sS -o "$LAST_BODY" -w '%{http_code}' -H 'Accept: application/json' "$BASE_URL/")"
assert_status 200
assert_json 'data.protocol' 3
assert_json 'data.auth.api_key === undefined' true

# Spec §4.1: the account, its devices (this session's device tagged), the key.
request_json GET "$BASE_URL/auth/me"
assert_status 200
assert_json 'typeof data.user.id' string
assert_json 'data.devices.filter((device) => device.current).length' 1
assert_json 'data.devices.every((device) => ["macos","ios","browser","cli"].includes(device.platform))' true
USER_ID="$(json_eval 'data.user.id')"
DEVICE_ID="$(json_eval 'data.devices.find((device) => device.current).id')"

request_json GET "$BASE_URL/auth/keys"
assert_status 200
assert_json 'typeof data.account_key.key_id' string
assert_json 'typeof data.account_key.wrapped' string
assert_json 'typeof data.account_key.salt' string
# The first set is create-only: a second one answers 409 with the winner.
request_json PUT "$BASE_URL/auth/keys" "$(node -e '
  const b = (n) => Buffer.alloc(n, 1).toString("base64");
  process.stdout.write(JSON.stringify({ wrapped: b(60), salt: b(16), verifier: b(32) }));
')"
assert_status 409
assert_json 'typeof data.account_key.key_id' string

# A sign-in without the device object is a client from before v3.
request_json POST "$BASE_URL/auth/email/verify" '{"email":"nobody@example.invalid","code":"000000","device_name":"old"}'
assert_status 400
assert_json 'data.error' 'update ObSink to continue'

# Vaults: the wrapped key is required, and the list carries the v3 fields.
VAULT_NAME="verify-$(date +%s)"
WRAPPED="$(openssl rand -base64 60 | tr -d '\n')"
request_json POST "$BASE_URL/vaults" "{\"name\":\"$VAULT_NAME\",\"max_file_size\":1024}"
assert_status 400
assert_json 'data.error' 'wrapped_key is required'
request_json POST "$BASE_URL/vaults" "{\"name\":\"$VAULT_NAME\",\"max_file_size\":1024,\"wrapped_key\":\"$WRAPPED\"}"
assert_status 201
VAULT_ID="$(json_eval 'data.vault.id')"
assert_json 'data.vault.revision' 0
assert_json 'data.vault.wrapped_key' "$WRAPPED"
printf 'Created vault %s\n' "$VAULT_ID"

request_json PUT "$BASE_URL/vaults/$VAULT_ID/devices/self" '{}'
assert_status 204

request_json GET "$BASE_URL/vaults"
assert_status 200
assert_json 'data.vaults.some((vault) => vault.id === args[0])' true "$VAULT_ID"
assert_json 'data.vaults.find((vault) => vault.id === args[0]).wrapped_key' "$WRAPPED" "$VAULT_ID"
assert_json 'data.vaults.find((vault) => vault.id === args[0]).devices.some((device) => device.id === args[1])' true "$VAULT_ID" "$DEVICE_ID"

request_json PATCH "$BASE_URL/vaults/$VAULT_ID" "{\"name\":\"$VAULT_NAME-renamed\"}"
assert_status 204
request_json GET "$BASE_URL/vaults"
assert_status 200
assert_json 'data.vaults.find((vault) => vault.id === args[0]).name' "$VAULT_NAME-renamed" "$VAULT_ID"

request_json GET "$BASE_URL/vaults/$VAULT_ID/manifest"
assert_status 200
assert_json 'Object.keys(data).length' 0

request_bytes PUT "$BASE_URL/vaults/$VAULT_ID/files/note.md" 'hello server' 'X-Content-Hash: hash-1' 'X-Enc-Path: enc-note'
assert_status 200

# Manifest ETag: a second GET with If-None-Match must return 304 with no body.
ETAG="$(curl -sS -D - -o /dev/null -H "Authorization: Bearer $OBSINK_BEARER" "$BASE_URL/vaults/$VAULT_ID/manifest" | awk 'tolower($1)=="etag:" {print $2}' | tr -d '\r')"
if [[ -z "$ETAG" ]]; then
  printf 'Manifest response carried no ETag\n' >&2
  exit 1
fi
LAST_BODY="$TMP_DIR/etag.bin"
LAST_STATUS="$(curl -sS -o "$LAST_BODY" -w '%{http_code}' -H "Authorization: Bearer $OBSINK_BEARER" -H "If-None-Match: $ETAG" "$BASE_URL/vaults/$VAULT_ID/manifest")"
assert_status 304

# The checkpoint report: this device now holds revision 1, and the list says so.
REVISION="$(printf '%s' "$ETAG" | tr -d '"')"
request_json PUT "$BASE_URL/vaults/$VAULT_ID/devices/self" "{\"revision\":$REVISION}"
assert_status 204
request_json GET "$BASE_URL/vaults"
assert_status 200
assert_json 'String(data.vaults.find((vault) => vault.id === args[0]).devices.find((device) => device.id === args[1]).last_revision)' "$REVISION" "$VAULT_ID" "$DEVICE_ID"
assert_json 'String(data.vaults.find((vault) => vault.id === args[0]).revision)' "$REVISION" "$VAULT_ID"

request_json GET "$BASE_URL/vaults/$VAULT_ID/manifest"
assert_status 200
assert_json 'data["note.md"].hash' hash-1

LAST_BODY="$TMP_DIR/file.bin"
LAST_STATUS="$(curl -sS -X GET -H "Authorization: Bearer $OBSINK_BEARER" -o "$LAST_BODY" -w '%{http_code}' "$BASE_URL/vaults/$VAULT_ID/files/note.md")"
assert_status 200
if [[ "$(cat "$LAST_BODY")" != 'hello server' ]]; then
  printf 'Unexpected file payload\n' >&2
  exit 1
fi

request_bytes PUT "$BASE_URL/vaults/$VAULT_ID/files/note.md" 'stale write' 'X-Parent-Hash: stale' 'X-Content-Hash: hash-2'
assert_status 409
assert_json 'data.current.hash' hash-1

# An overwrite archives the earlier version (spec §8.2).
request_bytes PUT "$BASE_URL/vaults/$VAULT_ID/files/note.md" 'hello again' 'X-Parent-Hash: hash-1' 'X-Content-Hash: hash-1b' 'X-Enc-Path: enc-note'
assert_status 200
request_json GET "$BASE_URL/vaults/$VAULT_ID/history/note.md"
assert_status 200
assert_json 'data.versions.length >= 1' true
VERSION_NAME="$(json_eval 'data.versions[0].name')"
LAST_BODY="$TMP_DIR/version.bin"
LAST_STATUS="$(curl -sS -X GET -H "Authorization: Bearer $OBSINK_BEARER" -o "$LAST_BODY" -w '%{http_code}' "$BASE_URL/vaults/$VAULT_ID/versions/$VERSION_NAME/note.md")"
assert_status 200
if [[ "$(cat "$LAST_BODY")" != 'hello server' ]]; then
  printf 'Unexpected archived version payload\n' >&2
  exit 1
fi

# Batch is multipart/form-data: an `operations` JSON part plus one `content`
# part per put, named by operation index.
printf 'second' > "$TMP_DIR/second.bin"
printf 'fresh' > "$TMP_DIR/fresh.bin"
cat > "$TMP_DIR/ops.json" <<'JSON'
{"operations":[{"action":"put","path":"note.md","parentHash":"stale","contentHash":"hash-2"},{"action":"put","path":"fresh.md","contentHash":"hash-3"}]}
JSON
LAST_BODY="$TMP_DIR/batch.json"
LAST_STATUS="$(curl -sS -o "$LAST_BODY" -w '%{http_code}' -H "Authorization: Bearer $OBSINK_BEARER" \
  -F "operations=@$TMP_DIR/ops.json;type=application/json" \
  -F "content=@$TMP_DIR/second.bin;filename=0;type=application/octet-stream" \
  -F "content=@$TMP_DIR/fresh.bin;filename=1;type=application/octet-stream" \
  "$BASE_URL/vaults/$VAULT_ID/batch")"
assert_status 200
assert_json 'data.results.map((result) => result.status).join(",")' 409,200

LAST_BODY="$TMP_DIR/delete.json"
LAST_STATUS="$(curl -sS -X DELETE -H "Authorization: Bearer $OBSINK_BEARER" -H 'X-Parent-Hash: hash-1b' -o "$LAST_BODY" -w '%{http_code}' "$BASE_URL/vaults/$VAULT_ID/files/note.md")"
assert_status 200

request_json GET "$BASE_URL/vaults/$VAULT_ID/manifest"
assert_status 200
assert_json 'String(data["note.md"].deleted)' true
assert_json 'data["fresh.md"].hash' hash-3

# The deletion is in the trash with its path token and its bytes (spec §9.3).
request_json GET "$BASE_URL/vaults/$VAULT_ID/trash"
assert_status 200
assert_json 'data.entries.some((entry) => entry.path === "note.md" && entry.encPath === "enc-note")' true
LAST_BODY="$TMP_DIR/trash.bin"
LAST_STATUS="$(curl -sS -X GET -H "Authorization: Bearer $OBSINK_BEARER" -o "$LAST_BODY" -w '%{http_code}' "$BASE_URL/vaults/$VAULT_ID/trash/note.md")"
assert_status 200
if [[ "$(cat "$LAST_BODY")" != 'hello again' ]]; then
  printf 'Unexpected trash payload\n' >&2
  exit 1
fi

request_json GET "$BASE_URL/auth/me"
assert_status 200
assert_json 'data.user.id' "$USER_ID"
assert_json 'data.usage.vaults.some((vault) => vault.id === args[0])' true "$VAULT_ID"

request_json POST "$BASE_URL/auth/invites" '{}'
assert_status 201
assert_json 'typeof data.invite.code === "string" && data.invite.code.length > 0' true

request_json DELETE "$BASE_URL/vaults/$VAULT_ID/devices/self"
assert_status 204
request_json DELETE "$BASE_URL/vaults/$VAULT_ID"
assert_status 204
printf 'Server verification passed for %s\n' "$VAULT_ID"
