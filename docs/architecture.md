# Architecture

This document is for contributors. It describes how ObSink syncs a folder and what the wire format looks like. For the product spec, see [spec.md](../spec.md).

## Components

```
┌─────────────────────┐      HTTPS      ┌──────────────────────────┐
│ Clients             │  Bearer token   │ Cloudflare Worker (TS)    │
│  - obsink CLI       │ ───────────────►│  - auth, routing          │
│  - Tauri desktop    │                 │  - conflict gating        │
│  (both wrap core/)  │ ◄───────────────│  - version/trash pruning  │
└─────────────────────┘                 └───────────┬──────────────┘
          │                                          │
   builds/diffs                              ┌───────┴────────┐
   manifests, encrypts                       │ R2  (blobs)    │
   files locally                             │ KV  (manifests)│
                                             └────────────────┘
```

The **Rust core** (`core/`) holds all the logic worth sharing across platforms. The Worker is deliberately thin: it treats paths and hashes as opaque strings and never decrypts anything.

## The sync cycle

`prepare_sync` → (resolve conflicts) → `complete_sync`:

1. **Build the working manifest** from the local folder (`build_manifest_from_dir`): walk files, record a keyed content hash, size, and mtime. The on-disk `.obsink/manifest.json` from the last sync is used to detect local deletions.
2. **Fetch the remote manifest** (`ApiClient::get_manifest`) and re-key it by real path.
3. **Diff** local vs remote (`diff_manifests`) into three lists:
   - **upload** — local is newer or new
   - **download** — remote is newer or new
   - **conflict** — both changed since the last common state (decided by mtime; equal mtime + different hash ⇒ conflict)
4. **Apply downloads** immediately; return uploads + conflicts as a `SyncPlan`.
5. The UI/CLI resolves conflicts (keep local / keep remote / keep both).
6. `complete_sync` applies resolutions, uploads pending changes (each PUT is conflict-gated by the server via `X-Parent-Hash`), handles any late 409s, and saves the new remote manifest to disk.

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

- **Why a token, not the path?** So the server (and anyone with R2 access) can't see filenames. The token is deterministic, so two devices independently compute the same token for the same path — which is what makes diffing work without coordination.
- **Why `encPath`?** A freshly-connected device pulls a manifest of tokens it can't reverse (the token is a one-way HMAC). `encPath` is reversible AES-GCM, so the client recovers the real filename and re-keys the manifest by real path locally.
- **Why HMAC the content, not SHA-256?** A plaintext SHA-256 would let the server confirm whether you store a known file. A keyed HMAC reveals nothing without the key, while still being a stable equality check for conflict detection.

### Conflict gating

On `PUT`/`DELETE` the client sends `X-Parent-Hash` (the hash it believes is current). The Worker compares it to the stored manifest hash; on mismatch it returns `409` with the current server entry. This makes retried/racing writes safe — they either succeed or surface a conflict, never silently clobber.

## Network resilience

`ApiClient` applies a 30s per-request timeout and retries transient failures (timeouts, connection errors) up to 3 times with exponential backoff. HTTP status errors and non-transient errors surface immediately as typed `ApiError`s. Logging is via `tracing` (`debug` per request, `info` per sync plan, `warn` on retry).

## Worker storage keys

- Blob: `<vaultId>/<token>`
- Version (on overwrite): `_versions/<vaultId>/<token>/<unixSeconds>`
- Trash (on delete): `_trash/<vaultId>/<token>/<unixSeconds>`

Two Cron Triggers prune `_versions/` (keep newest 10 per file / 14 days) and `_trash/` (30 days).

## Testing

- `core/` — unit tests for crypto (round-trips, sub-key separation, HMAC, path tokens), hashing, manifest diffing, and async sync flows against a mock HTTP server (`httpmock`).
- `worker/` — Vitest against in-memory fakes for KV and R2.
- `scripts/verify-*.sh` — live end-to-end checks against a deployed Worker.
