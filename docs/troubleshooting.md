# Troubleshooting

Common issues when running and syncing ObSink, and how to diagnose them.

First step for almost anything: turn on logging.

```bash
RUST_LOG=obsink_core=debug obsink sync   # logs go to stderr
```

## Authentication

**`401 unauthorized` / `unauthorized: sign in again`**
The session was revoked (Sign out on another device, account deleted) or expired (180 days), or the operator bearer does not match the server's `OBSINK_API_KEY`. Run `obsink login --server-url <url>` again; the desktop and iOS apps show "Signed out" and offer sign-in.

**`403 an invite code is required to create an account`**
The server already has accounts, so a new one needs an invite. Ask any existing user for `obsink invite` (or the operator for `obsink-server invite`) and pass it with `--invite-code` / the Invite code field. `invite code is invalid, used, or expired` means the code was spent or is older than 7 days.

**`404 vault not found`**
The vault belongs to a different account (each principal sees only its own vaults), was deleted, or you are pointed at the wrong server URL. List vaults: `obsink vaults --server-url <url>`.

**`503 email sign-in is not configured on this server`**
The operator has not set `SMTP_*`. Use Sign in with Apple on iOS, or ask the operator to configure SMTP (see [self-hosting.md](self-hosting.md)).

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
Connections time out after 15s, a transfer that stops moving for 60s fails, small metadata calls have a 30s budget, and transient failures (timeouts, connection drops) retry 3× with backoff. A persistent failure means the server is unreachable or the URL is wrong. Check `curl -fsS $OBSINK_SERVER_URL/healthz` and `curl -fsS $OBSINK_SERVER_URL/vaults -H "Authorization: Bearer $OBSINK_API_KEY"`.

**Uploads succeed but a later sync re-uploads the same file**
A `.obsink/manifest.json` that didn't persist: the diff compares content hashes against that checkpoint, never timestamps. Confirm the local manifest is being written (it lives at `<vault>/.obsink/manifest.json`) and that the directory is writable. A re-upload of identical content is harmless — the server answers `200` without writing.

**The first sync after upgrading deletes a file on the server**
Older builds checkpointed a file that had failed to download, so the next sync read it as a local deletion. Current builds hold failed paths back from the checkpoint; a vault whose last sync ran on an old build can carry one such entry over. Restore the file from the server's `_trash/` (retained 30 days) or from the other device, then sync again.

**Sync sees no remote changes that you know exist**
The client caches the last server manifest with its ETag in `<vault>/.obsink/remote-manifest.json` and asks the server "changed since?". The server answers from its own revision counter, so a stale answer means the two are out of step (a restored database backup, for example). Delete `.obsink/remote-manifest.json` and sync again.

**`507 vault storage limit reached`**
The account's per-vault byte budget (`MAX_VAULT_BYTES`, default 1 GiB) is full. The sync stops (this is fatal, not per-file). Free space by deleting files and syncing, or ask the operator to raise the limit. `obsink whoami` shows usage.

## Storage / server

**The server will not start: `OBSINK_SERVER_KEY: expected 32 bytes`**
The key must be 32 bytes, base64. Generate one with `obsink-server keygen`. If you lose the key that sealed an existing database, its metadata (emails, vault names, session names) and blobs are unreadable; restore the key from your backup.

**Blob directories remain after deleting a vault**
Vault deletion removes the rows and then the directories; if the process died in between, the daily retention pass removes directories whose vault row is gone. Run `obsink-server retention` to do it now (`docker compose exec server obsink-server retention`).

**Versions or trash growing unexpectedly**
Pruning runs in-process at startup and every `RETENTION_INTERVAL_SECS` (`_versions/`: newest 10 per file / 14 days; `_trash/`: 30 days). Check the server log for `retention pass complete`; run a pass by hand with `obsink-server retention`.

## Wire-format mismatch

**After upgrading, an existing vault won't sync or paths look wrong**
The manifest wire format is versioned (`PROTOCOL_VERSION`). A format change (e.g. the v2 HMAC-hash + encrypted-path migration) invalidates old manifests. Re-initialize the vault: delete it on the server (`DELETE /vaults/:id`, or from the app), then `init`/`connect` fresh.
