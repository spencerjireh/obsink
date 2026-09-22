# ObSink — Project Specification

> *Because things will go wrong.*

ObSink is a free, self-hosted, end-to-end encrypted sync engine for Obsidian vaults on macOS and iOS. It replaces paid sync services with a "Sync" button that the clients also press for you (on iOS when the app comes to the foreground or in a background refresh; on desktop and the CLI through a daemon), a shared Rust core, and a small Rust server you run yourself with `docker compose up`. Every vault an account owns is listed on every device the account is signed in to; one passphrase unlocks them all, and a vault that is not on a device yet is one `Download` away.

---

## 1. High-Level Architecture

```
┌──────────────────┐        ┌──────────────────────────────┐        ┌──────────────────┐
│  Desktop client  │        │   Self-hosted server (Rust)  │        │   Mobile client  │
│  (Tauri + Rust)  │        │   docker compose             │        │                  │
│                  │ HTTPS  │  axum      (API, accounts,   │ HTTPS  │  iOS (Swift)     │
│  macOS           │◄──────►│             devices, keys)   │◄──────►│  + File Provider │
│  CLI             │ (proxy)│  Postgres  (manifest/meta)   │ (proxy)│                  │
│  Browser (/app)  │        │  volume    (encrypted blobs) │        │                  │
└──────────────────┘        │  retention task              │        └──────────────────┘
                            │  web: site + /app + proxy    │
                            └──────────────────────────────┘
```

TLS is terminated by the operator's reverse proxy (Coolify's Traefik in the reference deployment); the server itself speaks plain HTTP. The `web` container (Caddy) serves the landing page and the browser client and forwards the API paths to the server, so the site domain doubles as an API host for builds that bake it.

### Components

| Component | Language | Purpose |
|---|---|---|
| `core/` | Rust | Shared sync engine: key hierarchy, encryption, hashing, manifest diffing, conflict detection, API client |
| `server/` | Rust (axum) | Self-hosted API: accounts, devices, wrapped keys, invites, vault storage, manifest, conflict gating, version retention |
| `cli/` | Rust | `obsink` reference client |
| `ui/` | TypeScript (React) | The screens shared by the desktop app and the browser client, over the `Backend` interface |
| `desktop/` | Rust + Web (Tauri) | macOS menu-bar app. Thin shell: the shared screens plus Tauri commands into Rust core |
| `core-wasm/` + `web/` + `site/` | Rust (wasm-bindgen) + TypeScript | Browser client at `/app` (the shared screens over a worker that syncs a local folder through the File System Access API, with core's pure rules compiled to wasm), the landing page, and the Caddy container that serves both and proxies the API |
| `mobile/` + `ios/` | Swift + Rust (via UniFFI) | iOS app + File Provider extension. SwiftUI interface, Rust core via generated bindings |

---

## 2. Tech Stack

- **Rust** — core sync library (key hierarchy, encryption, hashing, manifest diffing, conflict detection, API client) and the server (axum, sqlx, lettre)
- **Postgres + a filesystem volume** — metadata and encrypted blobs, wrapped with a server-side envelope key
- **Docker Compose** — one `docker compose up` for the server, Postgres, and (locally) a mail catcher; production on Coolify behind Traefik
- **Tauri v2 + React** — macOS desktop app; UI in HTML/CSS/TS
- **Swift + SwiftUI** — iOS main app + File Provider extension, calling Rust core via UniFFI bindings

---

## 3. Sync Model

### 3.1 Driven Sync

The sync engine has no clock and no file watcher: one call runs the full cycle below, and nothing happens between calls. What triggers a call is a driver outside the engine:

- the user tapping "Sync";
- on iOS, the app coming to the foreground and an OS-scheduled `BGAppRefreshTask` (`AutoSyncPolicy`: a vault with File Provider writes waiting, a server that is ahead, or no sync in the last 15 minutes; never a vault that is syncing, locked, or holding conflicts);
- on desktop and the CLI, a daemon (`core/src/daemon.rs`, `docs/architecture.md`) that debounces filesystem events (750 ms quiet per path, 2 s batch window, a stat gate for files still being written) and polls the server manifest ETag (5 s after activity, 60 s idle), one cycle at a time per vault, backing off on fatal errors. Paths in the shared ignore list (`.obsink/`, atomic-write temp files, `.obsidian/workspace*.json`, `.trash/`, `.DS_Store`, `.git/`, plus per-vault patterns) never sync and never wake it.

Drivers never resolve a conflict. A conflicted path stays pending for the user and everything else keeps syncing.

### 3.2 Sync Flow

When a sync starts (tap, foreground, background refresh, or daemon):

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
7. **Update local manifest** — Re-fetch the server manifest and save it as the new base, except that paths which failed to transfer or are still in conflict keep their previous base entry, so the next sync retries them. Then report the checkpoint: `PUT /vaults/:id/devices/self { revision }` with the manifest revision just saved. The report is best-effort; a failure is logged and never fails the sync or holds a path back. Sync complete.

### 3.3 Change Detection (Content Hashing)

Each file is identified by a keyed hash — `HMAC-SHA256(content_mac_key, plaintext)` — of its **plaintext** content (before encryption). Clients compare hashes against the base and the server manifest to determine what changed.

A path's **version** is the pair `(hash, deleted)`; a tombstone and an absent entry are the same version (a tombstone's hash exists only so the next write can present it as `X-Parent-Hash`). The `modified` timestamp is informational: the server stamps its own receipt time on every write, so timestamps from different devices are not comparable and never decide the diff.

Hashing plaintext (not ciphertext) is required because AES-GCM produces different ciphertext each time due to random nonces. Hashing ciphertext would make every file appear "changed" on every sync.

Keying the hash means an attacker with server access cannot confirm whether a specific known document exists in the vault; a plain SHA-256 would (that was wire-format v1's leak).

### 3.4 Stale Vault Warning

On app open, perform a lightweight `GET /manifest` check. If the server has changes the client hasn't pulled, display a banner:

> "3 files changed on another device. Sync before editing?"

This prevents most accidental conflicts. On iOS the same check feeds the auto-sync (§3.1): a vault the server is ahead of is synced right after the check, so the banner is the state of the moment before that sync starts and stays only for a vault the auto-sync skips (one holding conflicts).

---

## 4. Server API

### 4.1 Authentication, accounts, devices

Every vault request carries `Authorization: Bearer <token>`. The bearer is an `os_…` session token minted by `/auth/*`, stored as SHA-256, with a 180-day absolute expiry. The server resolves it to a **user** and a **device**; every vault route is scoped to the vaults that user is a member of. There is no other principal: scripts and harnesses sign in as ordinary accounts (`AUTH_DEV_RETURN_CODE=1` on a dev server returns the one-time code inline; against production they keep a minted session token).

Clients offer one setup flow: sign in with an emailed 6-digit one-time code (all platforms) or Sign in with Apple (iOS), then unlock with the account passphrase (§6, §12); the server is baked into each build (the CLI also takes `--server-url`), never a field in the UI. Apple sign-in needs no per-server Apple configuration because the identity token's audience is the ObSink app's bundle id (`APPLE_CLIENT_IDS`, default `com.obsink.ios`).

**Devices.** A device is a physical machine, identified by a client-generated UUID that the client keeps for good (macOS Keychain, shared by the desktop app and the CLI; iOS Keychain; IndexedDB in the browser, where a cleared site profile is a new device). Every sign-in carries `device: { id, name, platform }` with `platform` one of `macos`, `ios`, `browser`, `cli`; the field is required, and a sign-in without it is `400 { "error": "update ObSink to continue" }` (this is what keeps protocol-2 clients out, §6.1). A sign-in for a device id the account already knows replaces that device's session, so a device has exactly one session and signing in twice on one Mac does not produce two rows. `name` is free text (80 characters), sealed at rest, and can be changed from any device.

**Invite-only signup.** The first account on a fresh server signs up without an invite. After that, creating a new account requires an unused, unexpired invite code; existing accounts sign in freely. A code stays spent after the account that redeemed it is deleted. Any signed-in user can mint codes, and `obsink-server invite` mints one from the server's shell with no creator (bootstrap and operator use): 8 characters from `ABCDEFGHJKLMNPQRSTUVWXYZ23456789`, single-use, valid 7 days. Redemption failures are rate-limited process-wide (20 per minute). Invites create accounts; they are not vault sharing.

Per-account quotas: `MAX_VAULTS_PER_USER` (default 10) and `MAX_VAULT_BYTES` per vault (default 1 GiB), counted against the vault's owner.

Accounts decide only *which encrypted vaults* a bearer may list and write. They never touch vault content: rule §6 (server never sees plaintext) is unaffected.

Auth endpoints (no bearer unless noted):

- `GET /` → `{ service, protocol: 3, auth: { email, apple }, invite_required }` — the wire format the server speaks, which sign-in methods are configured, and whether new sign-ups need an invite. A client built for another protocol shows `Update ObSink` and stops.
- `POST /auth/email/start { email }` → sends the code over SMTP. One per email per 60 s; code valid 10 min, 5 attempts. A failed send does not consume the cooldown. `AUTH_DEV_RETURN_CODE=1` (dev only) returns the code in the response.
- `POST /auth/email/verify { email, code, device, invite_code? }` → `{ token, session, user }`.
- `POST /auth/apple { identity_token, device, email?, code?, invite_code? }` → same; verifies the RS256 JWT against Apple's JWKS (`iss`, `aud ∈ APPLE_CLIENT_IDS`, `exp`) and links to an existing email account when the token's `email` claim matches. Apple includes that claim only in the first token it issues for an app, so a client may forward the credential's email as `email`; because the hint is unverified, the server honours it only together with `code`, a one-time code from `/auth/email/start` for that address (consumed on success). A hint without a code is `403 { "error": "email verification required: …" }` and the client prompts for the code; a token with no claim and no hint signs in by Apple subject alone (a new account then has no email).
- `GET /auth/keys` (bearer) → `{ account_key: null }` for an account that has not set a passphrase, else `{ account_key: { key_id, wrapped, salt } }` (§6.1). The verifier is never returned.
- `PUT /auth/keys { wrapped, salt, verifier }` (bearer) → `201 { key_id }`. Create-only: when the account already has a key the answer is `409 { account_key: { key_id, wrapped, salt } }` and the client unlocks with that instead (two devices setting up the same new account at once cannot fork it). `wrapped` is base64 of `[12-byte nonce][32-byte ciphertext][16-byte tag]`, `salt` base64 of 16 bytes; anything else is `400`.
- `PUT /auth/keys/rewrap { wrapped, salt, verifier }` (bearer) → `200`. A passphrase change: the same account key wrapped under the new passphrase. `verifier` must equal the stored one (constant-time compare), which only a client holding the unwrapped account key can compute, so a stolen session cannot lock the owner out. `key_id` does not change.
- `GET /auth/me` (bearer) → `{ user, devices: [{ id, name, platform, created, last_seen, current, vault_ids }], usage: { vaults: [{ id, bytes }], total_bytes, max_vault_bytes, max_vaults } }`. `last_seen` is updated at sign-in and by the checkpoint report, not on every request.
- `PATCH /auth/devices/:id { name }` (bearer) → `200`. Rename a device of this account from any device.
- `DELETE /auth/devices/:id` (bearer) → sign out another device: its session, its `device_vaults` rows and the device row go in one transaction. The revoked device gets `401` on its next request and shows `Session expired`; its folders and keys stay where they are (there is no remote wipe). A later sign-in from the same machine registers it again.
- `DELETE /auth/session` (bearer) → sign out this device (the same effect on this device's row).
- `DELETE /auth/account` (bearer) → delete the account, its devices and sessions, its invites, its wrapped keys, and every vault it owns (blobs, versions, trash, manifests). Required by App Store guideline 5.1.1(v).
- `POST /auth/invites` (bearer) → `201 { invite: { code, created, expires } }`; `GET /auth/invites` → `{ invites: [{ code, created, expires, status, used_at }] }`.

A new account without a valid invite gets `403 { "error": "an invite code is required to create an account" }` or `403 { "error": "invite code is invalid, used, or expired" }`; clients show the message and focus the invite field.

Clients keep the bearer in the OS keychain (service `obsink`, account `bearer:<canonical server URL>`) next to the device id (`device:<canonical server URL>`), never in a config file. A `401` surfaces as "sign in again".

### 4.2 Data Model

**Postgres** holds metadata:

- `users` (id, sealed email, sealed Apple subject, keyed-HMAC lookup columns, `account_key_enc`, `account_key_salt`, `account_key_id`, `account_key_verifier`) — the account key columns are null until the first `PUT /auth/keys`; `account_key_enc` is client ciphertext stored as sent, the verifier a 32-byte HMAC
- `devices` (user_id, id, sealed name, platform, created, last_seen; primary key `(user_id, id)`, so two accounts on one machine never collide)
- `sessions` (id, user_id, device_id, SHA-256 of the token, created, expires; unique on `(user_id, device_id)`)
- `email_codes`, `invites` (`created_by` null for codes minted from the server's shell)
- `vaults` (id, `owner` → users, sealed name, created, `max_file_size`, `revision`, `last_write`) — `revision` increments on every manifest change and is the manifest ETag; `last_write` is the time of that change
- `vault_members` (vault_id, user_id, role, `wrapped_key`, created) — one row per account that holds the vault key, with the vault key wrapped under that account's key (§6.1); v1 writes exactly one row, role `owner`. This is the seam a later "share vault" feature fills in.
- `device_vaults` (user_id, device_id, vault_id, attached, last_synced, last_revision) — which devices hold a vault and how far each has synced; rows go with their device or vault
- `files` — one row per manifest entry: `(vault_id, path token, hash, modified, size, deleted, enc_path)`

**Filesystem volume** (`OBSINK_DATA_DIR`, default `/data`) holds the blobs:

```
blobs/live/<vault_id>/<ab>/<sha256(path token)>             current blob
blobs/_versions/<vault_id>/<sha256(path token)>/<unix>[-n]   previous versions (§8)
blobs/_trash/<vault_id>/<sha256(path token)>/<unix>[-n]      soft-deleted blobs (§9)
```

File names are hashes of the client's path token, so nothing a client sends can escape the store.

**Envelope encryption.** `OBSINK_SERVER_KEY` (32 bytes; generated on first run and written to `<data>/server.key` when unset) is HKDF input for three sub-keys: blobs are wrapped again with AES-GCM (they are already client ciphertext), sensitive columns (emails, Apple subjects, device names, vault names) are sealed with a per-row AAD, and lookups by email or Apple subject go through keyed HMAC indexes. Wrapped account and vault keys are already ciphertext under keys the server never has and are stored as sent. Losing the server key makes the metadata unreadable; the account passphrase is still required to read any content.

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
Returns every vault the account is a member of, with what each device needs to show the list and unlock the vault:

```json
{ "vaults": [ {
  "id": "…", "name": "notes", "created": 1713100800, "max_file_size": 52428800,
  "revision": 412, "last_write": 1713200000, "bytes": 431000000,
  "wrapped_key": "<AES-GCM(vault key) under this account's key>",
  "devices": [ { "id": "…", "name": "MacBook", "platform": "macos", "last_synced": 1713199000, "last_revision": 412 } ]
} ] }
```

**`POST /vaults`**
Creates a new vault. Body: `{ "id"?: "vault_<uuid>", "name": "my-vault", "wrapped_key": "<…>", "max_file_size"?: bytes }`. The client generates the vault key and wraps it under its account key with the vault id as AAD, so it mints the id too (`vault_` followed by a UUID; `409` if taken, the server mints one when absent); the vault row and the owner's `vault_members` row are written in one transaction. `400 set a passphrase first` when the account has no key yet. Returns `201 { "vault": { id, name, created, max_file_size } }`.

**`PATCH /vaults/:vault_id`**
`{ "name": "…" }` renames the vault for every device (owner only).

**`DELETE /vaults/:vault_id`**
Deletes the vault and its data for every device (owner only). Other devices see it disappear from `GET /vaults`; their local folders stay.

**`PUT /vaults/:vault_id/devices/self`**
Body `{ "revision"?: n }`. Without a revision: this device now holds the vault (called by Download and Create). With one: the checkpoint report of §3.2 step 7; sets `last_synced`, `last_revision` and the device's `last_seen`. Idempotent.

**`DELETE /vaults/:vault_id/devices/self`**
This device no longer holds the vault (called by `Remove from this device`).

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
1. Check membership, then read the current manifest entry for the path.
2. Body larger than the vault's `max_file_size` → `413 file too large`. Current usage plus the body over `MAX_VAULT_BYTES` → `507 vault storage limit reached`.
3. If an entry exists and `X-Parent-Hash` ≠ its hash → `409 Conflict` with `{ path, current }`. Exception: if the entry is live and already has `X-Content-Hash`, the upload is a retry whose first attempt landed, and the server answers `200` without writing.
4. Otherwise: move the previous live blob to `_versions/<ts>` (version history), write the new blob, upsert the manifest row, bump `revision` and `last_write`, `200 OK`.

**`DELETE /vaults/:vault_id/files/:path`**
Soft-deletes a file. Requires `X-Parent-Hash`. On hash mismatch → `409 Conflict`.

On success: moves the blob to `_trash/<ts>`, marks the entry deleted (keeping its hash, size, and encPath so a later upload can present the tombstone's hash as parent). Blob retained for 30 days before hard deletion by the retention task.

**`POST /vaults/:vault_id/batch`**
Batch operations as `multipart/form-data`:

- one `operations` part (`application/json`): `{ "operations": [ { "action": "put", "path", "parentHash"?, "contentHash", "encPath"? }, { "action": "delete", "path", "parentHash"? } ] }`
- one `content` part per put with `filename="<operation index>"` carrying the raw encrypted bytes

`action` must be exactly `put` or `delete`; any other value (or none) rejects the whole batch with `400` before anything runs. Operations run in order, each with the same transaction as the single-file routes. The response is `200 { "results": [ { path, status, conflict } ] }`; `409` entries carry the conflicting `current` entry, so a batch can partly succeed. A non-multipart body is `415`; a body over `MAX_BATCH_BYTES` (default 128 MiB) is `413`.

**`GET /vaults/:vault_id/history/:path`**
`{ "versions": [ { "name": "1713100800", "ts": 1713100800, "size": 2060 } ] }`, newest first: the entries under `_versions/` for this path token (§8). `name` is the directory entry (`<unix>[-n]`, two versions in one second get a suffix) and is what the blob route takes; `size` is the sealed size on the server. (The path token is a wildcard segment and the router allows nothing after it, which is why the list is not under `/files/:path/versions`.)

**`GET /vaults/:vault_id/versions/:name/:path`**
That version's encrypted blob.

**`GET /vaults/:vault_id/trash`**
`{ "entries": [ { "path": "<path token>", "encPath": "…", "hash": "…", "size": 2048, "deleted_at": 1713100800 } ] }`: the manifest's tombstones (the volume's directory names are one-way, so the listing comes from `files WHERE deleted`; `deleted_at` is the tombstone's receipt time).

**`GET /vaults/:vault_id/trash/:path`**
The newest trashed blob for that path token.

There is no server-side restore. The server cannot compute the manifest `hash` of an old blob, so a restore is a client operation: fetch the blob, decrypt, write it locally, and let the next sync upload it through the conflict-gated `PUT` (with the current hash, or the tombstone's, as `X-Parent-Hash`). §8.2 and §9.3 describe the UI.

### 4.4 Attachment Size Limit

Files above **50 MB** (`MAX_FILE_BYTES`) are rejected by the server. This prevents accidental syncing of large media files. Configurable per vault at creation (`max_file_size`, capped at the server maximum). The sync engine transfers files one request at a time and skips a too-large file as a per-file failure.

### 4.5 Retention task

An in-process task runs at startup and then every `RETENTION_INTERVAL_SECS` (default daily); `obsink-server retention` runs one pass by hand.

**Version pruning** — Deletes versions older than 14 days or beyond the newest 10 per file (whichever is hit first).

**Trash purging** — Hard-deletes trash older than 30 days.

**Housekeeping** — Removes expired sessions, day-old one-time codes, and blob directories whose vault row no longer exists. Devices are never expired: the user sees and removes them.

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

### 6.1 Key hierarchy (wire format v3)

One passphrase per account; one random key per vault; the server holds only wrapped keys.

| Key | Made from | Lives |
|---|---|---|
| **KEK** (key-encryption key) | Argon2id(passphrase, `salt`) — 64 MiB / 3 iterations / 1 lane, 32 bytes; `salt` is 16 random bytes generated when the passphrase is set | derived on demand, never stored |
| **Account key** | 32 random bytes, generated once when the passphrase is first set | in the OS keychain (`account:<user id>`, with `key_id`); on the server as `AES-256-GCM(KEK, account key)` with AAD = user id, plus the salt and a `key_id`; in the browser in worker memory only |
| **Verifier** | `HMAC-SHA256(HKDF(account key, "obsink:v3:verify"), user id)` | on the server, never returned; proves a rewrap request comes from a client that holds the account key |
| **Vault key** | 32 random bytes, generated when the vault is created | in the OS keychain under the vault id, as before; on the server per member as `AES-256-GCM(HKDF(account key, "obsink:v3:vault-wrap"), vault key)` with AAD = vault id |
| **Sub-keys** | HKDF-SHA256 of the vault key: `content_enc`, `content_mac`, `path_token`, `path_enc` (unchanged from v2) | derived on demand |

Consequences:

- Unlocking a device runs Argon2id once, not once per vault; every vault key is one AES-GCM unwrap away. Download is one action.
- A wrong passphrase is a failed unwrap (the GCM tag), detected on the client before anything is downloaded. No probe file is needed.
- Changing the passphrase rewraps the account key (`PUT /auth/keys/rewrap`); nothing else changes. The account key and the vault keys are never rotated in v3.
- Vault keys do not derive from the account key, so a vault can later be shared by wrapping its key for another member. Only the wrap changes hands, never a passphrase.
- The passphrase never leaves the device, but the wrapped account key does travel to any session holder, and a database dump contains it. Argon2id at the parameters above is the defence, and clients require at least 12 characters when the passphrase is set.
- **File encryption**: AES-256-GCM with a random 96-bit nonce per file; blob = `[12-byte nonce][ciphertext][16-byte GCM auth tag]`. Unchanged.
- `PROTOCOL_VERSION = 3`. v2 vaults (passphrase-derived keys, one passphrase per vault) are not migrated: the cutover wipes the server, and v2 clients cannot sign in (§4.1).

### 6.2 What Is Encrypted, and what the server holds

- File contents: **encrypted** (stored as opaque blobs on the server volume, wrapped once more with the server key)
- File paths: **encrypted** (`encPath`) and **tokenized** (manifest keys are `HMAC(path_token_key, path)`)
- File hashes: **keyed HMACs** of plaintext content (see §3.3)
- Vault names, device names, emails: sealed with the server's envelope key (the server can read them; nothing else can)
- Account key, vault keys: **wrapped** under keys the server never has. The server holds no KEK, no account key, no vault key, and cannot check a passphrase; it can only tell whether a rewrap request knows the account key.

### 6.3 Key Storage

| Platform | Account key | Device id | Vault keys |
|---|---|---|---|
| macOS (desktop app and CLI share these) | Keychain `account:<user id>`, `key_id` alongside; `user:<canonical server URL>` names the signed-in user id so the entry can be found without a request | Keychain `device:<canonical server URL>` (`OBSINK_DEVICE_ID` overrides for harnesses) | Keychain, account = vault id |
| iOS | Keychain (app group, `AfterFirstUnlock`), same account names | Keychain | Keychain, account = vault id |
| Browser | worker memory for the tab's lifetime; a reload shows `Unlock` | IndexedDB | worker memory |

Nothing wrapped is cached in client config: a vault's wrapped key is fetched from `GET /vaults` when the vault is downloaded or created, unwrapped once, and the unwrapped key is what the keychain keeps. A keychain account key whose `key_id` no longer matches `GET /auth/keys` (the losing side of the first-set race in §4.1) is discarded and the client asks for the passphrase again.

### 6.4 No Key Recovery

There is no key recovery mechanism. Lost passphrase = lost data. This is a deliberate design choice. A passphrase change (rewrap) needs the current passphrase on a device that is already unlocked.

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

`History` on the vault page (§15): pick a file, see its versions (timestamp, size) from `GET …/versions`, open one as a decrypted read-only preview, and `Restore` it. Restore writes the version over the local file; the next sync uploads it as an ordinary change, conflict-gated against whatever the server holds by then. A version of a file that has since been deleted is restored the same way (the upload presents the tombstone's hash as its parent).

---

## 9. Deletions

### 9.1 Soft Delete

When a file is deleted locally and synced, the server moves the blob to `_trash/{vault_id}/{path}/{timestamp}` and marks the manifest entry with `"deleted": true`.

Other clients see the deletion flag on next sync and remove the file locally.

### 9.2 Retention

Trashed files are retained for **30 days**. Purged by the daily retention task (§4.5).

### 9.3 Recovery

`Recently deleted` under `History` on the vault page (§15): the tombstones from `GET /vaults/:id/trash` with their real paths (decrypted `encPath`), sizes and deletion times, a decrypted preview, and `Restore`. Restore writes the file back into the local folder; the next sync uploads it with the tombstone's hash as its parent.

---

## 10. Multi-Vault Support

### 10.1 Data Isolation

Each vault has:
- A unique `vault_id`
- Its own blob directories (`live/{vault_id}/`, `_versions/{vault_id}/`, `_trash/{vault_id}/`)
- Its own manifest rows (`files` where `vault_id = ...`) and revision counter
- Its own random key (§6.1), wrapped per member

### 10.2 Vault Management

The server keeps a `vaults` table with an owner and a `vault_members` table (v1: the owner only). Clients list, create, rename and delete the vaults their account is a member of, and tell the server which device holds which vault.

### 10.3 Client UX

The vault list on every client is the account's list, not the device's. Each row shows the vault's state on this device (§15), or `Not on this device` with a `Download` action for a vault the account owns that this device does not hold. Download asks for a folder on desktop and in the browser (always a picker, no default location) and uses the app's own container on iOS, registers the device with `PUT /vaults/:id/devices/self`, and runs the first sync. `Create vault` asks for a name and (on desktop and the browser) a folder; the passphrase is never asked again after the unlock. A vault can be renamed from any device. On desktop, `Move folder` re-points a vault at another folder (the `.obsink/` bookkeeping moves with it, so the next sync is a no-op). `Remove from this device` detaches the device and drops the vault key from the keychain; `Delete vault on server` removes it for every device. On iOS each vault has its own cache directory, item database, and File Provider location (§11.2).

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

### 12.1 Sign in and unlock

1. **Sign in** with an email code or Sign in with Apple (iOS). Each build talks to one server (baked in at build time; the CLI also takes `--server-url`), so there is no URL to enter. New accounts need an invite code unless the server has no users yet. The client sends its device id, name and platform; the session token goes to the keychain, so this happens once per device.
2. **Unlock.** The client calls `GET /auth/keys`.
   - `null` (a new account): `Set passphrase`, twice, at least 12 characters. The client generates the salt and the account key, wraps the key, computes the verifier, and `PUT /auth/keys`. A `409` means another device set it first: the generated key is discarded and the screen becomes `Unlock` with the message `A passphrase was already set on another device. Enter it.`
   - a key: `Unlock`. The client derives the KEK, unwraps the account key (a failed tag is `Passphrase does not match this account.`), and stores it in the keychain with the `key_id`.
   A crash between sign-in and the `PUT` is harmless: the next launch finds `null` again.
3. **The vault list** (`GET /vaults`). The first account on a fresh server sees `No vaults yet.` and `Create vault`.

### 12.2 First vault

`Create vault`: name, then a folder (desktop, browser) — an existing Obsidian vault folder is fine, its files are uploaded on the first sync. The client generates the vault key, wraps it, `POST /vaults`, `PUT /vaults/:id/devices/self`, stores the vault key in the keychain, and syncs.

### 12.3 Another device

Sign in (a new device row), unlock (the same passphrase; the unwrap is the check), and the list shows every vault as `Not on this device`. `Download` on each one the user wants here: folder (desktop, browser), `GET /vaults` supplies the wrapped key, unwrap, register the device, first sync pulls all files.

---

## 13. Repo Structure

```
obsink/
├── core/                     Rust shared library (the pure modules also build for wasm32)
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs            public API surface; cfg-gates the IO modules off wasm32
│       ├── crypto.rs         Argon2id KEK, key wrapping, verifier, HKDF sub-keys, AES-256-GCM, path tokens
│       ├── manifest.rs       manifest diffing, conflict detection, checkpoint
│       ├── hasher.rs         keyed content hashing (HMAC-SHA256) + the (mtime, size) hash cache
│       ├── ignore.rs         built-in and per-vault ignore rules
│       ├── sync_rules.rs     pure sync decisions: upload batching, effective conflict choice, copy names
│       ├── pacing.rs         daemon pacing: poll intervals, exponential backoff
│       ├── server_url.rs     URL normalization and the legacy host alias table
│       ├── api_client.rs     HTTP client for the server (ETag cache, multipart batch, history, device report)
│       ├── auth.rs           sign-in with device identity, account keys, devices, invites
│       ├── keychain.rs       macOS Keychain (or a directory of files) for keys, the bearer and the device id
│       ├── sync_engine.rs    orchestrates the full sync cycle
│       ├── daemon.rs         the driver: watcher + poll + backoff around the engine
│       └── types.rs          shared types (SyncResult, Conflict, FileEntry, etc.)
├── core-wasm/                wasm-bindgen bindings over the pure core for the browser client
├── server/                   self-hosted server (axum)
│   ├── Cargo.toml            depends on ../core
│   ├── Dockerfile            build context = repo root
│   ├── migrations/           sqlx migrations
│   ├── src/
│   │   ├── main.rs           serve | migrate | invite | retention | keygen | healthcheck
│   │   ├── config.rs         env vars, server key
│   │   ├── crypto.rs         envelope encryption
│   │   ├── blobs.rs          filesystem blob store
│   │   ├── auth/             session principal, email, apple, sessions, devices, keys, invites
│   │   ├── routes/           vaults (members, devices), files, batch, history (versions, trash), me
│   │   └── retention.rs      pruning task
│   └── tests/                DATABASE_URL-gated integration tests
├── ui/                       shared React screens + the Backend interface (npm workspace, TS source)
│   └── src/
│       ├── backend.tsx       Backend interface, BackendProvider, Platform nouns
│       ├── settings/, popover/, components/, hooks/, lib/
│       └── styles.css
├── desktop/                  Tauri v2 menu-bar app
│   ├── src-tauri/
│   │   ├── Cargo.toml        depends on ../core
│   │   ├── tauri.conf.json   bundle, windows, CSP
│   │   └── src/main.rs       Tauri commands wrapping core, tray, windows
│   └── src/
│       ├── backend.ts        the Backend over Tauri invoke/listen
│       └── main.tsx          entry: PopoverApp or SettingsApp by window label
├── web/                      browser client at /app + the website container
│   ├── src/
│   │   ├── main.tsx, App.tsx  entry, error boundary, visibility wiring
│   │   ├── backend.ts        the Backend over a worker; folder picking on the page
│   │   ├── shared/           IndexedDB layer, worker protocol
│   │   └── worker/           fetch layer, account, keys, vaults, fs (File System Access), sync, driver, activity
│   ├── Dockerfile            wasm-pack + npm build, Caddy
│   └── Caddyfile             site at /, client at /app, API paths proxied to the server
├── site/                     landing page (index.html, icon.svg, robots.txt) and install.sh
├── cli/                      `obsink` CLI (reference client)
│   ├── Cargo.toml
│   └── src/main.rs
├── mobile/                   UniFFI facade over core for iOS
├── ios/                      Xcode project (XcodeGen): app, File Provider, tests
├── design/                   icon.svg + tray.svg (rasters via scripts/gen-icons.sh)
├── docs/                     self-hosting, architecture, platforms, troubleshooting
├── scripts/                  build-ios, release-ios, testflight.py, ci-import-signing-cert, gen-icons,
│                             verify-* harnesses (server, CLI, iOS sim, web e2e, desktop live + smoke),
│                             test-web-container, test-install-sh, check-commit-msg, merge-pr
├── package.json              npm workspaces: ui, desktop, web (lint, format, typecheck, build, test)
├── docker-compose.yml        local stack: server and web (built), Postgres, Mailpit
├── docker-compose.coolify.yml production stack: server and web built from source, Postgres
├── AGENTS.md, DESIGN.md      agent rules; UI rules (tokens, components, copy)
├── lefthook.yml              git hooks: rustfmt, prettier, commit message
├── deny.toml                 cargo-deny policy
├── rust-toolchain.toml       pinned Rust version
└── .github/
    ├── pull_request_template.md
    ├── rulesets/main.json    branch ruleset for main (applied with gh api)
    └── workflows/
        ├── ci.yml            commit check, fmt/clippy/tests, cargo-deny, server on Postgres, web (wasm + client + image + route checks), desktop lint+build+install.sh test, iOS simulator tests
        └── release.yml       on v*.*.* tags (or a manual dry run): verify versions, build the universal CLI and the signed, notarized universal DMG, publish once
```

---

## 14. Non-Functional Requirements

- **Privacy:** Server never sees plaintext. All encryption/decryption happens on-device; the server holds keys only in wrapped form.
- **Cost:** one small VPS (or any Docker host) runs the server, Postgres, and the blob volume for a household of users; no per-request pricing.
- **Reliability:** One sync at a time per vault, driven by explicit calls into an engine without timers, means no data races. Conflict detection means no silent data loss.
- **Portability:** Rust core compiles to every target platform; the server is one static binary in a distroless image. No platform lock-in beyond the iOS File Provider.
- **Simplicity:** Minimal moving parts. One engine call does everything; the drivers (foreground and background refresh, the daemon) only decide when to make it.

---

## 15. Client Information Architecture

The same three nouns on every client: vaults, devices, the account. `DESIGN.md` §5 holds the exact strings; this section says what goes where.

### 15.1 Vault list

Every vault the account is a member of, from `GET /vaults` merged with the device's own records. Per row: state dot, name, the state text. States on this device: `Up to date`, `n to upload · n to download`, `n conflicts`, `Syncing…`, `Offline`, `Session expired`, `Error: …`, `Needs folder access` (browser), `Unlock` (browser, after a reload); and for a vault this device does not hold: `Not on this device` with `Download`. A vault the server no longer lists (deleted elsewhere) is shown once as `Deleted on the server` with `Remove from this device`. The desktop popover shows the same list, `Download` rows included, and stays a place without forms: `Download` opens the settings window at the folder picker. `Create vault` sits below the list.

### 15.2 Vault page

- **Status**: state line, `Last synced <relative>`, per-vault usage, the local path with `Open folder`, `Sync now`, notices (stale, pending local, checkpoint failed), `Conflicts`.
- **Devices**: every device that holds this vault, from `GET /vaults`: name, platform, `Last synced <relative>`, and how far behind the server it is (`n revisions behind`, or `Up to date`). This device is tagged `This device`. Read-only here; sign-out lives on the Devices tab.
- **Activity**: this vault's log (uploads, downloads, deletions, conflicts, errors, sync summaries), newest first.
- **History**: `File history` (pick a file, list versions, preview, `Restore`) and `Recently deleted` (tombstones with real paths, preview, `Restore`). §8.2, §9.3.
- **Manage vault**: `Rename`, `Move folder` (desktop), `Remove from this device`, `Delete vault on server`.

### 15.3 Devices tab

One row per device of the account, from `GET /auth/me`: name (editable in place, `Rename`), platform, `Last seen <relative>`, `This device` tag, the vaults it holds as a muted line, and `Sign out`. Signing out this device is the ordinary sign-out; signing out another device is `DELETE /auth/devices/:id`. The revoked device shows `Session expired` on its next request and keeps its folders and keys.

### 15.4 Settings tab

Signed in as (email or user id), the server in mono, usage across vaults, `Change passphrase` (current, new twice), `Invite someone` and the invite list, `Sign out`, `Delete account`. When signed out: the sign-in form. When signed in and locked (browser after a reload, or a lost first-set race): the unlock form, above everything else.

### 15.5 Protocol gate

`GET /` on launch. `protocol` other than the client's: one page, `Update ObSink`, with the download link; nothing else runs.

### 15.6 iOS

The same three tabs (`Vaults`, `Devices`, `Settings`). The vault list is a scroll of cards with the same states and a `Download` button on a `Not on this device` card; the vault page is the pushed `Manage` screen with the sections above. `Unlock` and `Set passphrase` are steps of the sign-in sheet.
