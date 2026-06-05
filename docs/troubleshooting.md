# Troubleshooting

Common issues when running and syncing ObSink, and how to diagnose them.

First step for almost anything: turn on logging.

```bash
RUST_LOG=obsink_core=debug obsink sync   # logs go to stderr
```

## Authentication

**`401 unauthorized` / `unexpected status 401`**
The API key the client sends doesn't match the Worker's `API_KEY` secret. Confirm the key, and re-set it with `wrangler secret put API_KEY` if unsure. Every request needs `Authorization: Bearer <key>`.

**`404 vault not found`**
The vault ID isn't in the Worker's `vaults` KV entry. It may have been wiped, or you're pointed at the wrong Worker URL. List vaults: `curl -H "Authorization: Bearer $KEY" $WORKER_URL/vaults`.

## Decryption / passphrase

**Files download but contents look like garbage, or `crypto error: decryption failed`**
The passphrase (and thus the derived key) doesn't match the one used to upload. The passphrase + vault ID are the only inputs to key derivation — a different passphrase produces a different key and AES-GCM authentication fails. Re-connect with the correct passphrase.

**A freshly connected device shows no files even though the vault has data**
`get_manifest` skips entries whose `encPath` it can't decrypt (wrong key). If *all* entries are skipped, the key is wrong for this vault. Verify the passphrase; check that you connected to the intended vault ID.

**`security` keychain errors on the CLI (macOS)**
Key storage uses the login Keychain. If you've overridden `HOME` (e.g. in a script), the Keychain can't be found — use `OBSINK_HOME` to relocate config instead of `HOME`, which leaves Keychain resolution intact.

## Conflicts

**Sync reports conflicts and stops**
By design — ObSink never auto-overwrites. A conflict means both sides changed with the same modification time (or otherwise diverged). Resolve each one:
- **keep local** — upload your version, overwriting remote
- **keep remote** — download the server version, overwriting local
- **keep both** — keep remote and save your version as `name.conflict.ext`

In the CLI, `sync` prompts `1/2/3` per conflict. In the desktop app, pick per-file in the Conflict Resolver, then Apply.

**A `409` during sync that becomes a "late conflict"**
Another device wrote the same file between your `prepare_sync` and `complete_sync`. ObSink catches the server's `409`, surfaces it as a fresh conflict, and asks you to resolve — nothing is lost.

## Network

**Sync hangs then errors**
Requests time out after 30s and transient failures (timeouts, connection drops) retry 3× with backoff. A persistent failure means the Worker is unreachable or the URL is wrong. Check `curl -fsS $WORKER_URL/vaults -H "Authorization: Bearer $KEY"`.

**Uploads succeed but a later sync re-uploads the same file**
Usually a clock/mtime issue or a `.obsink/manifest.json` that didn't persist. Confirm the local manifest is being written (it lives at `<vault>/.obsink/manifest.json`) and that the directory is writable.

## Storage / server

**Old files still appear in R2 after deleting a vault**
Deleting a vault's KV entries removes it logically; orphaned R2 blobs become unreachable (file reads require the vault to exist). Wrangler can't bulk-list R2 objects, so residual encrypted blobs may remain — they're inert. Delete by exact key with `wrangler r2 object delete obsink-files/<key>` if you want them gone.

**Versions or trash growing unexpectedly**
Pruning runs on Cron Triggers (`_versions/`: newest 10 per file / 14 days; `_trash/`: 30 days). If they're not running, confirm the `[triggers] crons` block is present in `wrangler.toml` and was included in the last `wrangler deploy`.

## Wire-format mismatch

**After upgrading, an existing vault won't sync or paths look wrong**
The manifest wire format is versioned (`PROTOCOL_VERSION`). A format change (e.g. the v2 HMAC-hash + encrypted-path migration) invalidates old manifests. Re-initialize the vault: wipe its KV manifest and R2 objects, then `init`/`connect` fresh.
