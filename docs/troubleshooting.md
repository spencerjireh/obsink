# Troubleshooting

Common issues when running and syncing ObSink, and how to diagnose them.

First step for almost anything: turn on logging.

```bash
RUST_LOG=obsink_core=debug obsink sync   # logs go to stderr
```

## Authentication

**`401 unauthorized` / `unauthorized: sign in again`**
The session was revoked (Sign out on another device, account deleted) or expired (180 days). Run `obsink login` again (`--server-url <url>` for a server other than the default); the desktop and iOS apps show "Signed out" and offer sign-in.

**`403 an invite code is required to create an account`**
The server already has accounts, so a new one needs an invite. Ask any existing user for `obsink invite` (or the operator for `obsink-server invite`) and pass it with `--invite-code` / the Invite code field. `invite code is invalid, used, or expired` means the code was spent or is older than 7 days.

**`404 vault not found`**
The vault belongs to a different account (each account sees only its own vaults), was deleted, or you are pointed at the wrong server URL. List vaults: `obsink vaults --server-url <url>`.

**`503 email sign-in is not configured on this server`**
The operator has not set `SMTP_*`. Use Sign in with Apple on iOS, or ask the operator to configure SMTP (see [self-hosting.md](self-hosting.md)).

## Decryption / passphrase

**`passphrase does not match this account` at Unlock**
The server checks a verifier derived from the account key before it hands anything over, so a wrong passphrase is refused here and never reaches a vault. Every device of the account uses the same passphrase; `obsink passphrase` (or Settings > Change passphrase) changes it for all of them, since the key underneath stays the same.

**Files download but contents look like garbage, or `crypto error: decryption failed`**
The vault key saved on this device is not the one the account holds for that vault (the vault was deleted and created again under the same folder, or the key was saved by a build from before wire format v3). Remove the vault from this device and `Download` it again: the key is unwrapped from the account's copy.

**A freshly downloaded device shows no files even though the vault has data**
`get_manifest` skips entries whose `encPath` it can't decrypt (wrong key). If *all* entries are skipped, the key is wrong for this vault: same fix as above.

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
Connections time out after 15s, a transfer that stops moving for 60s fails, small metadata calls have a 30s budget, and transient failures (timeouts, connection drops) retry 3× with backoff. A persistent failure means the server is unreachable or the URL is wrong. Check `curl -fsS $OBSINK_SERVER_URL/healthz` and `curl -fsS $OBSINK_SERVER_URL/vaults -H "Authorization: Bearer $OBSINK_BEARER"` (a session token from `obsink login`).

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

**`Update ObSink to continue` (apps) or `400 update ObSink to continue` (CLI)**
The wire format is versioned (`PROTOCOL_VERSION`; `GET /` reports the server's). A client older than the server is refused before any sign-in code is spent. Install the current release (`curl -fsSL https://obsink.spencerjireh.com/install.sh | sh` for the CLI, the DMG for the desktop app, TestFlight for iOS).

**After upgrading, an existing vault won't sync or paths look wrong**
A format change invalidates old manifests; v3 replaced the whole schema, so a server upgraded across it starts empty (see [self-hosting.md](self-hosting.md) §7). Create the vault again with `init` and download it on the other devices.

## Browser client

**`/app` says the browser is not supported**
Folder sync needs the File System Access API, which Chrome, Edge and other Chromium browsers provide over HTTPS only. Safari and Firefox do not; use the desktop app, the CLI or the iOS app. A self-hosted copy served over plain HTTP shows the same page with a note about HTTPS.

**`Locked` after a reload**
The account key lives only in the worker's memory; nothing about the passphrase is stored in the browser. Enter the account passphrase in `Unlock`. The bearer, the device id, the vault list and the sync bookkeeping survive the reload (IndexedDB), so nothing else is lost.

**`Needs folder access`**
Chrome grants access to a picked folder per session. Press `Allow access` on the vault page and accept the prompt; the folder itself is remembered. If the folder was moved or deleted, remove the vault and add it again.

**Sync stops when the tab closes, and pauses while it is hidden or offline**
There is no background process: the worker polls while the tab is open and visible, and resumes with a poll when the tab returns. Keep the tab open for a sync to run, or use the desktop app.

**`The browser client stopped. Reload the page.`**
The worker died (out of memory, or the page was restored from the back-forward cache). Reload; the vault and its bookkeeping are intact.

**`The browser is out of storage for ObSink.`**
IndexedDB hit its quota. Free space in the browser's site settings, or clear other sites' data; the vault's files are on disk and on the server, so the local bookkeeping rebuilds on the next sync.

## Desktop smoke script

- **`WARN: tray step skipped: System Events could not read the menu`** — the tray
  step of `scripts/verify-desktop-smoke.mjs` reads the menu through the
  accessibility API. Grant the terminal you run it from Accessibility in
  System Settings > Privacy & Security > Accessibility and run it again. The
  flows before it decide pass or fail; the tray step only reports.
- **`no automation seam on port`** — the app was built without
  `--features tauri/custom-protocol` or is a release build; the script builds
  the right binary itself, so a stale `target/debug/obsink-desktop` from
  another build is the usual cause. Rerun the script.
- **`refusing http://...`** — the live and smoke scripts create and delete
  accounts, so they only run against localhost unless `OBSINK_LIVE_ALLOW_REMOTE=1`
  or `OBSINK_SMOKE_ALLOW_REMOTE=1`.
