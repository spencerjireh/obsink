# ObSink

Self-hosted, end-to-end encrypted sync for [Obsidian](https://obsidian.md) vaults. A shared Rust
core builds and diffs manifests, encrypts locally, and talks to a small Rust server you run with
`docker compose up` (Postgres for metadata, a volume for blobs). The server stores ciphertext, path
tokens, and keyed hashes — it holds no vault key material and performs no decryption.

Sync is driven, not ambient: one engine call runs a full cycle, and the clients make that call on
a tap, when the iOS app comes to the foreground or gets a background refresh, or from the
desktop/CLI daemon. Obsidian opens the synced directory as a normal vault; no plugin is involved.

## Design

- **Client-side crypto.** Argon2id (64 MiB, t=3, p=1) over passphrase + vault ID → 32-byte master
  key → four HKDF-SHA256 sub-keys for content encryption, content MAC, path tokens, and path
  encryption. Contents are AES-256-GCM.
- **Opaque server view.** Manifest entries are keyed by `HMAC(path_token_key, path)`, so filenames
  never leave the device. The reversible `encPath` (AES-GCM of the real path) lets a freshly
  connected device recover paths and re-key the manifest locally.
- **Keyed content hashes.** The manifest `hash` is `HMAC(content_mac_key, plaintext)`, not a bare
  SHA-256 — stable enough for equality checks, useless to a server trying to confirm you store a
  known file.
- **Conflict gating.** Every `PUT`/`DELETE` carries `X-Parent-Hash`. A mismatch against the stored
  manifest returns `409` with the current entry, so racing or retried writes surface a conflict
  instead of clobbering. The check runs in one database transaction. The engine never
  auto-resolves; resolution (keep local / remote / both) is a client decision.
- **Server-side envelope.** Blobs and sensitive metadata (emails, vault names, device names) are
  wrapped again with a server key derived from `OBSINK_SERVER_KEY`, so a copied volume or database
  dump is useless on its own.
- **Invite-only accounts.** The first sign-up on a fresh server is open; every later account needs
  an invite code minted by an existing user. Sign in with an emailed one-time code (SMTP) or Sign in
  with Apple on iOS.
- **Retention.** Overwrites and deletes move to `_versions/` and `_trash/`; a daily task prunes to
  10 versions or 14 days, and 30 days respectively.

Wire protocol is `PROTOCOL_VERSION = 2`. Full manifest schema, key-derivation table, and rationale
in [docs/architecture.md](docs/architecture.md).

## Sync cycle

`prepare_sync` → resolve conflicts → `complete_sync`:

1. Walk the vault directory into a working manifest (keyed hash, size, mtime) and load the base —
   the last-sync `.obsink/manifest.json` — which also identifies local deletions.
2. Fetch the remote manifest (conditionally: the last ETag lives in
   `.obsink/remote-manifest.json`, so an unchanged manifest is a `304`) and re-key it by real path
   via `encPath`.
3. `diff_manifests` compares base, local, and remote by content hash: a side changed when its hash
   differs from the base; only one side changed → upload or download; both changed to different
   content → conflict. Timestamps never decide.
4. Downloads apply immediately; uploads and conflicts return as a `SyncPlan`.
5. The client resolves conflicts, then `complete_sync` uploads with parent-hash gating, returns late
   409s as another round of conflicts, and checkpoints the new manifest. Transfers are best-effort —
   per-file failures are skipped while the batch continues, fatal failures stop early, and failed or
   conflicted paths are held back from the checkpoint so a dropped sync resumes next time. Per-file progress streams to each client (CLI stderr,
   a desktop Tauri event, an iOS callback).

## Server API

Bearer-token auth on every vault route (a session token, or the operator `OBSINK_API_KEY` for
scripts). Full contract in [spec.md §4](spec.md).

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/` | Sign-in methods offered; whether an invite is required |
| `POST` | `/auth/email/start`, `/auth/email/verify`, `/auth/apple` | Sign in |
| `GET` | `/auth/me` | Account, devices, storage usage |
| `POST` | `/auth/invites` | Mint an invite code |
| `GET` | `/vaults` | List vault summaries |
| `POST` | `/vaults` | Create a vault |
| `GET` | `/vaults/:id/manifest` | Fetch the token-keyed manifest (`ETag` / `If-None-Match`) |
| `GET` | `/vaults/:id/files/:token` | Download a blob |
| `PUT` | `/vaults/:id/files/:token` | Upload (requires `X-Parent-Hash`) |
| `DELETE` | `/vaults/:id/files/:token` | Delete (requires `X-Parent-Hash`) |
| `POST` | `/vaults/:id/batch` | Batched operations as `multipart/form-data` |

Blobs live under `blobs/live/<vaultId>/…`, with history under `blobs/_versions/<vaultId>/…/<unix>`
and `blobs/_trash/<vaultId>/…/<unix>` on the server's data volume.

`ApiClient` enforces a 30s per-request timeout and retries transient failures up to 3× with
exponential backoff; HTTP status errors surface immediately as typed `ApiError`s.

## Repository layout

| Path | Contents |
|---|---|
| `core/` | Sync engine: `crypto`, `hasher`, `manifest`, `sync_engine`, `api_client`, `auth`, `types`; the pure modules also build for wasm32 |
| `core-wasm/` | wasm-bindgen bindings over the pure core (keys, diff, rules) for the browser client |
| `cli/` | `obsink` CLI — the reference client |
| `server/` | `obsink-server` (axum): accounts, invites, vaults, files, batch, retention; Dockerfile |
| `ui/` | The React screens and the `Backend` interface they run on (npm workspace shared by `desktop/` and the browser client) |
| `desktop/` | Tauri v2 shell (menu-bar on macOS) and its Tauri `Backend` |
| `web/` | Browser client at `/app` (Chrome, Edge): the same screens over a worker that syncs a local folder through the File System Access API |
| `ios/`, `mobile/` | SwiftUI app + File Provider extension over the shared core |
| `docker-compose.yml` | Local stack: server built from the checkout, Postgres, Mailpit |
| `site/` | The landing page and `install.sh`, served by the `web` container next to the client |
| `docker-compose.coolify.yml` | Production stack: server and website built from source by Coolify, Postgres |
| `scripts/` | iOS build/release tooling and live verification harnesses |
| `docs/` | Self-hosting, architecture, platform, troubleshooting |

## Install

The public server's site, [obsink.spencerjireh.com](https://obsink.spencerjireh.com), has the
signed macOS app (universal DMG), the command line tool, the browser client at `/app` (Chrome,
Edge) and the TestFlight note. The CLI installs with one line:

```bash
curl -fsSL https://obsink.spencerjireh.com/install.sh | sh
```

Accounts on that server are invite-only: ask an existing user for a code. The rest of this
section builds the CLI from source against any server.

## Quickstart

Run a server first — locally with `docker compose up -d`, or on your own host following
[docs/self-hosting.md](docs/self-hosting.md). Then sign in and create a vault:

```bash
# First account on a fresh server needs no invite; later ones do (`obsink invite`)
cargo run -p obsink -- login --server-url https://obsink.example.com --email you@example.com

# Create a remote vault and perform the initial sync
cargo run -p obsink -- init \
  --server-url https://obsink.example.com \
  --vault-name my-notes \
  --directory ~/Obsidian/my-notes \
  --passphrase "correct horse battery staple"

# Subsequent syncs read ~/.obsink/config.toml
cargo run -p obsink -- sync
```

Attach another device to the same vault:

```bash
cargo run -p obsink -- login --server-url https://obsink.example.com --email you@example.com
cargo run -p obsink -- connect \
  --server-url https://obsink.example.com \
  --vault-id vault_xxxxxxxx \
  --directory ~/Obsidian/my-notes \
  --passphrase "correct horse battery staple"
```

Other subcommands: `vaults` (list remote vaults), `status` (pending changes for a directory),
`whoami` (account, devices, usage), `invite` (mint a code for someone else), `logout`.

The desktop app and the iOS app wrap the same core — see [docs/platforms.md](docs/platforms.md).

## Development

```bash
brew install lefthook xcodegen && lefthook install           # git hooks (once per clone)
cargo test --workspace                                       # core, CLI, mobile, server unit tests
DATABASE_URL=postgres://postgres:postgres@localhost:5433/postgres \
  OBSINK_TEST_REQUIRE_DB=1 cargo test -p obsink-server       # server integration tests (needs Postgres)
cargo clippy --workspace --all-targets -- -D warnings && cargo deny check
npm ci && npm run lint && npm run format:check && npm run typecheck -w ui && npm run build -w desktop && cargo check -p obsink-desktop
docker compose up -d && ./scripts/verify-server-deploy.sh    # contract check against the local stack
```

`main` only accepts rebase-merged pull requests with green CI; see [CONTRIBUTING.md](CONTRIBUTING.md)
for branch and commit conventions.

`RUST_LOG=obsink_core=debug` enables per-request and per-sync-plan logging (stderr; stdout stays
clean for scripted parsing).

## Documentation

- [Self-hosting](docs/self-hosting.md) — docker compose, environment, Coolify, backups
- [Architecture](docs/architecture.md) — sync engine internals, wire format, server storage
- [Platform setup](docs/platforms.md) — per-client status and setup
- [Troubleshooting](docs/troubleshooting.md) — sync and conflict scenarios

## Status

The Rust core, CLI, server, macOS desktop app and browser client are complete. iOS is in
TestFlight; the on-device Files-app check is the open item. Task tracking lives in the Plane project `OBS`;
conventions are in [AGENTS.md](AGENTS.md).

## License

MIT
