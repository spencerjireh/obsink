# Architecture

This document is for contributors. It describes how ObSink syncs a folder and what the wire format looks like. For the product spec, see [spec.md](../spec.md).

## Components

```
┌─────────────────────┐      HTTPS      ┌──────────────────────────────┐
│ Clients             │  Bearer token   │ obsink-server (Rust, axum)    │
│  - obsink CLI       │ ───────────────►│  - accounts, invites, routing │
│  - Tauri desktop    │  (via a TLS     │  - conflict gating (one tx)   │
│  - iOS app          │   proxy)        │  - envelope encryption        │
│  (all wrap core/)   │ ◄───────────────│  - version/trash retention    │
└─────────────────────┘                 └───────────┬──────────────────┘
          │                                          │
   builds/diffs                              ┌───────┴────────────────┐
   manifests, encrypts                       │ Postgres  (manifests,  │
   files locally                             │            accounts)   │
                                             │ volume    (blobs)      │
                                             └────────────────────────┘
```

The **Rust core** (`core/`) holds all the logic worth sharing across platforms. The server is deliberately thin: it treats paths and hashes as opaque strings and never decrypts vault content.

## The sync cycle

`prepare_sync` → (resolve conflicts) → `complete_sync`:

1. **Build the working manifest** from the local folder (`build_manifest_from_dir`): walk files, record a keyed content hash, size, and mtime. The on-disk `.obsink/manifest.json` from the last sync is used to detect local deletions.
2. **Fetch the remote manifest** (`fetch_remote_manifest`) and re-key it by real path. The last manifest and its `ETag` are cached in `.obsink/remote-manifest.json`; the fetch sends `If-None-Match` and reuses the cache on `304`. A corrupt or missing cache falls back to an unconditional fetch.
3. **Diff** local vs remote (`diff_manifests`) into three lists:
   - **upload** — local is newer or new
   - **download** — remote is newer or new
   - **conflict** — both changed since the last common state (decided by mtime; equal mtime + different hash ⇒ conflict)
4. **Apply downloads** immediately; return uploads + conflicts as a `SyncPlan`.
5. The UI/CLI resolves conflicts (keep local / keep remote / keep both).
6. `complete_sync` applies resolutions, uploads pending changes (each PUT is conflict-gated by the server via `X-Parent-Hash`), handles any late 409s, and saves the new remote manifest to disk. Transfers are best-effort: a per-file failure (e.g. a 413) is recorded and skipped while the batch continues; a fatal failure (network down, auth) stops the batch. When there are no late conflicts and no fatal failure, the server manifest is re-fetched and saved even on partial success — the resume point for the next sync. Per-file failures come back as `SyncFailure { path, kind, error, fatal }` on the `SyncResult`.

The engine **never auto-resolves** a conflict — that's a UI decision.

## Wire format (v2)

`PROTOCOL_VERSION = 2`. The guiding principle: **the server learns nothing about your vault**.

### Key derivation

The passphrase + vault ID (salt) go through Argon2id (64 MiB / 3 iterations / 1 lane) to produce a 32-byte **master key**. The master key is never used directly — it's HKDF-SHA256 input keying material for four purpose-separated sub-keys (`derive_keys`):

| Sub-key | Used for |
|---|---|
| `content_enc` | AES-256-GCM encryption of file contents |
| `content_mac` | HMAC-SHA256 of plaintext contents → the manifest `hash` |
| `path_token` | Deterministic `HMAC(path)` → the per-file server token |
| `path_enc`  | AES-256-GCM of the real path → `encPath` |

### Manifest

The server stores a manifest per vault, keyed by **path token** (not the real path):

```jsonc
{
  "5f337b…": {                  // HMAC(path_token_key, "notes/today.md")
    "hash": "0a9332…",          // HMAC(content_mac_key, plaintext)
    "modified": 1780641294,
    "size": 48,
    "deleted": false,
    "encPath": "Xaf4zXtq…"      // AES-GCM(path_enc_key, "notes/today.md"), base64
  }
}
```

- **Why a token, not the path?** So the server (and anyone with access to its storage) cannot see filenames. The token is deterministic, so two devices independently compute the same token for the same path — which is what makes diffing work without coordination.
- **Why `encPath`?** A freshly-connected device pulls a manifest of tokens it can't reverse (the token is a one-way HMAC). `encPath` is reversible AES-GCM, so the client recovers the real filename and re-keys the manifest by real path locally.
- **Why HMAC the content, not SHA-256?** A plaintext SHA-256 would let the server confirm whether you store a known file. A keyed HMAC reveals nothing without the key, while still being a stable equality check for conflict detection.

### Conflict gating

On `PUT`/`DELETE` the client sends `X-Parent-Hash` (the hash it believes is current). The server compares it to the stored manifest hash inside the write transaction; on mismatch it returns `409` with the current server entry. This makes retried/racing writes safe — they either succeed or surface a conflict, never silently clobber.

## Network resilience

`ApiClient` applies a 30s per-request timeout and retries transient failures (timeouts, connection errors) up to 3 times with exponential backoff. HTTP status errors and non-transient errors surface immediately as typed `ApiError`s. Logging is via `tracing` (`debug` per request, `info` per sync plan, `warn` on retry).

### Partial-sync recovery

Above the per-request retry, the sync engine is partial-sync aware. Each non-conflict transfer error is classified **fatal** (`ApiError::Http`, or `UnexpectedStatus` 401/403/5xx → stop the batch) or **per-file** (413/404/other 4xx, local crypto/IO → record and continue). Recorded failures land on `SyncResult.failures` as `SyncFailure { path, kind, error, fatal }`. Because the manifest checkpoints on partial success (sync-cycle step 6), a dropped sync resumes on the next run — hash-based diffing means already-pushed files are skipped automatically.

### Progress reporting

Sync is observable through a `ProgressSink` trait (`Phase` / `FileStarted` / `FileCompleted` / `FileFailed` / `Done` events) that each facade adapts to its native channel: the CLI prints to stderr, the desktop emits a `sync://progress` Tauri event, and iOS receives a UniFFI `ProgressListener` callback. Callers with no UI pass `NoProgress`.

## Server storage

Postgres tables (`server/migrations/0001_init.sql`):

- `users` — id, sealed email, sealed Apple subject, keyed-HMAC lookup columns
- `sessions` — id, `sha256(token)`, sealed device name, created, expires (180 days)
- `email_codes`, `invites` — one-time codes (keyed HMAC, attempts, cooldown) and invite codes
- `vaults` — id, tenant (`default` for the operator, else the user id), sealed name, `max_file_size`, `revision`
- `files` — one row per manifest entry: `(vault_id, path token, hash, modified, size, deleted, enc_path)`

Blob volume (`OBSINK_DATA_DIR/blobs`), file names are `sha256(path token)`:

- Live: `live/<vaultId>/<ab>/<hash>`
- Version (on overwrite): `_versions/<vaultId>/<hash>/<unixSeconds>[-n]`
- Trash (on delete): `_trash/<vaultId>/<hash>/<unixSeconds>[-n]`

Every write (`PUT`, `DELETE`, each batch operation) is one transaction: lock the vault row `FOR UPDATE`, read the current entry, check size and quota, compare `X-Parent-Hash`, move the old blob aside, write the new one, upsert the row, bump `revision`. Two devices racing on one path get exactly one `200` and one `409`. `revision` is the manifest `ETag`.

Envelope encryption (`server/src/crypto.rs`): `OBSINK_SERVER_KEY` is HKDF input for three sub-keys — blob wrapping, column sealing (AES-GCM with a `<table>.<column>:<row id>` AAD so ciphertexts cannot be moved between rows), and keyed lookup HMACs. Path tokens, content hashes, and `encPath` are stored as the client sent them: they are already HMACs or ciphertext under the vault key.

The retention task (`server/src/retention.rs`) runs at startup and every `RETENTION_INTERVAL_SECS`: prune `_versions/` (keep newest 10 per file / 14 days) and `_trash/` (30 days), delete expired sessions and day-old codes, and remove blob directories whose vault row is gone.

## Testing

- `core/` — unit tests for crypto (round-trips, sub-key separation, HMAC, path tokens), hashing, manifest diffing, the ETag cache, multipart batch encoding, and async sync flows against a mock HTTP server (`httpmock`).
- `server/` — unit tests for the envelope, key loading, and blob paths; integration tests (`server/tests/`) spin up the real router on a throwaway Postgres database per test and drive it with `reqwest` and the core `ApiClient`/`AuthClient`. They need `DATABASE_URL` and skip without it; CI sets `OBSINK_TEST_REQUIRE_DB=1`.
- `scripts/verify-server-deploy.sh` (curl contract check) and `scripts/verify-cli-deployed-sync.sh` (two CLI devices) — live checks against a running server; `scripts/verify-ios-sim-e2e.sh` drives the iOS simulator against the same server.
