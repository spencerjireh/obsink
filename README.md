# ObSink

Self-hosted, end-to-end encrypted sync for [Obsidian](https://obsidian.md) vaults. A shared Rust
core builds and diffs manifests, encrypts locally, and talks to a Cloudflare Worker backed by R2
(blobs) and KV (manifests). The server stores ciphertext, path tokens, and keyed hashes — it holds
no key material and performs no decryption.

Sync is explicit: clients push and pull on demand rather than watching the filesystem. Obsidian
opens the synced directory as a normal vault; no plugin is involved.

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
  instead of clobbering. The engine never auto-resolves; resolution (keep local / remote / both) is
  a client decision.
- **Retention.** Overwrites and deletes are copied to `_versions/` and `_trash/`; Cron Triggers
  prune to 10 versions or 14 days, and 30 days respectively.

Wire protocol is `PROTOCOL_VERSION = 2`. Full manifest schema, key-derivation table, and rationale
in [docs/architecture.md](docs/architecture.md).

## Sync cycle

`prepare_sync` → resolve conflicts → `complete_sync`:

1. Walk the vault directory into a working manifest (keyed hash, size, mtime); the last-sync
   `.obsink/manifest.json` identifies local deletions.
2. Fetch the remote manifest and re-key it by real path via `encPath`.
3. `diff_manifests` splits into upload / download / conflict (mtime decides; equal mtime with
   differing hash is a conflict).
4. Downloads apply immediately; uploads and conflicts return as a `SyncPlan`.
5. The client resolves conflicts, then `complete_sync` uploads with parent-hash gating, handles late
   409s, and persists the new manifest.

## Worker API

Bearer-token auth on every route.

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/vaults` | List vault summaries |
| `POST` | `/vaults` | Create a vault |
| `GET` | `/vaults/:id/manifest` | Fetch the token-keyed manifest |
| `GET` | `/vaults/:id/files/:token` | Download a blob |
| `PUT` | `/vaults/:id/files/:token` | Upload (requires `X-Parent-Hash`) |
| `DELETE` | `/vaults/:id/files/:token` | Delete (requires `X-Parent-Hash`) |
| `POST` | `/vaults/:id/batch` | Batched manifest operations |

Storage keys are `<vaultId>/<token>`, with history under `_versions/<vaultId>/<token>/<unix>` and
`_trash/<vaultId>/<token>/<unix>`.

`ApiClient` enforces a 30s per-request timeout and retries transient failures up to 3× with
exponential backoff; HTTP status errors surface immediately as typed `ApiError`s.

## Repository layout

| Path | Contents |
|---|---|
| `core/` | Sync engine: `crypto`, `hasher`, `manifest`, `sync_engine`, `api_client`, `types` |
| `cli/` | `obsink` CLI — the reference client |
| `worker/` | Cloudflare Worker (TypeScript): auth, routing, conflict gating, pruning |
| `desktop/` | Tauri v2 + React app (menu-bar on macOS) |
| `ios/`, `mobile/` | SwiftUI app + File Provider extension over the shared core |
| `infra/terraform/` | R2 bucket and KV namespace |
| `scripts/` | Config rendering and live deployment verification |
| `docs/` | Deployment, self-hosting, architecture, platform, troubleshooting |

## Quickstart

Deploy the Worker to your own Cloudflare account first — see
[docs/self-hosting.md](docs/self-hosting.md). Then:

```bash
# Create a remote vault and perform the initial sync
cargo run -p obsink -- init \
  --worker-url https://obsink-worker.<your-subdomain>.workers.dev \
  --api-key "$WORKER_API_KEY" \
  --vault-name my-notes \
  --directory ~/Obsidian/my-notes \
  --passphrase "correct horse battery staple"

# Subsequent syncs read ~/.obsink/config.toml
cargo run -p obsink -- sync
```

Attach another device to the same vault:

```bash
cargo run -p obsink -- connect \
  --worker-url https://obsink-worker.<your-subdomain>.workers.dev \
  --api-key "$WORKER_API_KEY" \
  --vault-id vault_xxxxxxxx \
  --directory ~/Obsidian/my-notes \
  --passphrase "correct horse battery staple"
```

Other subcommands: `vaults` (list remote vaults), `status` (pending changes for a directory).

The desktop app wraps the same core behind a menu-bar UI — see
[docs/platforms.md](docs/platforms.md).

## Development

```bash
cargo test --workspace                                       # core + CLI
(cd worker && npm ci && npm test && npm run typecheck)       # Vitest over in-memory KV/R2 fakes
(cd desktop && npm ci && npm run build && cargo check -p obsink-desktop)
./scripts/verify-*.sh                                        # live checks against a deployed Worker
```

`RUST_LOG=obsink_core=debug` enables per-request and per-sync-plan logging (stderr; stdout stays
clean for scripted parsing).

## Documentation

- [Self-hosting](docs/self-hosting.md) — provision Cloudflare, deploy, set the API key
- [Deployment & CI](docs/deploy.md) — Terraform, Wrangler, GitHub Actions, verification scripts
- [Architecture](docs/architecture.md) — sync engine internals and wire format
- [Platform setup](docs/platforms.md) — per-client status and setup
- [Troubleshooting](docs/troubleshooting.md) — sync and conflict scenarios

## Status

Phases 1–2 (Rust core, CLI, Worker) are complete and deployed. Phase 3 (desktop) is
feature-complete in code. iOS is in progress; Windows/Linux packaging is planned. Task tracking
lives in the Plane project `OBS`; conventions are in [AGENTS.md](AGENTS.md).

## License

MIT
