#!/usr/bin/env bash

set -euo pipefail

OUTPUT_PATH="${1:-worker/wrangler.toml}"

: "${WORKER_NAME:?WORKER_NAME is required}"
: "${KV_NAMESPACE_ID:?KV_NAMESPACE_ID is required}"
: "${R2_BUCKET_NAME:?R2_BUCKET_NAME is required}"

COMPATIBILITY_DATE="${COMPATIBILITY_DATE:-2026-04-14}"
MAX_BATCH_INLINE_BYTES="${MAX_BATCH_INLINE_BYTES:-52428800}"

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

[triggers]
crons = ["0 3 * * *", "30 3 * * *"]
EOF

printf 'Wrote %s\n' "$OUTPUT_PATH"
