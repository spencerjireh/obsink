# ObSink — Project Specification

> *Because things will go wrong.*

ObSink is a free, self-hosted, end-to-end encrypted sync engine for Obsidian vaults on macOS and iOS. It replaces paid sync services with a manual "Sync" button, a shared Rust core, and a small Rust server you run yourself with `docker compose up`.

---

## 1. High-Level Architecture

```
┌──────────────────┐        ┌──────────────────────────────┐        ┌──────────────────┐
│  Desktop client  │        │   Self-hosted server (Rust)  │        │   Mobile client  │
│  (Tauri + Rust)  │        │   docker compose             │        │                  │
│                  │ HTTPS  │  axum      (API, accounts)   │ HTTPS  │  iOS (Swift)     │
│  macOS           │◄──────►│  Postgres  (manifest/meta)   │◄──────►│  + File Provider │
│  CLI             │ (proxy)│  volume    (encrypted blobs) │ (proxy)│                  │
│                  │        │  retention task              │        │                  │
└──────────────────┘        └──────────────────────────────┘        └──────────────────┘
```

TLS is terminated by the operator's reverse proxy (Coolify's Traefik in the reference deployment); the server itself speaks plain HTTP.

### Components

| Component | Language | Purpose |
|---|---|---|
| `core/` | Rust | Shared sync engine: encryption, hashing, manifest diffing, conflict detection, API client |
| `server/` | Rust (axum) | Self-hosted API: accounts and invites, vault storage, manifest, conflict gating, version retention |
| `cli/` | Rust | `obsink` reference client |
| `desktop/` | Rust + Web (Tauri) | macOS menu-bar app. Thin UI shell calling into Rust core |
| `mobile/` + `ios/` | Swift + Rust (via UniFFI) | iOS app + File Provider extension. SwiftUI interface, Rust core via generated bindings |

---

## 2. Tech Stack

- **Rust** — core sync library (encryption, hashing, manifest diffing, conflict detection, API client) and the server (axum, sqlx, lettre)
- **Postgres + a filesystem volume** — metadata and encrypted blobs, wrapped with a server-side envelope key
- **Docker Compose** — one `docker compose up` for the server, Postgres, and (locally) a mail catcher; production on Coolify behind Traefik
- **Tauri v2 + React** — macOS desktop app; UI in HTML/CSS/TS
- **Swift + SwiftUI** — iOS main app + File Provider extension, calling Rust core via UniFFI bindings

---

## 3. Sync Model

### 3.1 Manual Sync (Button-Driven)

There is no automatic file watching or background sync. Users press a "Sync" button to trigger the full sync cycle. This eliminates debouncing, race conditions, partial write handling, and iOS background scheduling issues.

### 3.2 Sync Flow

When the user taps "Sync":

1. **Pull manifest** — `GET /manifest` from the server. Compare it against the working manifest (the vault on disk) and the **base** (`.obsink/manifest.json`, the checkpoint of the last completed sync; a base entry with no file on disk is a local deletion).
2. **Compute diff** — Per path, a side has changed when its version (§3.3) differs from the base:
   - Changed on the server only → download (or delete locally when the server entry is a tombstone)
   - Changed locally only → upload (or delete on the server when the file is gone locally)
   - Changed on both sides to the same content → nothing (converged)
   - Changed on both sides to different content → conflict
   With no base (first sync on a device) a path present on both sides with different content is a conflict.
3. **Download remote changes** — apply local deletions first, then `GET /files/:path` for each server-changed file. Decrypt and save locally.
4. **Resolve conflicts** — If any conflicts exist, pause sync and present the conflict resolution UI. User picks a winner per file (see §5).
5. **Upload local changes** — `POST /batch` (or individual `PUT /files/:path`) for all locally-changed files plus resolved conflicts.
6. **Handle late 409s** — If any uploads return `409 Conflict` (edge case: another device synced between steps 1 and 5), return them as a conflict-only plan and resolve those too.
7. **Update local manifest** — Re-fetch the server manifest and save it as the new base, except that paths which failed to transfer or are still in conflict keep their previous base entry, so the next sync retries them. Sync complete.

### 3.3 Change Detection (Content Hashing)

Each file is identified by a keyed hash — `HMAC-SHA256(content_mac_key, plaintext)` — of its **plaintext** content (before encryption). Clients compare hashes against the base and the server manifest to determine what changed.

A path's **version** is the pair `(hash, deleted)`; a tombstone and an absent entry are the same version (a tombstone's hash exists only so the next write can present it as `X-Parent-Hash`). The `modified` timestamp is informational: the server stamps its own receipt time on every write, so timestamps from different devices are not comparable and never decide the diff.

Hashing plaintext (not ciphertext) is required because AES-GCM produces different ciphertext each time due to random nonces. Hashing ciphertext would make every file appear "changed" on every sync.

Keying the hash means an attacker with server access cannot confirm whether a specific known document exists in the vault; a plain SHA-256 would (that was wire-format v1's leak).

### 3.4 Stale Vault Warning

On app open, perform a lightweight `GET /manifest` check. If the server has changes the client hasn't pulled, display a banner:

> "3 files changed on another device. Sync before editing?"

This prevents most accidental conflicts.

---

## 4. Server API

### 4.1 Authentication and accounts

Every vault request carries `Authorization: Bearer <token>`. The server resolves the bearer to a **principal**, and every vault route is scoped to that principal's tenant:

| Principal | Bearer | Tenant | Who uses it |
|---|---|---|---|
| operator | the `OBSINK_API_KEY` environment value (compared in constant time) | `default` | the admin CLI, `scripts/verify-*`, harnesses |
| user | an `os_…` session token minted by `/auth/*`, stored as SHA-256, 180-day absolute expiry | the user id | every app user |

Clients offer one setup flow: enter the server URL, then sign in with an emailed 6-digit one-time code (all platforms) or Sign in with Apple (iOS). The operator bearer has no UI; it exists for scripts. Apple sign-in needs no per-server Apple configuration because the identity token's audience is the ObSink app's bundle id (`APPLE_CLIENT_IDS`, default `com.obsink.ios`).

**Invite-only signup.** The first account on a fresh server signs up without an invite. After that, creating a new account requires an unused, unexpired invite code; existing accounts sign in freely. A code stays spent after the account that redeemed it is deleted. Any signed-in user (and the operator) can mint codes: 8 characters from `ABCDEFGHJKLMNPQRSTUVWXYZ23456789`, single-use, valid 7 days. Redemption failures are rate-limited process-wide (20 per minute).

Per-account quotas: `MAX_VAULTS_PER_USER` (default 10) and `MAX_VAULT_BYTES` per vault (default 1 GiB). The operator has no quotas.

Accounts decide only *which encrypted vaults* a bearer may list and write. They never touch vault content: rule §6 (server never sees plaintext) is unaffected, and the vault ID stays the KDF salt.

Auth endpoints (no bearer unless noted):

- `GET /` → `{ service, auth: { email, apple, api_key }, invite_required }` — which sign-in methods are configured and whether new sign-ups need an invite.
- `POST /auth/email/start { email }` → sends the code over SMTP. One per email per 60 s; code valid 10 min, 5 attempts. A failed send does not consume the cooldown. `AUTH_DEV_RETURN_CODE=1` (dev only) returns the code in the response.
- `POST /auth/email/verify { email, code, device_name?, invite_code? }` → `{ token, session, user }`.
- `POST /auth/apple { identity_token, device_name?, email?, code?, invite_code? }` → same; verifies the RS256 JWT against Apple's JWKS (`iss`, `aud ∈ APPLE_CLIENT_IDS`, `exp`) and links to an existing email account when the token's `email` claim matches. Apple includes that claim only in the first token it issues for an app, so a client may forward the credential's email as `email`; because the hint is unverified, the server honours it only together with `code`, a one-time code from `/auth/email/start` for that address (consumed on success). A hint without a code is `403 { "error": "email verification required: …" }` and the client prompts for the code; a token with no claim and no hint signs in by Apple subject alone (a new account then has no email).
- `GET /auth/me` (bearer) → `{ kind, user, sessions[], usage: { vaults: [{ id, bytes }], total_bytes, max_vault_bytes, max_vaults } }` (limits are `null` for the operator).
- `DELETE /auth/session` (bearer) → sign out this device; `DELETE /auth/sessions/:id` → sign out another device.
- `DELETE /auth/account` (bearer) → delete the account, its sessions, its invites, and every vault it owns (blobs, versions, trash, manifests). Required by App Store guideline 5.1.1(v).
- `POST /auth/invites` (bearer) → `201 { invite: { code, created, expires } }`; `GET /auth/invites` → `{ invites: [{ code, created, expires, status, used_at }] }`.
- `DELETE /vaults/:id` (bearer) → delete one vault and its data.

A new account without a valid invite gets `403 { "error": "an invite code is required to create an account" }` or `403 { "error": "invite code is invalid, used, or expired" }`; clients show the message and focus the invite field.

Clients keep the bearer in the OS keychain (service `obsink`, account `bearer:<canonical server URL>`), never in a config file. A `401` surfaces as "sign in again".

### 4.2 Data Model

**Postgres** holds metadata:

- `users` (id, sealed email, sealed Apple subject, keyed-HMAC lookup columns), `sessions` (SHA-256 of the token, sealed device name, expiry), `email_codes`, `invites`
- `vaults` (id, tenant, sealed name, `max_file_size`, `revision`) — `revision` increments on every manifest change and is the manifest ETag
- `files` — one row per manifest entry: `(vault_id, path token, hash, modified, size, deleted, enc_path)`

**Filesystem volume** (`OBSINK_DATA_DIR`, default `/data`) holds the blobs:

```
blobs/live/<vault_id>/<ab>/<sha256(path token)>             current blob
blobs/_versions/<vault_id>/<sha256(path token)>/<unix>[-n]   previous versions (§8)
blobs/_trash/<vault_id>/<sha256(path token)>/<unix>[-n]      soft-deleted blobs (§9)
```

File names are hashes of the client's path token, so nothing a client sends can escape the store.

**Envelope encryption.** `OBSINK_SERVER_KEY` (32 bytes; generated on first run and written to `<data>/server.key` when unset) is HKDF input for three sub-keys: blobs are wrapped again with AES-GCM (they are already client ciphertext), sensitive columns (emails, Apple subjects, device names, vault names) are sealed with a per-row AAD, and lookups by email or Apple subject go through keyed HMAC indexes. Losing the key makes the metadata unreadable; the vault passphrase is still required to read any content.

**Manifest structure** (as served; keys are path tokens, `encPath` recovers the real path):

```json
{
  "<path token>": {
    "hash": "a1b2c3...",
    "modified": 1713100800,
    "size": 2048,
    "deleted": false,
    "encPath": "<AES-GCM(real path)>"
  }
}
```

### 4.3 Endpoints

**`GET /vaults`**
Returns the vaults the principal owns. Used during setup to select which vault to connect to.

**`POST /vaults`**
Creates a new vault. Body: `{ "name": "my-vault", "max_file_size"?: bytes }`. Returns `201 { "vault": { id, name, created, max_file_size } }`.

**`GET /vaults/:vault_id/manifest`**
Returns the full manifest JSON for a vault with `ETag: "<revision>"` and `Cache-Control: private, no-cache`. A request with `If-None-Match` matching the current revision returns `304` with no body; clients keep the last manifest and its ETag in `<vault>/.obsink/remote-manifest.json`.

**`GET /vaults/:vault_id/files/:path`**
Downloads a single encrypted file blob (`application/octet-stream`, `Cache-Control: no-store`).

**`PUT /vaults/:vault_id/files/:path`**
Uploads a single file with conflict detection.

Headers:
- `X-Parent-Hash` — hash of the version the client based their edit on (absent for a new file)
- `X-Content-Hash` — hash of the new content (required)
- `X-Enc-Path` — encrypted real path (kept from the previous entry when absent)

Logic, all inside one database transaction that locks the vault row:
1. Read the current manifest entry for the path.
2. Body larger than the vault's `max_file_size` → `413 file too large`. For accounts, current usage plus the body over `MAX_VAULT_BYTES` → `507 vault storage limit reached`.
3. If an entry exists and `X-Parent-Hash` ≠ its hash → `409 Conflict` with `{ path, current }`. Exception: if the entry is live and already has `X-Content-Hash`, the upload is a retry whose first attempt landed, and the server answers `200` without writing.
4. Otherwise: move the previous live blob to `_versions/<ts>` (version history), write the new blob, upsert the manifest row, bump `revision`, `200 OK`.

**`DELETE /vaults/:vault_id/files/:path`**
Soft-deletes a file. Requires `X-Parent-Hash`. On hash mismatch → `409 Conflict`.

On success: moves the blob to `_trash/<ts>`, marks the entry deleted (keeping its hash, size, and encPath so a later upload can present the tombstone's hash as parent). Blob retained for 30 days before hard deletion by the retention task.

**`POST /vaults/:vault_id/batch`**
Batch operations as `multipart/form-data`:

- one `operations` part (`application/json`): `{ "operations": [ { "action": "put", "path", "parentHash"?, "contentHash", "encPath"? }, { "action": "delete", "path", "parentHash"? } ] }`
- one `content` part per put with `filename="<operation index>"` carrying the raw encrypted bytes

`action` must be exactly `put` or `delete`; any other value (or none) rejects the whole batch with `400` before anything runs. Operations run in order, each with the same transaction as the single-file routes. The response is `200 { "results": [ { path, status, conflict } ] }`; `409` entries carry the conflicting `current` entry, so a batch can partly succeed. A non-multipart body is `415`; a body over `MAX_BATCH_BYTES` (default 128 MiB) is `413`.

### 4.4 Attachment Size Limit

Files above **50 MB** (`MAX_FILE_BYTES`) are rejected by the server. This prevents accidental syncing of large media files. Configurable per vault at creation (`max_file_size`, capped at the server maximum). The sync engine transfers files one request at a time and skips a too-large file as a per-file failure.

### 4.5 Retention task

An in-process task runs at startup and then every `RETENTION_INTERVAL_SECS` (default daily); `obsink-server retention` runs one pass by hand.

**Version pruning** — Deletes versions older than 14 days or beyond the newest 10 per file (whichever is hit first).

**Trash purging** — Hard-deletes trash older than 30 days.

**Housekeeping** — Removes expired sessions, day-old one-time codes, and blob directories whose vault row no longer exists.

---

## 5. Conflict Resolution

### 5.1 Detection

A conflict occurs when a `PUT` request's `X-Parent-Hash` does not match the server manifest's current hash for that file. The server returns `409 Conflict`.

### 5.2 Client-Side Resolution (Option A with Preview)

Conflicts are resolved in the app UI, **not** by dumping `.conflict` files into the Obsidian vault.

When sync detects conflicts, the sync pauses and shows a conflict resolution screen:

1. List of conflicted files with count: "2 conflicts need your attention"
2. Tap a file to see a detail screen
3. Detail screen has a toggle or segmented control: "This device" / "Other device"
4. Each side shows the full note content (read-only preview) and last-modified timestamp
5. Three actions per file:
   - **Keep local** — upload local version, overwrite server
   - **Keep remote** — download server version, overwrite local
   - **Keep both** — save remote version as `{filename}.conflict.{ext}` in the vault as an escape hatch. Only offered when both sides are live: with a deletion on one side it collapses to keeping the side that still exists.
6. After all conflicts are resolved, sync completes

### 5.3 Future Enhancement (v2)

Inline diff with merge — show a unified view with conflicting sections highlighted in two colors. User taps each section to pick a winner. Requires a longest-common-subsequence diff algorithm. Not in v1.

---

## 6. Encryption

### 6.1 Scheme

- **Key derivation:** Argon2id from user passphrase → 256-bit master key
- **File encryption:** AES-256-GCM with a random 96-bit nonce per file
- **Encrypted blob format:** `[12-byte nonce][ciphertext][16-byte GCM auth tag]`
- **One key per vault.** Different vaults can have different passphrases.

### 6.2 What Is Encrypted

- File contents: **encrypted** (stored as opaque blobs on the server volume, wrapped once more with the server key)
- File paths in manifest: **plaintext** (server needs paths for routing and manifest lookups)
- Vault names: **plaintext** (server needs to list vaults)
- File hashes in manifest: **plaintext** (derived from plaintext content; see §3.3 for information leak discussion)

### 6.3 Key Storage

On first setup, user enters passphrase. The app derives the key via Argon2id and stores it in the platform keychain:

| Platform | Storage |
|---|---|
| macOS | macOS Keychain |
| iOS | iOS Keychain |

Key is loaded from keychain on app launch. User only re-enters passphrase when setting up a new device or connecting to a new vault.

### 6.4 No Key Recovery

There is no key recovery mechanism. Lost passphrase = lost data. This is a deliberate design choice for simplicity in v1.

---

## 7. .obsidian Config Syncing

The `.obsidian/` configuration directory is synced alongside vault content. This includes themes, snippets, hotkeys, and plugin settings.

**Known risk:** Some desktop plugins don't work on mobile and vice versa. Syncing config may cause warnings or errors on some platforms. This is acceptable — Obsidian handles missing plugins gracefully (disables them), and the user can manage platform-specific config manually.

**Optional future enhancement:** per-platform `.obsidian` overrides or a `.obsidian-ignore` file to exclude specific config files from sync.

---

## 8. File Versioning

### 8.1 Retention Policy

On each file upload, the server moves the current blob to `_versions/{vault_id}/{hashed path}/{timestamp}` before writing the new one.

Retention: **14 days, max 10 versions per file** (whichever limit is hit first). Pruned by the daily retention task (§4.5).

### 8.2 Access

v1: backend safety net only. Recovery requires operator access to the data volume (`obsink-server` decrypts nothing; the vault passphrase is still needed) or a simple CLI tool.

Future: "Browse history" button per file in the app UI, showing a list of past versions with timestamps and the ability to restore.

---

## 9. Deletions

### 9.1 Soft Delete

When a file is deleted locally and synced, the server moves the blob to `_trash/{vault_id}/{path}/{timestamp}` and marks the manifest entry with `"deleted": true`.

Other clients see the deletion flag on next sync and remove the file locally.

### 9.2 Retention

Trashed files are retained for **30 days**. Purged by the daily retention task (§4.5).

### 9.3 Recovery

v1: operator access to the data volume or a CLI tool.

Future: "Recently deleted" view in the app UI.

---

## 10. Multi-Vault Support

### 10.1 Data Isolation

Each vault has:
- A unique `vault_id`
- Its own blob directories (`live/{vault_id}/`, `_versions/{vault_id}/`, `_trash/{vault_id}/`)
- Its own manifest rows (`files` where `vault_id = ...`) and revision counter
- Its own encryption passphrase and derived key

### 10.2 Vault Management

The server keeps a `vaults` table scoped by tenant. Clients can list, create, and delete the vaults their principal owns.

### 10.3 Client UX

On app launch, the client shows a vault picker if multiple vaults are configured. Each vault's passphrase is stored separately in the platform keychain, and on iOS each vault has its own cache directory, item database, and File Provider location (§11.2). The user can add/remove vaults from settings.

---

## 11. iOS File Provider

### 11.1 API

Uses `NSFileProviderReplicatedExtension` (the modern replicated API).

### 11.2 Architecture

The main app and File Provider extension share data through an **App Group** container, laid out per vault:
- **SQLite database** per vault (`items-<vault id>.sqlite`, via GRDB) — item metadata (identifiers, parent identifiers, filenames, hashes, sync state, pending flags)
- **Local file cache** per vault (`Vault/<vault id>/`) — decrypted file contents in the shared container
- **One File Provider domain per vault** — identifier = vault id, display name = vault name; the extension derives its directory and database from the domain it is instantiated for

### 11.3 Extension Responsibilities

- `enumerateItems` — a folder's live children (or, for the working set, every live item; the trash is always empty), in pages of 500 rows so a large vault's metadata never sits in one array inside the extension's memory budget
- `enumerateChanges` — reports items added/modified/deleted since last enumeration, driven by database state (`rowVersion` anchor, tombstones as deletes), also in pages with `moreComing` and the last delivered row as the next anchor
- `fetchContents` — serves decrypted files from local cache as a staged copy (the system consumes the file it is handed, so the vault copy is never returned directly)
- `createItem` / `modifyItem` — accepts writes from Obsidian, saves to local cache, sets `pendingUpload = true` in database; a folder rename or move rewrites the paths of everything under it
- `deleteItem` — removes the cache file or folder and tombstones the row and, for a folder, every descendant, each with its own `rowVersion`
- Item versions: the content version is the bytes' (size, mtime) and the metadata version is `rowVersion`, so a rename or a pending flag never makes the system re-fetch unchanged contents
- If the database cannot be opened (a data-protection-locked container before first unlock, a stale `-wal`), every request answers `NSFileProviderError.cannotSynchronize` instead of crashing the extension
- `NSExtensionFileProviderSupportsPickingFolders` is set so Obsidian's folder picker can select the vault

### 11.4 The Extension Does NOT Touch the Network

All networking lives in the main app's sync engine. The extension is a passive passthrough to local storage. After a sync (completed, or paused on conflicts after its downloads were applied), the main app reconciles the database and calls `NSFileProviderManager.signalEnumerator(for:)` on the vault's working set and root container to notify the extension of new data.

### 11.5 Item Database Schema

```sql
CREATE TABLE items (
    identifier       TEXT PRIMARY KEY,
    parentIdentifier TEXT NOT NULL,
    filename         TEXT NOT NULL,
    contentHash      TEXT,            -- reserved; the extension has no keys, so it is not populated yet
    localPath        TEXT,
    isDirectory      INTEGER NOT NULL DEFAULT 0,
    size             INTEGER,
    modified         INTEGER,
    pendingUpload    INTEGER NOT NULL DEFAULT 0,
    pendingDeletion  INTEGER NOT NULL DEFAULT 0,
    isDeleted        INTEGER NOT NULL DEFAULT 0,  -- tombstone reported by enumerateChanges
    rowVersion       INTEGER NOT NULL DEFAULT 0   -- monotonic change anchor
);
```

### 11.6 Item Identifiers

Stable UUIDs assigned on first encounter. **Never** use file paths as identifiers (files can be renamed). Store the UUID ↔ path mapping in the database.

---

## 12. Initial Setup Flow

### 12.1 First Device (Creating a Vault)

1. Enter the server URL, then sign in with an email code or Sign in with Apple (iOS). New accounts need an invite code unless the server has no users yet. The session token goes to the keychain, so this happens once per device.
2. Choose: "Create new vault" or "Connect to existing vault"
3. If creating: enter vault name, choose passphrase → app derives key, stores in keychain, creates vault on server, optionally imports existing local Obsidian vault folder
4. If connecting: app lists vaults from server, user picks one, enters passphrase → key derived, stored in keychain, initial pull of all files

### 12.2 Adding a New Device

1. Enter the same server URL and sign in to the same account
2. App lists available vaults
3. User selects vault(s) and enters passphrase for each
4. Initial sync pulls all files

### 12.3 Validation

On connect, the app downloads a single file and attempts decryption. If it fails, the passphrase is wrong. Fail fast with a clear error.

---

## 13. Repo Structure

```
obsink/
├── core/                     Rust shared library
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs            public API surface
│       ├── crypto.rs         AES-256-GCM, Argon2id key derivation
│       ├── manifest.rs       manifest diffing, conflict detection
│       ├── hasher.rs         SHA-256 file hashing
│       ├── api_client.rs     HTTP client for the server (ETag cache, multipart batch)
│       ├── auth.rs           sign-in, sessions, invites
│       ├── sync_engine.rs    orchestrates full sync flow
│       └── types.rs          shared types (SyncResult, Conflict, FileEntry, etc.)
├── server/                   self-hosted server (axum)
│   ├── Cargo.toml            depends on ../core
│   ├── Dockerfile            build context = repo root
│   ├── migrations/           sqlx migrations
│   ├── src/
│   │   ├── main.rs           serve | migrate | invite | retention | keygen | healthcheck
│   │   ├── config.rs         env vars, server key
│   │   ├── crypto.rs         envelope encryption
│   │   ├── blobs.rs          filesystem blob store
│   │   ├── auth/             principal, email, apple, sessions, invites
│   │   ├── routes/           vaults, files, batch, me
│   │   └── retention.rs      pruning task
│   └── tests/                DATABASE_URL-gated integration tests
├── mobile/                   UniFFI facade over core for iOS
├── ios/                      Xcode project (XcodeGen): app, File Provider, tests
├── desktop/                  Tauri desktop app
│   ├── src-tauri/
│   │   ├── Cargo.toml        depends on ../core
│   │   └── src/
│   │       └── main.rs       Tauri commands wrapping core
│   └── src/                  React UI
│       ├── App.tsx
│       ├── main.tsx
│       └── styles.css
├── cli/                       Rust CLI tool for testing/debugging
│   ├── Cargo.toml
│   └── src/
│       └── main.rs
├── scripts/                  build-ios, release-ios, verify-* harnesses
├── docker-compose.yml        local stack: server (built), Postgres, Mailpit
├── docker-compose.coolify.yml production stack: server built from source, Postgres
├── lefthook.yml              git hooks: rustfmt, prettier, commit message
├── deny.toml                 cargo-deny policy
├── rust-toolchain.toml       pinned Rust version
└── .github/
    ├── pull_request_template.md
    ├── rulesets/main.json    branch ruleset for main (applied with gh api)
    └── workflows/
        ├── ci.yml            commit check, fmt/clippy/tests, cargo-deny, server on Postgres, desktop lint+build, iOS simulator tests
        └── release.yml       on v*.*.* tags: verify versions, build + publish the macOS arm64 CLI
```

---

## 14. Non-Functional Requirements

- **Privacy:** Server never sees plaintext. All encryption/decryption happens on-device.
- **Cost:** one small VPS (or any Docker host) runs the server, Postgres, and the blob volume for a household of users; no per-request pricing.
- **Reliability:** Manual sync means no data races. Conflict detection means no silent data loss.
- **Portability:** Rust core compiles to every target platform; the server is one static binary in a distroless image. No platform lock-in beyond the iOS File Provider.
- **Simplicity:** Minimal moving parts. No daemon processes. No background sync (v1). One button does everything.
