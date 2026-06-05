# ObSink

> Because things will go wrong.

ObSink is a free, self-hosted, end-to-end encrypted sync engine for [Obsidian](https://obsidian.md) vaults. It replaces paid sync services with a manual **Sync** button backed by your own Cloudflare account and a shared Rust core. Your files are encrypted on-device; the server only ever sees ciphertext, opaque path tokens, and keyed hashes.

## Why

- **Self-hosted** — runs on your Cloudflare account (Workers + R2 + KV). No third party holds your notes.
- **End-to-end encrypted** — AES-256-GCM content encryption with an Argon2id-derived key. The server cannot read filenames, contents, or correlate files by hash.
- **Cross-platform** — a single Rust core drives a CLI, a Tauri desktop app (macOS/Windows/Linux), and (planned) iOS + Android clients.
- **Conflict-aware** — the engine never silently overwrites. Conflicts surface for you to resolve (keep local / keep remote / keep both).

## Repository layout

| Path | What it is |
|---|---|
| `core/` | Rust sync engine: crypto, hashing, manifest diffing, conflict detection, API client |
| `cli/` | `obsink` command-line client (the reference client; great for testing) |
| `worker/` | Cloudflare Worker (TypeScript): storage, manifest, conflict gating, version/trash pruning |
| `desktop/` | Tauri v2 + React desktop app (menu-bar app on macOS) |
| `infra/terraform/` | Terraform for the R2 bucket + KV namespace |
| `scripts/` | Config rendering and live deployment-verification scripts |
| `docs/` | Deployment, self-hosting, architecture, platform, and troubleshooting guides |

## Quickstart (CLI against your own Worker)

1. **Deploy the Worker** to your Cloudflare account — see [docs/self-hosting.md](docs/self-hosting.md).
2. **Create and sync a vault:**

   ```bash
   # Create a new remote vault and do the first sync
   cargo run -p obsink -- init \
     --worker-url https://obsink-worker.<your-subdomain>.workers.dev \
     --api-key "$WORKER_API_KEY" \
     --vault-name my-notes \
     --directory ~/Obsidian/my-notes \
     --passphrase "correct horse battery staple"

   # Later, sync changes (run from anywhere; reads ~/.obsink/config.toml)
   cargo run -p obsink -- sync
   ```

3. **Connect a second device** to the same vault:

   ```bash
   cargo run -p obsink -- connect \
     --worker-url https://obsink-worker.<your-subdomain>.workers.dev \
     --api-key "$WORKER_API_KEY" \
     --vault-id vault_xxxxxxxx \
     --directory ~/Obsidian/my-notes \
     --passphrase "correct horse battery staple"
   ```

Point Obsidian at the local folder and it opens as a normal vault — no plugin required.

The desktop app wraps the same core with a menu-bar UI; see [docs/platforms.md](docs/platforms.md).

## Development

```bash
cargo test --workspace          # Rust core + CLI tests
(cd worker && npm ci && npm test && npm run typecheck)
(cd desktop && npm ci && npm run build && cargo check -p obsink-desktop)
```

Set `RUST_LOG=obsink_core=debug` to see request- and sync-level logging from the CLI (logs go to stderr).

## Documentation

- [Self-hosting guide](docs/self-hosting.md) — provision Cloudflare, deploy, set the API key
- [Deployment & CI](docs/deploy.md) — Terraform, Wrangler, GitHub Actions, verification scripts
- [Architecture](docs/architecture.md) — how the sync engine and wire format work
- [Platform setup](docs/platforms.md) — per-platform client status and setup
- [Troubleshooting](docs/troubleshooting.md) — common sync and conflict scenarios

## Status

Phases 1–2 (Rust core + CLI, Cloudflare Worker) are complete and deployed. Phase 3 (desktop) is feature-complete in code. iOS, Android, and Windows/Linux packaging are in progress — see [progress.md](progress.md).

## License

MIT
