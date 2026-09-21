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

1. **Load local state** (`load_local_state`): walk the folder into a working manifest (keyed content hash, size, mtime) and load the **base** — `.obsink/manifest.json`, the checkpoint of the last completed sync. A base entry with no file on disk becomes a local tombstone that keeps the base hash (the parent hash for the remote delete).
2. **Fetch the remote manifest** (`fetch_remote_manifest`) and re-key it by real path. The last manifest and its `ETag` are cached in `.obsink/remote-manifest.json`; the fetch sends `If-None-Match` and reuses the cache on `304`. A corrupt or missing cache falls back to an unconditional fetch.
3. **Diff** base vs local vs remote (`diff_manifests`). A side "changed" when its version — the pair `(hash, deleted)`, with absent and deleted counting as the same version — differs from the base. `modified` is never consulted: the server stamps its own receipt time on every write, so mtimes are not comparable across devices.
   - only local changed → **upload** (or a remote delete when the local side is gone)
   - only remote changed → **download** (or a local delete when the remote side is gone)
   - both changed to the same version → nothing (converged)
   - both changed to different versions → **conflict**
   With no base (first sync) a path that exists on both sides with different content is a conflict, never a silent pick.
4. **Apply downloads** immediately — local deletes first, one by one, then the downloads eight at a time (`buffer_unordered`), so a case-only rename (`Note.md` → `note.md`) survives a case-insensitive volume — and return uploads + conflicts as a `SyncPlan`. A fatal download error stops further downloads from starting.
5. The UI/CLI resolves conflicts (keep local / keep remote / keep both).
6. `complete_sync` applies resolutions, uploads pending changes through `POST /vaults/:id/batch` — up to 64 operations or 32 MiB of ciphertext per request, each one read, encrypted and sent before the next batch is prepared; every operation is conflict-gated by the server via its `parentHash`, and the answer carries one status per operation (200, 409 with the current entry, or a per-file 4xx) — and **checkpoints**: the server manifest is re-fetched and saved as the next base, except that every **held-back** path — a failed download or upload, a failed local delete, or a late 409 — keeps its previous base entry (or none), so the next diff retries it or still sees "both changed". Transfers are best-effort: a per-file failure (e.g. a 413) is recorded and skipped while the rest continues; a fatal failure (network down, auth, a 5xx) stops the remaining batches and skips the checkpoint. Filesystem work (the walk, reads, decrypt + atomic writes, the checkpoint) runs on tokio's blocking pool so it never parks the reactor. Late 409s come back on `SyncResult.conflicts`; `SyncPlan::from_late_conflicts` turns them into a conflict-only plan for another resolution round. Per-file failures come back as `SyncFailure { path, kind, error, fatal }`.

The engine **never auto-resolves** a conflict — that's a UI decision. The one collapse it does apply: **keep both** needs two live versions, so with a deletion on one side it becomes keep local (remote deleted) or keep remote (local deleted).

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

`ApiClient` applies a 15s connect timeout and a 60s read-inactivity timeout to every request, plus a 30s whole-request budget on the small metadata calls (manifest, vault list, delete); blob transfers get no whole-request budget, so a slow but moving 50 MB upload completes. Transient failures (timeouts, connection errors) retry up to 3 times with exponential backoff. HTTP status errors and non-transient errors surface immediately as typed `ApiError`s. Logging is via `tracing` (`debug` per request, `info` per sync plan, `warn` on retry).

### Partial-sync recovery

Above the per-request retry, the sync engine is partial-sync aware. Each non-conflict transfer error is classified **fatal** (`ApiError::Http`, or `UnexpectedStatus` 401/403/5xx → stop the batch) or **per-file** (413/404/other 4xx, local crypto/IO → record and continue). Recorded failures land on `SyncResult.failures` as `SyncFailure { path, kind, error, fatal }`. Because the manifest checkpoints on partial success (sync-cycle step 6) and holds back the paths that failed, a dropped sync resumes on the next run: files that landed are converged (local version == remote version ⇒ nothing to do) and the failed ones are diffed against their old base again.

### Atomic writes

Downloaded files, `.obsink/manifest.json`, the remote-manifest cache, and the client config files go through `write_atomic` (temp file in the same directory, `fsync`, rename), so a crash mid-write leaves the previous version in place rather than a truncated file. The hasher skips the `.obsink-tmp` temp files a crash could leave behind.

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
