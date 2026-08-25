#!/usr/bin/env bash

set -euo pipefail

OUTPUT_PATH="${1:-worker/wrangler.toml}"

: "${WORKER_NAME:?WORKER_NAME is required}"
: "${KV_NAMESPACE_ID:?KV_NAMESPACE_ID is required}"
: "${R2_BUCKET_NAME:?R2_BUCKET_NAME is required}"

COMPATIBILITY_DATE="${COMPATIBILITY_DATE:-2026-04-14}"
MAX_BATCH_INLINE_BYTES="${MAX_BATCH_INLINE_BYTES:-52428800}"
# Hosted-mode (accounts) settings. Leave APPLE_CLIENT_IDS empty on a
# self-hosted deployment that only uses API_KEY; email sign-in additionally
# needs the RESEND_API_KEY secret (wrangler secret put).
APPLE_CLIENT_IDS="${APPLE_CLIENT_IDS:-}"
MAIL_FROM="${MAIL_FROM:-ObSink <onboarding@resend.dev>}"
MAX_VAULTS_PER_USER="${MAX_VAULTS_PER_USER:-10}"
MAX_VAULT_BYTES="${MAX_VAULT_BYTES:-1073741824}"

cat > "$OUTPUT_PATH" <<EOF
name = "$WORKER_NAME"
main = "src/index.ts"
compatibility_date = "$COMPATIBILITY_DATE"

[[kv_namespaces]]
binding = "META"
id = "$KV_NAMESPACE_ID"

[[r2_buckets]]
binding = "FILES"
bucket_name = "$R2_BUCKET_NAME"

[vars]
MAX_BATCH_INLINE_BYTES = $MAX_BATCH_INLINE_BYTES
APPLE_CLIENT_IDS = "$APPLE_CLIENT_IDS"
MAIL_FROM = "$MAIL_FROM"
MAX_VAULTS_PER_USER = "$MAX_VAULTS_PER_USER"
MAX_VAULT_BYTES = "$MAX_VAULT_BYTES"

[triggers]
crons = ["0 3 * * *", "30 3 * * *"]
EOF

printf 'Wrote %s\n' "$OUTPUT_PATH"
