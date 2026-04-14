# ObSink — Project Specification

> *Because things will go wrong.*

ObSink is a free, self-hosted, end-to-end encrypted sync engine for Obsidian vaults across macOS, iOS, Windows, Linux, and Android. It replaces paid sync services with a manual "Sync" button backed by Cloudflare infrastructure and a shared Rust core.

---

## 1. High-Level Architecture

```
┌──────────────────┐        ┌──────────────────────────┐        ┌──────────────────┐
│  Desktop clients │        │     Cloudflare            │        │   Mobile clients │
│  (Tauri + Rust)  │        │                           │        │                  │
│                  │ HTTPS  │  Worker  (API logic, TS)  │ HTTPS  │  iOS (Swift)     │
│  macOS           │◄──────►│  R2      (file storage)   │◄──────►│  Android (Tauri) │
│  Windows         │        │  KV      (manifest/meta)  │        │                  │
│  Linux           │        │  Cron    (version pruning)│        │                  │
└──────────────────┘        └──────────────────────────┘        └──────────────────┘
```

### Components

| Component | Language | Purpose |
|---|---|---|
| `core/` | Rust | Shared sync engine: encryption, hashing, manifest diffing, conflict detection, API client |
| `worker/` | TypeScript | Cloudflare Worker API: file storage, manifest management, conflict gating, version retention |
| `desktop/` | Rust + Web (Tauri) | macOS, Windows, Linux desktop apps. Thin UI shell calling into Rust core |
| `ios/` | Swift + Rust (via UniFFI) | iOS app + File Provider extension. SwiftUI interface, Rust core via generated bindings |
| `android/` | Tauri (or React Native + Rust) | Android app. Tauri mobile if viable, React Native as fallback |

---

## 2. Tech Stack

- **Rust** — core sync library (encryption, hashing, manifest diffing, conflict detection, Cloudflare API client)
- **TypeScript** — Cloudflare Worker backend
- **Tauri v2 + web frontend** — desktop apps (macOS, Windows, Linux) and Android app; UI in HTML/CSS/JS (framework TBD: Svelte, React, or Vue)
- **Swift + SwiftUI** — iOS main app + File Provider extension, calling Rust core via UniFFI bindings
- **Cloudflare** — Worker (compute), R2 (object storage), KV (manifest metadata), Cron Triggers (pruning)

---

## 3. Sync Model

### 3.1 Manual Sync (Button-Driven)

There is no automatic file watching or background sync. Users press a "Sync" button to trigger the full sync cycle. This eliminates debouncing, race conditions, partial write handling, and iOS background scheduling issues.

### 3.2 Sync Flow

When the user taps "Sync":

1. **Pull manifest** — `GET /manifest` from the server. Compare server manifest against local manifest.
2. **Compute diff** — Produce three lists:
   - Files newer on server → need downloading
   - Files newer locally → need uploading
   - Files changed on both sides → conflicts
3. **Download remote changes** — `GET /files/:path` for each server-newer file. Decrypt and save locally.
4. **Resolve conflicts** — If any conflicts exist, pause sync and present the conflict resolution UI. User picks a winner per file (see §5).
5. **Upload local changes** — `POST /batch` (or individual `PUT /files/:path`) for all locally-changed files plus resolved conflicts.
6. **Handle late 409s** — If any uploads return `409 Conflict` (edge case: another device synced between steps 1 and 5), re-pull and resolve those too.
7. **Update local manifest** — Set local manifest to match server state. Sync complete.

### 3.3 Change Detection (Content Hashing)

Each file is identified by a SHA-256 hash of its **plaintext** content (before encryption). Clients compare local hashes against the server manifest to determine what changed.

Hashing plaintext (not ciphertext) is required because AES-GCM produces different ciphertext each time due to random nonces. Hashing ciphertext would make every file appear "changed" on every sync.

**Minor information leak:** An attacker with server access could confirm whether a specific known document exists in the vault by comparing hashes. Mitigation (optional, not v1): HMAC the hash with the encryption key so hashes are only meaningful to key holders.

### 3.4 Stale Vault Warning

On app open, perform a lightweight `GET /manifest` check. If the server has changes the client hasn't pulled, display a banner:

> "3 files changed on another device. Sync before editing?"

This prevents most accidental conflicts.

---

## 4. Cloudflare Worker API

### 4.1 Authentication

Shared API key in `Authorization: Bearer <token>` header. Stored as a Cloudflare Worker secret. Both clients send it with every request.

### 4.2 Data Model

**R2** stores encrypted file blobs, keyed by `{vault_id}/{file_path}` (e.g., `vault_abc/daily-notes/2026-04-14.md`).

**KV** stores:
- Per-vault manifests: key = `manifest:{vault_id}`, value = JSON manifest object
- Vault index: key = `vaults`, value = JSON array of vault metadata (id, name, created timestamp)

**Manifest structure:**

```json
{
  "daily-notes/2026-04-14.md": {
    "hash": "a1b2c3...",
    "modified": 1713100800,
    "size": 2048
  },
  "projects/sync-app.md": {
    "hash": "d4e5f6...",
    "modified": 1713090000,
    "size": 512
  }
}
```

Single KV entry per vault. KV has a 25MB value limit — sufficient for thousands of notes. Shard later if needed.

### 4.3 Endpoints

**`GET /vaults`**
Returns list of all vault names and IDs. Used during setup to select which vault to connect to.

**`POST /vaults`**
Creates a new vault. Body: `{ "name": "my-vault" }`. Returns the new vault ID.

**`GET /vaults/:vault_id/manifest`**
Returns the full manifest JSON for a vault.

**`GET /vaults/:vault_id/files/:path`**
Downloads a single encrypted file blob from R2.

**`PUT /vaults/:vault_id/files/:path`**
Uploads a single file with conflict detection.

Required headers:
- `X-Parent-Hash` — hash of the version the client based their edit on
- `X-Content-Hash` — hash of the new content

Logic:
1. Read current manifest from KV
2. If file exists in manifest and `X-Parent-Hash` ≠ manifest hash → return `409 Conflict` with current server metadata
3. If file is new or hashes match → write blob to R2, copy previous blob to `_versions/{vault_id}/{path}/{timestamp}` (for version history), update manifest in KV, return `200 OK`

**`DELETE /vaults/:vault_id/files/:path`**
Soft-deletes a file. Requires `X-Parent-Hash`. On hash mismatch → `409 Conflict`.

On success: moves blob to `_trash/{vault_id}/{path}/{timestamp}`, marks as deleted in manifest. Blob retained for 30 days before hard deletion by cron.

**`POST /vaults/:vault_id/batch`**
Batch operations endpoint. Body:

```json
{
  "operations": [
    {
      "action": "put",
      "path": "daily-notes/2026-04-14.md",
      "parentHash": "a1b2c3...",
      "contentHash": "x7y8z9...",
      "content": "<base64 encrypted blob>"
    },
    {
      "action": "delete",
      "path": "trash/old-note.md",
      "parentHash": "m1n2o3..."
    }
  ]
}
```

Returns per-operation results. Some may succeed, some may return `409`.

### 4.4 Attachment Size Limit

Files above **50MB** are rejected by the Worker. This prevents accidental syncing of large media files. Configurable per vault (stored in vault metadata in KV). The batch endpoint should exclude large files — those go through individual `PUT` requests.

### 4.5 Cron Jobs (Cloudflare Cron Triggers)

**Version pruning** — Runs daily. Lists all objects under `_versions/`. Deletes versions older than 14 days or beyond 10 versions per file (whichever is hit first).

**Trash purging** — Runs daily. Lists all objects under `_trash/`. Hard-deletes anything older than 30 days.

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
   - **Keep both** — save remote version as `{filename}.conflict.{ext}` in the vault as an escape hatch
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

- File contents: **encrypted** (stored as opaque blobs in R2)
- File paths in manifest: **plaintext** (server needs paths for routing and manifest lookups)
- Vault names: **plaintext** (server needs to list vaults)
- File hashes in manifest: **plaintext** (derived from plaintext content; see §3.3 for information leak discussion)

### 6.3 Key Storage

On first setup, user enters passphrase. The app derives the key via Argon2id and stores it in the platform keychain:

| Platform | Storage |
|---|---|
| macOS | macOS Keychain |
| iOS | iOS Keychain |
| Android | Android Keystore |
| Windows | DPAPI / Credential Manager |
| Linux | libsecret / kwallet |

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

On each file upload, the Worker copies the current blob to `_versions/{vault_id}/{path}/{timestamp}` before overwriting.

Retention: **14 days, max 10 versions per file** (whichever limit is hit first). Pruned daily by Cron Trigger.

### 8.2 Access

v1: backend safety net only. Recovery requires manual R2 access or a simple CLI tool.

Future: "Browse history" button per file in the app UI, showing a list of past versions with timestamps and the ability to restore.

---

## 9. Deletions

### 9.1 Soft Delete

When a file is deleted locally and synced, the server moves the blob to `_trash/{vault_id}/{path}/{timestamp}` and marks the manifest entry with `"deleted": true`.

Other clients see the deletion flag on next sync and remove the file locally.

### 9.2 Retention

Trashed files are retained for **30 days**. Purged by daily Cron Trigger.

### 9.3 Recovery

v1: manual R2 access or CLI tool.

Future: "Recently deleted" view in the app UI.

---

## 10. Multi-Vault Support

### 10.1 Data Isolation

Each vault has:
- A unique `vault_id`
- Its own R2 key prefix (`{vault_id}/...`)
- Its own manifest entry in KV (`manifest:{vault_id}`)
- Its own encryption passphrase and derived key

### 10.2 Vault Management

The Worker maintains a `vaults` key in KV listing all vault IDs and names. Clients can list, create, and (eventually) delete vaults.

### 10.3 Client UX

On app launch, the client shows a vault picker if multiple vaults are configured. Each vault's passphrase is stored separately in the platform keychain. The user can add/remove vaults from settings.

---

## 11. iOS File Provider

### 11.1 API

Uses `NSFileProviderReplicatedExtension` (the modern replicated API).

### 11.2 Architecture

The main app and File Provider extension share data through an **App Group** container:
- **SQLite database** (via GRDB or similar) — item metadata (identifiers, parent identifiers, filenames, hashes, sync state, pending flags)
- **Local file cache** — decrypted file contents in the shared container

### 11.3 Extension Responsibilities

- `enumerateChanges` — reports items added/modified/deleted since last enumeration, driven by database state
- `fetchContents` — serves decrypted files from local cache
- `createItem` / `modifyItem` — accepts writes from Obsidian, saves to local cache, sets `pendingUpload = true` in database

### 11.4 The Extension Does NOT Touch the Network

All networking lives in the main app's sync engine. The extension is a passive passthrough to local storage. On sync completion, the main app calls `NSFileProviderManager.signalEnumerator(for:)` to notify the extension of new data.

### 11.5 Item Database Schema

```sql
CREATE TABLE items (
    identifier       TEXT PRIMARY KEY,
    parentIdentifier TEXT NOT NULL,
    filename         TEXT NOT NULL,
    contentHash      TEXT,
    localPath        TEXT,
    isDirectory      INTEGER NOT NULL DEFAULT 0,
    size             INTEGER,
    modified         INTEGER,
    pendingUpload    INTEGER NOT NULL DEFAULT 0,
    pendingDeletion  INTEGER NOT NULL DEFAULT 0
);
```

### 11.6 Item Identifiers

Stable UUIDs assigned on first encounter. **Never** use file paths as identifiers (files can be renamed). Store the UUID ↔ path mapping in the database.

---

## 12. Initial Setup Flow

### 12.1 First Device (Creating a Vault)

1. Enter Cloudflare Worker URL
2. Enter API key
3. Choose: "Create new vault" or "Connect to existing vault"
4. If creating: enter vault name, choose passphrase → app derives key, stores in keychain, creates vault on server, optionally imports existing local Obsidian vault folder
5. If connecting: app lists vaults from server, user picks one, enters passphrase → key derived, stored in keychain, initial pull of all files

### 12.2 Adding a New Device

1. Enter Worker URL and API key (manually typed or from QR code in future)
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
│       ├── api_client.rs     HTTP client for Cloudflare Worker
│       ├── sync_engine.rs    orchestrates full sync flow
│       └── types.rs          shared types (SyncResult, Conflict, FileEntry, etc.)
├── worker/                   Cloudflare Worker
│   ├── package.json
│   ├── wrangler.toml
│   └── src/
│       └── index.ts
├── desktop/                  Tauri desktop app
│   ├── src-tauri/
│   │   ├── Cargo.toml        depends on ../core
│   │   └── src/
│   │       └── main.rs       Tauri commands wrapping core
│   └── src/                  web UI (Svelte/React/Vue TBD)
│       ├── App.[ext]
│       ├── SyncButton.[ext]
│       ├── ConflictResolver.[ext]
│       └── Settings.[ext]
├── ios/                      Xcode project
│   ├── ObSink/               main app (SwiftUI)
│   │   ├── ObSinkApp.swift
│   │   ├── SyncView.swift
│   │   ├── ConflictView.swift
│   │   └── SettingsView.swift
│   ├── FileProvider/          extension target
│   │   ├── FileProviderExtension.swift
│   │   └── FileProviderItem.swift
│   └── RustCore/              UniFFI generated Swift bindings
├── android/                   Tauri mobile (or React Native fallback)
│   └── ...
├── cli/                       Rust CLI tool for testing/debugging
│   ├── Cargo.toml
│   └── src/
│       └── main.rs
└── README.md
```

---

## 14. Non-Functional Requirements

- **Privacy:** Server never sees plaintext. All encryption/decryption happens on-device.
- **Cost:** Cloudflare free tier should cover personal use indefinitely (100k Worker requests/day, 10GB R2, 100k KV reads/day).
- **Reliability:** Manual sync means no data races. Conflict detection means no silent data loss.
- **Portability:** Rust core compiles to every target platform. No platform lock-in beyond the iOS File Provider.
- **Simplicity:** Minimal moving parts. No daemon processes. No background sync (v1). One button does everything.
