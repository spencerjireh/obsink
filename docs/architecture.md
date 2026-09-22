# Architecture

This document is for contributors. It describes how ObSink syncs a folder and what the wire format looks like. For the product spec, see [spec.md](../spec.md).

## Components

```
┌─────────────────────┐      HTTPS      ┌──────────────────────────────┐
│ Clients             │  Bearer token   │ obsink-server (Rust, axum)    │
│  - obsink CLI       │ ───────────────►│  - accounts, invites, routing │
│  - Tauri desktop    │  (via a TLS     │  - conflict gating (one tx)   │
│  - iOS app          │   proxy)        │  - envelope encryption        │
│  - browser (web/)   │ ◄───────────────│  - version/trash retention    │
│  (all wrap core/)   │                 │                               │
└─────────────────────┘                 └───────────┬──────────────────┘
          │                                          │
   builds/diffs                              ┌───────┴────────────────┐
   manifests, encrypts                       │ Postgres  (manifests,  │
   files locally                             │            accounts)   │
                                             │ volume    (blobs)      │
                                             └────────────────────────┘
```

The **Rust core** (`core/`) holds all the logic worth sharing across platforms. The server is deliberately thin: it treats paths and hashes as opaque strings and never decrypts vault content.

The **web container** (`web/Dockerfile`, Caddy with `web/Caddyfile`) is the public site: the landing page and `install.sh` from `site/`, the browser client at `/app`, and a reverse proxy that forwards `/auth/*`, `/vaults/*`, `/healthz` and non-browser `GET /` to the server. It lets the browser client stay same-origin (no CORS on the server) and keeps the old API host answering for builds that bake it; the API itself has its own domain.

The core is split along one seam: the modules that need no filesystem and no network (`crypto`, `manifest`, `ignore`, `sync_rules`, `pacing`, `server_url`, the data half of `hash_cache`) build for every target, wasm32 included; `sync_engine`, `hasher`, `api_client`, `auth`, `daemon` and `watcher` are native-only. `core-wasm/` wraps the portable half with wasm-bindgen (an opaque `VaultKeys` handle so key material stays in wasm memory; manifests, diffs and conflicts as JSON) for the browser client, whose sync driver is written in TypeScript against the same rules.

## The sync cycle

`prepare_sync` → (resolve conflicts) → `complete_sync`:

1. **Load local state** (`load_local_state`): walk the folder into a working manifest (keyed content hash, size, mtime) and load the **base** — `.obsink/manifest.json`, the checkpoint of the last completed sync. The walk consults `.obsink/hash-cache.json`, a memo of `(mtime, size) → hash` fingerprinted by the vault's `content_mac` key: a file whose stat pair is unchanged keeps its hash without being read, so a repeat walk of an unchanged vault is a stat per file (about 14 ms for 3 000 files against about 3 s cold in a debug build). A re-keyed vault, a corrupt cache or a missing one starts empty. A base entry with no file on disk becomes a local tombstone that keeps the base hash (the parent hash for the remote delete).
2. **Fetch the remote manifest** (`fetch_remote_manifest`) and re-key it by real path. The last manifest and its `ETag` are cached in `.obsink/remote-manifest.json`; the fetch sends `If-None-Match` and reuses the cache on `304`. A corrupt or missing cache falls back to an unconditional fetch.
3. **Diff** base vs local vs remote (`diff_manifests`). A side "changed" when its version — the pair `(hash, deleted)`, with absent and deleted counting as the same version — differs from the base. `modified` is never consulted: the server stamps its own receipt time on every write, so mtimes are not comparable across devices.
   - only local changed → **upload** (or a remote delete when the local side is gone)
   - only remote changed → **download** (or a local delete when the remote side is gone)
   - both changed to the same version → nothing (converged)
   - both changed to different versions → **conflict**
   With no base (first sync) a path that exists on both sides with different content is a conflict, never a silent pick.
4. **Apply downloads** immediately — local deletes first, one by one, then the downloads eight at a time (`buffer_unordered`), so a case-only rename (`Note.md` → `note.md`) survives a case-insensitive volume — and return uploads + conflicts as a `SyncPlan`. A fatal download error stops further downloads from starting.
5. The UI/CLI resolves conflicts (keep local / keep remote / keep both).
6. `complete_sync` applies resolutions, uploads pending changes through `POST /vaults/:id/batch` — up to 64 operations or 32 MiB of ciphertext per request, each one read, encrypted and sent before the next batch is prepared; every operation is conflict-gated by the server via its `parentHash`, and the answer carries one status per operation (200, 409 with the current entry, or a per-file 4xx) — and **checkpoints**: the server manifest is re-fetched and saved as the next base, except that every **held-back** path — a failed download or upload, a failed local delete, or a late 409 — keeps its previous base entry (or none), so the next diff retries it or still sees "both changed". Transfers are best-effort: a per-file failure (e.g. a 413, or a response that could not be decoded) is recorded and skipped while the rest continues; a fatal failure (the server unreachable, a request that could not be sent or timed out, auth, a 5xx) stops the remaining batches and skips the checkpoint. A checkpoint that fails on its own (the re-fetch or the base write) is reported as `SyncResult.checkpoint_error`, not as a file failure: the files moved, and the next cycle redoes the bookkeeping. Filesystem work (the walk, reads, decrypt + atomic writes, the checkpoint) runs on tokio's blocking pool so it never parks the reactor. Late 409s come back on `SyncResult.conflicts`; `SyncPlan::from_late_conflicts` turns them into a conflict-only plan for another resolution round. Per-file failures come back as `SyncFailure { path, kind, error, fatal }`. `complete_sync` itself is three steps (`apply_resolutions`, `run_uploads`, `checkpoint`) plus the accounting between them.

The engine **never auto-resolves** a conflict — that's a UI decision. The one collapse it does apply: **keep both** needs two live versions, so with a deletion on one side it becomes keep local (remote deleted) or keep remote (local deleted).

## The daemon

`core/src/daemon.rs` is a driver *above* the engine (`AGENTS.md` rule 2): it decides when to call `prepare_sync` / `complete_sync` and never changes what they do. One daemon per vault, one cycle at a time, driven from a single command channel (`SyncNow`, `Resolve`, `Stop`) so nothing else runs a cycle on a daemon-managed vault. `obsink watch` and the desktop app host it.

- **Local edits** come from `core/src/watcher.rs` (`notify`, recursive; FSEvents on macOS) as vault-relative paths, minus the ignore rules. A `Debouncer` releases a path once it has been quiet for 750 ms *and* its `(mtime, size)` stat still matches the one seen at its last event (a file mid-write keeps changing size), or after a 2 s batch window from the first event so a busy path cannot hold the rest back. Events that arrive during a cycle land in the next batch; the paths the cycle itself wrote (downloads, local deletes) are dropped so they do not trigger a no-op cycle.
- **Remote edits** are found by `remote_changed`: a conditional manifest fetch with the cached ETag, every 5 s within a minute of any activity and every 60 s when idle. A 304 costs nothing; a 200 refreshes the cache so the cycle that follows is served from it.
- **Conflicts** are never resolved by the daemon. Every conflict is answered with `ConflictResolutionChoice::Defer`: nothing moves for that path, it is held back from the checkpoint (so the next diff still sees "both changed"), and it comes back on `SyncResult.conflicts` and as a `ConflictsPending` event each cycle. Everything else keeps syncing. A `Resolve` command applies the user's choices to the pending plan; anything unanswered stays deferred.
- **Failures** that are fatal (network down, auth, 5xx) suppress every trigger for `5 s × 2^n`, capped at 5 min, until a cycle succeeds.
- **Ignore rules** (`core/src/ignore.rs`) are shared by the walker, the watcher and the diff: `.obsink/`, `*.obsink-tmp`, `.obsidian/workspace.json`, `.obsidian/workspace-mobile.json`, `.trash/`, `.DS_Store`, `.git/`, plus a vault's own `ignore` patterns. `diff_local_and_remote` filters base, local and remote alike, so a path that was synced before it became ignored is simply invisible rather than deleted.

## Wire format (v3)

`PROTOCOL_VERSION = 3`. The guiding principle: **the server learns nothing about your vault**.

### Key hierarchy

One passphrase per account, one random key per vault, and the server holds only wrapped keys (spec §6.1 has the full table):

- **KEK** = Argon2id(passphrase, 16 random bytes of salt; 64 MiB / 3 iterations / 1 lane). Derived on demand, never stored.
- **Account key**: 32 random bytes, generated when the passphrase is first set. Stored on the server as `AES-256-GCM(KEK, account key)` with the user id as AAD, next to the salt and a `key_id`; kept unwrapped in the client's keychain. A wrong passphrase is a failed GCM tag on the client. A server-side verifier, `HMAC(HKDF(account key, "obsink:v3:verify"), user id)`, gates a rewrap (passphrase change) and is never returned.
- **Vault key**: 32 random bytes per vault. Stored per member as `AES-256-GCM(HKDF(account key, "obsink:v3:vault-wrap"), vault key)` with the vault id as AAD (`vault_members.wrapped_key`); kept unwrapped in the keychain under the vault id. Because it does not derive from the account key, a later "share vault" only has to wrap it for another member.
- **Sub-keys**: the vault key is HKDF-SHA256 input keying material for four purpose-separated sub-keys (`derive_keys`), unchanged from v2:

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
- **Why wrap keys instead of deriving them?** Argon2id runs once per unlock, not once per vault; `Download` on a new device is one action; a passphrase change is a rewrap, not a re-encryption; and a vault key can later be handed to another member. The cost: the wrapped account key travels to any session holder, so the passphrase (12 characters minimum) and the Argon2id parameters are the defence.

### Conflict gating

On `PUT`/`DELETE` the client sends `X-Parent-Hash` (the hash it believes is current). The server compares it to the stored manifest hash inside the write transaction; on mismatch it returns `409` with the current server entry. This makes retried/racing writes safe — they either succeed or surface a conflict, never silently clobber.

## Network resilience

`ApiClient` applies a 15s connect timeout and a 60s read-inactivity timeout to every request, plus a 30s whole-request budget on the small metadata calls (manifest, vault list, delete); blob transfers get no whole-request budget, so a slow but moving 50 MB upload completes. Transient failures (timeouts, connection errors) retry up to 3 times with exponential backoff. HTTP status errors and non-transient errors surface immediately as typed `ApiError`s. Logging is via `tracing` (`debug` per request, `info` per sync plan, `warn` on retry).

### Partial-sync recovery

Above the per-request retry, the sync engine is partial-sync aware. Each non-conflict transfer error is classified **fatal** (`ApiError::Http` when the request could not be sent, connect or timed out; `Unauthorized`; `UnexpectedStatus` 401/403/5xx → stop the batch) or **per-file** (413/404/other 4xx, a response body that could not be decoded, local crypto/IO → record and continue). Recorded failures land on `SyncResult.failures` as `SyncFailure { path, kind, error, fatal }`. Because the manifest checkpoints on partial success (sync-cycle step 6) and holds back the paths that failed, a dropped sync resumes on the next run: files that landed are converged (local version == remote version ⇒ nothing to do) and the failed ones are diffed against their old base again.

### Atomic writes

Downloaded files, `.obsink/manifest.json`, the remote-manifest cache, and the client config files go through `write_atomic` (temp file in the same directory, `fsync`, rename), so a crash mid-write leaves the previous version in place rather than a truncated file. The hasher skips the `.obsink-tmp` temp files a crash could leave behind.

### Progress reporting

Sync is observable through a `ProgressSink` trait (`Phase` / `FileStarted` / `FileCompleted` / `FileFailed` / `Done` events) that each facade adapts to its native channel: the CLI prints to stderr, the desktop emits a `sync://progress` Tauri event, and iOS receives a UniFFI `ProgressListener` callback. Callers with no UI pass `NoProgress`.

## Server storage

Postgres tables (`server/migrations/0001_init.sql`):

- `users` — id, sealed email, sealed Apple subject, keyed-HMAC lookup columns, the wrapped account key with its salt, `key_id` and verifier
- `devices` — `(user_id, id)`, sealed name, platform, created, last_seen; one session per device
- `sessions` — id, user, device, `sha256(token)`, created, expires (180 days)
- `email_codes`, `invites` — one-time codes (keyed HMAC, attempts, cooldown) and invite codes
- `vaults` — id, owner, sealed name, `max_file_size`, `revision`, `last_write`
- `vault_members` — `(vault_id, user_id)`, role, the vault key wrapped for that member
- `device_vaults` — which devices hold a vault, with the last synced revision (reported best-effort after each checkpoint)
- `files` — one row per manifest entry: `(vault_id, path token, hash, modified, size, deleted, enc_path)`

Blob volume (`OBSINK_DATA_DIR/blobs`), file names are `sha256(path token)`:

- Live: `live/<vaultId>/<ab>/<hash>`
- Version (on overwrite): `_versions/<vaultId>/<hash>/<unixSeconds>[-n]`
- Trash (on delete): `_trash/<vaultId>/<hash>/<unixSeconds>[-n]`

Every write (`PUT`, `DELETE`, each batch operation) is one transaction: check membership, lock the vault row `FOR UPDATE`, read the current entry, check size and quota, compare `X-Parent-Hash`, move the old blob aside, write the new one, upsert the row, bump `revision`. Two devices racing on one path get exactly one `200` and one `409`. `revision` is the manifest `ETag`. Version and trash blobs are readable through `GET …/versions` and `GET …/trash` (spec §4.3); restore is a client operation, since only a client can compute the manifest `hash` of a restored file.

Envelope encryption (`server/src/crypto.rs`): `OBSINK_SERVER_KEY` is HKDF input for three sub-keys — blob wrapping, column sealing (AES-GCM with a `<table>.<column>:<row id>` AAD so ciphertexts cannot be moved between rows), and keyed lookup HMACs. Path tokens, content hashes, `encPath`, and the wrapped account and vault keys are stored as the client sent them: they are already HMACs or ciphertext under keys the server never has.

The retention task (`server/src/retention.rs`) runs at startup and every `RETENTION_INTERVAL_SECS`: prune `_versions/` (keep newest 10 per file / 14 days) and `_trash/` (30 days), delete expired sessions and day-old codes, and remove blob directories whose vault row is gone.

## Testing

- `core/` — unit tests for crypto (round-trips, sub-key separation, HMAC, path tokens), hashing, manifest diffing, the ETag cache, multipart batch encoding, and async sync flows against a mock HTTP server (`httpmock`).
- `core-wasm/` — the bindings run natively under `cargo test` (JSON shapes, key handle round-trips against the native derivation) and inside a wasm runtime with `wasm-pack test --node --release` (the `web` CI job).
- `server/` — unit tests for the envelope, key loading, and blob paths; integration tests (`server/tests/`) spin up the real router on a throwaway Postgres database per test and drive it with `reqwest` and the core `ApiClient`/`AuthClient`. They need `DATABASE_URL` and skip without it; CI sets `OBSINK_TEST_REQUIRE_DB=1`.
- `scripts/verify-server-deploy.sh` (curl contract check) and `scripts/verify-cli-deployed-sync.sh` (two CLI devices) — live checks against a running server; `scripts/verify-ios-sim-e2e.sh` drives the iOS simulator against the same server.
