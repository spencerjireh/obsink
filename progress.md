# ObSink — Progress & Phases

> Track implementation progress. Each phase builds on the previous and produces a usable artifact.

---

## Phase 1: Rust Core Library + CLI Tool

**Goal:** A working sync engine you can test from the command line. This is the foundation everything else builds on.

**Estimated effort:** 1–2 weekends

### 1.1 Project Setup
- [x] Initialize Rust workspace (`obsink/` root with `core/` and `cli/` members)
- [x] Set up `Cargo.toml` with initial dependencies: `aes-gcm`, `argon2`, `sha2`, `reqwest`, `serde`, `serde_json`, `tokio`
- [x] Establish module structure: `lib.rs`, `crypto.rs`, `manifest.rs`, `hasher.rs`, `api_client.rs`, `sync_engine.rs`, `types.rs`
- [x] Define core types: `FileEntry`, `Manifest`, `SyncResult`, `Conflict`, `SyncAction`, `VaultConfig`

### 1.2 Crypto Module
- [x] Implement Argon2id key derivation (passphrase → 256-bit key)
- [x] Implement AES-256-GCM encrypt (plaintext → `[nonce][ciphertext][tag]`)
- [x] Implement AES-256-GCM decrypt (blob → plaintext, with auth verification)
- [x] Unit tests: round-trip encrypt/decrypt, wrong key rejection, tampered ciphertext detection

### 1.3 Hashing Module
- [x] Implement SHA-256 hashing of file contents
- [x] Implement manifest generation from a local directory (walk directory → hash each file → produce manifest)
- [x] Unit tests: deterministic hashing, empty file handling, binary file handling

### 1.4 Manifest Diffing
- [x] Implement manifest comparison: given local manifest + server manifest, produce three lists (upload, download, conflict)
- [x] Handle new files (present locally, absent on server and vice versa)
- [x] Handle deleted files (present in manifest with `deleted: true` flag)
- [x] Unit tests: all diff scenarios (new, modified, deleted, conflict, no changes)

### 1.5 API Client
- [x] Implement `GET /vaults/:id/manifest`
- [x] Implement `GET /vaults/:id/files/:path`
- [x] Implement `PUT /vaults/:id/files/:path` with `X-Parent-Hash` and `X-Content-Hash` headers
- [x] Implement `DELETE /vaults/:id/files/:path` with `X-Parent-Hash`
- [x] Implement `POST /vaults/:id/batch`
- [x] Implement `GET /vaults` and `POST /vaults`
- [x] Handle `409 Conflict` responses and surface them as `Conflict` types
- [x] Handle auth (Bearer token in headers)

### 1.6 Sync Engine
- [x] Implement full sync orchestration: pull manifest → diff → download → detect conflicts → return actions
- [x] Sync engine should NOT resolve conflicts — it returns them for the UI layer to handle
- [x] After UI resolution, sync engine uploads changes and updates local manifest
- [x] Implement local manifest persistence (read/write JSON manifest to disk)
- [x] Handle first-time sync (empty local manifest, full download)

### 1.7 CLI Tool
- [x] `obsink init` — create a new vault on the server, set up local config
- [x] `obsink connect` — connect to an existing vault, enter passphrase, initial pull
- [x] `obsink sync` — run full sync cycle, print conflicts to stdout, prompt for resolution
- [x] `obsink status` — show local manifest status from a local directory
- [x] Store config (worker URL, API key, vault ID) in `~/.obsink/config.toml`
- [x] Store encryption key in platform keychain (macOS Keychain for now)
- [x] End-to-end test: create files → sync up → modify on "another device" (curl) → sync down → verify

Phase 1 status: complete. Verified by Rust tests covering first-time pull, upload, and conflict resolution flows plus CLI-backed config and key storage paths.

---

## Phase 2: Cloudflare Worker Backend

**Goal:** A deployed API that the CLI tool can talk to. Testable with curl independently.

**Estimated effort:** 1 weekend

### 2.1 Project Setup
- [x] `wrangler init` in `worker/` directory
- [x] Create R2 bucket (e.g., `obsink-files`)
- [x] Create KV namespace (e.g., `obsink-meta`)
- [x] Configure `wrangler.toml` with bindings
- [x] Set API key as Worker secret

### 2.2 Vault Management
- [x] `GET /vaults` — list all vaults from KV
- [x] `POST /vaults` — create new vault, generate vault ID, initialize empty manifest in KV

### 2.3 Manifest Endpoint
- [x] `GET /vaults/:vault_id/manifest` — read manifest from KV and return

### 2.4 File Operations
- [x] `GET /vaults/:vault_id/files/:path` — read blob from R2, return raw bytes
- [x] `PUT /vaults/:vault_id/files/:path` — conflict detection logic:
  - Read current manifest
  - Compare `X-Parent-Hash` against manifest hash
  - On match or new file: copy current blob to `_versions/` prefix, write new blob to R2, update manifest, return `200`
  - On mismatch: return `409` with current server metadata
- [x] `DELETE /vaults/:vault_id/files/:path` — soft delete:
  - Conflict check with `X-Parent-Hash`
  - Move blob to `_trash/` prefix
  - Mark `deleted: true` in manifest
- [x] Enforce 50MB file size limit on uploads

### 2.5 Batch Endpoint
- [x] `POST /vaults/:vault_id/batch` — accept array of operations
- [x] Process each operation independently, return per-operation results
- [x] Handle mix of successes and 409s in single response

### 2.6 Cron Jobs
- [x] Version pruning cron: list `_versions/`, delete entries older than 14 days or beyond 10 per file
- [x] Trash purging cron: list `_trash/`, delete entries older than 30 days
- [x] Configure both in `wrangler.toml` as Cron Triggers

### 2.7 Auth Middleware
- [x] Validate `Authorization: Bearer <token>` on all requests
- [x] Return `401` on missing/invalid token

### 2.8 Integration Testing
- [x] Test all endpoints with curl
- [x] Test conflict detection: upload file, upload again with wrong parent hash, verify 409
- [x] Test soft delete and verify blob appears in `_trash/`
- [x] Test batch with mixed operations
- [x] Connect CLI tool to deployed Worker, run full sync cycle

Phase 2 status: complete and deployed. The Worker is live at `https://obsink-worker.spencer-080.workers.dev` with a KV namespace (`META`), R2 bucket (`obsink-files`), the `API_KEY` secret, and both pruning crons. Local Vitest coverage (manifest/file fetch, soft delete, size rejection, batch, version + trash pruning) is green, and the deployed endpoint passed both live verification scripts: `verify-worker-deploy.sh` (all endpoints incl. a real 409, batch, delete) and `verify-cli-deployed-sync.sh` (two-device CLI sync with bidirectional propagation and an interactively resolved conflict).

---

## Phase 3: Tauri macOS Desktop App

**Goal:** A menu bar app on macOS that syncs your vault with one click. Your daily driver.

**Estimated effort:** 1–2 weekends

### 3.1 Project Setup
- [x] Initialize Tauri v2 project in `desktop/`
- [x] Add `core/` as a Cargo dependency in `src-tauri/Cargo.toml`
- [x] Choose frontend framework (Svelte, React, or Vue) and scaffold
- [x] Decide on and configure system tray / menu bar behavior

### 3.2 Tauri Commands (Rust → Frontend Bridge)
- [x] `sync_vault` — triggers full sync, returns sync result (including conflicts)
- [x] `resolve_conflict` — accepts user's choice (keep local / keep remote / keep both) per file
- [x] `get_status` — returns current sync state (last sync time, pending changes count)
- [x] `get_vaults` — list configured vaults
- [x] `add_vault` — setup flow for new vault (create or connect)
- [x] `get_manifest_diff` — lightweight check for stale vault warning

### 3.3 UI Screens
- [x] **Main view:** sync button, sync status summary, configured vault list, and setup form
- [x] **Vault switching:** choose the active vault when multiple vaults are configured
- [x] **Stale vault warning banner:** lightweight manifest check surfaced in the UI on app open
- [x] **Conflict resolution preview:** side-by-side or tabbed preview of both versions before choosing a winner
- [x] **Conflict resolution actions:** pick keep local / keep remote / keep both for each conflicted file
- [x] **Settings / setup:** Worker URL, API key, create/connect flow, passphrase, and local folder path

### 3.4 Keychain Integration
- [x] Store/retrieve encryption keys via macOS Keychain from Rust core
- [x] Store Worker URL and API key in config file (`~/.obsink/config.toml` or Tauri app data)

### 3.5 Vault Folder Management
- [x] Configure which local folder maps to which vault
- [x] Obsidian opens this folder as a vault — no special integration needed on desktop

### 3.6 Testing
- [ ] Full sync cycle through the UI
- [ ] Conflict resolution flow (create conflict manually, verify UI works)
- [ ] Stale vault warning (modify file via curl on server, open app, verify banner)
- [ ] Multiple vault switching

Phase 3 status: feature-complete in code; pending live verification. The repo contains a Tauri v2 + React desktop app with core-backed sync/status/setup commands, local app config, macOS keychain integration, active-vault switching, stale-vault warnings, and conflict previews. Menu-bar behavior is now wired: a tray icon with Sync Now / Show / Quit, left-click to surface the window, close-to-tray so background sync survives, and a `tray://sync-now` event that drives the existing frontend sync flow. A Tauri capabilities file (`capabilities/default.json`) was added so `invoke`/`listen` are permitted at runtime. Remaining work (§3.6) is running the full flow end-to-end inside the actual Tauri shell against a deployed Worker — gated on Worker deployment.

---

## Phase 4: iOS App + File Provider

**Goal:** Sync your vault to your iPhone/iPad with Obsidian seeing the files natively.

**Estimated effort:** 2–3 weekends (File Provider is the hard part)

### 4.1 Xcode Project Setup
- [ ] Create new Xcode project with App + File Provider extension targets
- [ ] Configure App Group for shared container between app and extension
- [ ] Set up UniFFI: compile Rust core for `aarch64-apple-ios`, generate Swift bindings
- [ ] Verify Rust core functions are callable from Swift

### 4.2 Shared Database
- [ ] Add GRDB (or similar SQLite wrapper) to the project
- [ ] Create `items` table in shared App Group container (see spec §11.5 for schema)
- [ ] Implement basic CRUD operations on item database
- [ ] Verify both main app and extension can read/write the database

### 4.3 File Provider Extension (The Hard Part)
- [ ] Implement `NSFileProviderReplicatedExtension`
- [ ] Implement `enumerateChanges` — report items added/modified/deleted based on database state
- [ ] Implement `fetchContents` — serve decrypted files from local cache in app group container
- [ ] Implement `createItem` — handle new files written by Obsidian, set `pendingUpload = true`
- [ ] Implement `modifyItem` — handle file edits by Obsidian, update local cache and database
- [ ] Implement `deleteItem` — handle deletions by Obsidian, set `pendingDeletion = true`
- [ ] Implement proper sync anchor tracking (incrementing integer or timestamp)
- [ ] Assign stable UUID identifiers to items (never use paths as identifiers)
- [ ] **Validation checkpoint:** static test files appear in iOS Files app and Obsidian can open them as a vault

### 4.4 Sync Engine Integration
- [ ] Build sync screen in SwiftUI: sync button, status display, last sync time
- [ ] Wire sync button to Rust core (via UniFFI): pull manifest → diff → download → return conflicts
- [ ] After sync, write downloaded files to app group container, update item database
- [ ] Call `NSFileProviderManager.signalEnumerator(for:)` to notify extension of changes
- [ ] Handle upload of `pendingUpload` items through Rust core
- [ ] Handle `pendingDeletion` items

### 4.5 Conflict Resolution UI
- [ ] Conflict list screen (count + file names)
- [ ] Conflict detail screen (segmented control: "This device" / "Other device")
- [ ] Read-only preview of each version's content
- [ ] Show last-modified timestamp for each version
- [ ] Action buttons: Keep local, Keep remote, Keep both
- [ ] Wire resolution choices back to sync engine to complete sync

### 4.6 Settings & Setup
- [ ] Settings screen: Worker URL, API key, vault management
- [ ] First-run setup flow: enter URL, API key, create/connect vault, enter passphrase
- [ ] Store encryption keys in iOS Keychain
- [ ] Vault picker if multiple vaults configured

### 4.7 Testing
- [ ] Create note on Mac → sync → verify appears on iOS via File Provider
- [ ] Edit note on iOS → sync → verify change appears on Mac
- [ ] Create conflict → verify resolution UI works
- [ ] Delete on one device → sync → verify deletion propagates
- [ ] Stale vault warning on app open
- [ ] Test with real Obsidian vault (plugins, images, .obsidian config)

Phase 4 status: foundation verified, not started in earnest. The `aarch64-apple-ios` and `aarch64-apple-ios-sim` Rust targets are installed and `obsink-core` cross-compiles cleanly for both (reqwest's `rustls-tls` means no OpenSSL cross-compile barrier). Remaining work is the bulk of the phase and needs hands-on Xcode/design work that can't be driven headlessly: the UniFFI binding surface (the core exposes async `prepare_sync`/`complete_sync` and rich types that need a deliberate FFI design), generating Swift bindings + an XCFramework, and the Xcode App + File Provider extension project with App Group entitlements.

---

## Phase 5: Android (Tauri Mobile)

**Goal:** Bring ObSink to Android.

**Estimated effort:** 1–2 weekends

### 5.1 Tauri Mobile Setup
- [ ] Configure Tauri v2 mobile build for Android in `desktop/` (or separate `android/` directory)
- [ ] Verify Rust core compiles for Android targets (`aarch64-linux-android`, `armv7-linux-androideabi`)
- [ ] Build and run basic Tauri app on Android emulator

### 5.2 Android File Access
- [ ] Determine how Obsidian Android accesses vault files (folder on internal storage)
- [ ] Configure Tauri app to sync files to a folder Obsidian can open
- [ ] If deeper integration needed: evaluate implementing a DocumentsProvider via Kotlin plugin
- [ ] **Fallback:** if Tauri mobile isn't mature enough, pivot to React Native with Rust via `react-native-rust-bridge` or JNI

### 5.3 UI
- [ ] Reuse desktop web UI (sync button, conflict resolution, settings)
- [ ] Adapt layout for mobile screen sizes
- [ ] Test touch interactions on conflict resolution flow

### 5.4 Testing
- [ ] Full sync cycle on Android
- [ ] Obsidian Android opens synced vault folder
- [ ] Conflict resolution flow
- [ ] Cross-device: Mac → server → Android → server → iOS → verify consistency

---

## Phase 6: Windows & Linux Desktop

**Goal:** Complete platform coverage.

**Estimated effort:** 1 weekend (mostly build/packaging)

### 6.1 Windows
- [ ] Cross-compile Rust core for `x86_64-pc-windows-msvc`
- [ ] Build Tauri app for Windows
- [ ] Implement keychain storage via DPAPI / Credential Manager
- [ ] Test full sync cycle
- [ ] Test Obsidian opening synced vault folder
- [ ] Package as `.msi` or `.exe` installer

### 6.2 Linux
- [ ] Cross-compile Rust core for `x86_64-unknown-linux-gnu`
- [ ] Build Tauri app for Linux
- [ ] Implement keychain storage via libsecret / kwallet
- [ ] Test full sync cycle
- [ ] Package as `.AppImage` or `.deb`

Linux CI status: GitHub Actions now runs the Rust core + CLI test suite on `ubuntu-latest` and builds/bundles a Linux `.deb` of the desktop app (`linux-bundle` job, uploaded as an artifact). The boxes above stay unchecked pending a verified green CI run and the libsecret key-store backend — the desktop currently shells out to the macOS `security` tool, so the Linux build compiles and bundles but key storage is not yet functional there.

---

## Phase 7: Polish & Hardening

**Goal:** Make ObSink reliable and pleasant for daily use.

**Estimated effort:** Ongoing

### 7.1 Reliability
- [x] Handle network failures gracefully (retry logic, timeout handling, offline detection)
- [ ] Handle partial sync failures (some files uploaded, then network drops)
- [ ] Handle corrupted local cache (re-pull from server)
- [ ] Handle manifest corruption (rebuild from R2 file listing)
- [x] Add logging throughout Rust core (configurable verbosity)

Reliability/logging status: the API client now uses a 30s per-request timeout and retries transient failures (timeouts, connection errors) up to 3 times with exponential backoff; non-transient errors and HTTP status errors surface immediately as typed `ApiError`s. Structured logging uses `tracing` across the core (request-level `debug`, sync-plan `info`, retry `warn`); the CLI installs a stderr subscriber controlled by `RUST_LOG` (default `warn`). Still open: partial-sync recovery, corrupted-local-cache re-pull, and manifest rebuild from an R2 listing (the last needs a new Worker list endpoint).

### 7.2 UX Improvements
- [ ] Progress indicator during sync (file count, bytes transferred)
- [ ] Sync history log (last N syncs with timestamps and summary)
- [ ] "Recently deleted" view for recovering trashed files
- [ ] "Version history" view per file (list past versions, preview, restore)
- [ ] QR code pairing for adding new devices (encode Worker URL + API key)
- [ ] Badge/notification when vault is stale for extended period

### 7.3 Performance
- [ ] Delta sync: only transfer changed bytes for large files (rsync-style, future consideration)
- [ ] Parallel file uploads/downloads (configurable concurrency)
- [ ] Manifest caching with ETag/If-None-Match to skip unchanged manifests

### 7.4 Security
- [x] HMAC file hashes with encryption key (eliminate information leak from plaintext hashes)
- [x] Encrypt file paths in manifest (path → HMAC key, store encrypted path index)
- [x] Audit Argon2id parameters (memory cost, iterations) for adequate strength
- [ ] Pin TLS certificates for Cloudflare Worker endpoint (optional/paranoid)

Wire format v2 (`PROTOCOL_VERSION = 2`): the master key (Argon2id) is now only HKDF input; four purpose-separated sub-keys are derived (content encryption, content MAC, path token, path encryption). The server stores entries keyed by a deterministic `HMAC(path_token_key, path)` token, with the manifest `hash` being `HMAC(content_mac_key, plaintext)` and an `encPath` (AES-GCM of the real path) so a fresh device can recover filenames. The server never sees plaintext paths or content-derived hashes. Verified live: the deployed manifest is token-keyed with HMAC hashes and `encPath`. Argon2id audited at 64 MiB / 3 / 1 (exceeds OWASP 2024). TLS pinning intentionally deferred (optional/paranoid).

### 7.5 Documentation
- [x] README with project overview and quickstart
- [x] Self-hosting guide (setting up your own Cloudflare account, Worker, R2, KV)
- [x] Per-platform setup guide (macOS, iOS, Android, Windows, Linux)
- [x] Architecture doc for contributors
- [x] Troubleshooting guide (common sync issues, conflict scenarios)

Documentation status: `README.md` plus `docs/self-hosting.md`, `docs/architecture.md`, `docs/platforms.md`, and `docs/troubleshooting.md` are written against the deployed v2 system (joining the existing `docs/deploy.md`). The platform guide records per-OS status; the architecture doc documents the v2 wire format and crypto.

---

## Summary

| Phase | What you get | Effort |
|---|---|---|
| 1. Rust core + CLI | Working sync engine, testable from terminal | 1–2 weekends |
| 2. Cloudflare Worker | Deployed backend, testable with curl | 1 weekend |
| 3. Tauri macOS app | Daily-driver desktop sync app | 1–2 weekends |
| 4. iOS app + File Provider | iPhone/iPad sync with native Obsidian integration | 2–3 weekends |
| 5. Android | Android sync | 1–2 weekends |
| 6. Windows & Linux | Full platform coverage | 1 weekend |
| 7. Polish | Reliability, UX, security hardening | Ongoing |

**Total to a working Mac + iOS setup (Phases 1–4): ~5–8 weekends**
