# Platform Setup

ObSink shares one Rust core across every client. This page covers per-platform setup and current status. The live task/phase checklist lives in the Plane project `OBS` — see [AGENTS.md](../AGENTS.md) for the conventions.

| Platform | Status | Notes |
|---|---|---|
| Server (`obsink-server`) | ✅ Complete | Rust/axum + Postgres, run with docker compose; see [self-hosting.md](self-hosting.md) |
| CLI (`obsink`) | ✅ Complete | Reference client; macOS Keychain for key storage |
| macOS desktop | ✅ Verified end-to-end | Tauri v2 menu-bar app; flows covered by `live_tests::desktop_flows_live` and `account_flow_live` (`#[ignore]`) |
| iOS | 🟡 E2E-verified on simulator | Mac↔iOS sync, conflicts, deletions, launch auto-sync all pass on-simulator (`scripts/verify-ios-sim-e2e.sh`); Files/Obsidian visual check needs a device |

Windows, Linux, and Android clients are out of scope.

## CLI

The CLI is the simplest way to use ObSink and the easiest to script.

```bash
# Sign in once per machine (emailed 6-digit code), then work with vaults
obsink login [--server-url <url>] [--email <you@example.com>] [--invite-code <code>]
obsink whoami                                     # account, signed-in devices, storage usage
obsink invite [--list]                            # mint a code for someone else
obsink vaults
obsink init    --vault-name <name> --directory <path> [--passphrase <p>]
obsink connect --vault-id <id>     --directory <path> [--passphrase <p>]
obsink logout

obsink sync                       # full sync cycle; prompts to resolve conflicts
obsink watch                      # keep syncing: watch the folder, poll the server; Ctrl-C stops
obsink status [--directory <path>]
```

- `--server-url` also reads `OBSINK_SERVER_URL`; after the first command it defaults to the URL in the saved config, and before any config exists to the public server `https://obsink-api.spencerjireh.com` (a self-hosted build bakes its own by setting `OBSINK_SERVER_URL` at compile time, as the desktop app does). `--api-key` (`OBSINK_API_KEY`) supplies the operator bearer for scripts and harnesses; normal use is `login`.
- Config lives at `~/.obsink/config.toml` (server URL, vault ID, local path — **no secrets**). Set `OBSINK_HOME` to relocate it (used for per-device isolation in tests). Configs written before the server pivot (`worker_url`) still load.
- The macOS Keychain (service `obsink`) holds the encryption key (account = vault ID) and the server bearer (account = `bearer:<server url>`). `OBSINK_KEYRING_DIR=<dir>` swaps it for a directory of files (CI, harnesses).
- Each vault directory keeps `.obsink/manifest.json` (last completed sync), `.obsink/remote-manifest.json` (last server manifest + ETag, so an unchanged manifest costs a `304`) and `.obsink/hash-cache.json` (the `(mtime, size) → hash` memo).
- `obsink watch` runs the daemon (`docs/architecture.md`, "The daemon") for the configured vault: a filesystem watcher plus a debounce (750 ms quiet per path, 2 s batch window, a stat gate for files still being written), an ETag poll of the server every 5 s after activity and every 60 s when idle, exponential backoff on fatal errors (5 s to 5 min), and one line per event on stdout (`sync started`, `sync finished: n uploaded, n downloaded, n failed`, `n conflict(s) waiting; run \`obsink sync\` to resolve:` with the paths). Conflicts are never resolved by the daemon: the conflicted paths stay pending while everything else keeps syncing.
- Paths that never sync: `.obsink/`, `*.obsink-tmp`, `.obsidian/workspace.json`, `.obsidian/workspace-mobile.json`, `.trash/`, `.DS_Store`, `.git/`. Add a vault's own patterns with `ignore = ["drafts/", "*.tmp"]` in `config.toml` (`dir/`, `*.ext`, an exact `a/b.md`, or a bare name at any depth). A path already on the server when it becomes ignored is left there untouched; it just stops taking part in the diff.
- `RUST_LOG=obsink_core=debug obsink sync` prints request/sync logging to stderr.

If you omit `--passphrase`, the CLI prompts for it interactively.

## Browser

The same screens as the desktop settings window, served at `/app` on the website (`web/`), for a Mac or PC without the app installed. It syncs a local folder through the File System Access API, so it works in Chrome, Edge and other Chromium browsers over HTTPS; other browsers (and plain HTTP) get a page that says why and points at the downloads. There is no popover and no daemon: everything runs in a Web Worker while the tab is open, and stops when it closes.

```bash
wasm-pack build core-wasm --target web    # the pure core for the browser (once, and after core changes)
docker compose up -d                       # a local server on 18080 (OBSINK_PORT)
npm run dev -w web                         # http://localhost:5173/app/, API paths proxied to the server
npm run typecheck -w web && npm run test -w web && npm run build -w web
```

How it maps onto the desktop app: `web/src/backend.ts` implements the `Backend` interface over a worker (`web/src/worker/`), which holds the fetch layer (`api.ts`, same-origin paths so the web container's proxy reaches the API without CORS), the account commands, the vault list and the sync driver. The folder is picked with `showDirectoryPicker`; its handle, the bearer (`bearer:<origin>`), the vault entries and the per-vault bookkeeping (base manifest, remote-manifest cache, hash cache, activity log) live in IndexedDB (`web/src/shared/db.ts`), the browser's `~/.obsink` plus keychain, minus secrets: the passphrase is never stored and the derived keys stay in worker memory for the tab's lifetime, so a reload shows `Needs passphrase` with an Unlock field on the vault page. Chrome forgets a folder grant per session; the vault then shows `Needs folder access` with an Allow access button. The sync cycle (`worker/sync.ts`) is the native `prepare_sync`/`complete_sync` in TypeScript, with every rule that core decides in code (batching, effective conflict choices, `.conflict` copy names, the checkpoint, the diff, ignore patterns, the hash-cache fingerprint) asked of `core-wasm`, so the browser and the native clients cannot drift. `worker/driver.ts` is the browser's daemon: it polls the server every 5 s within a minute of activity and every 60 s when idle, rescans the folder through the hash cache on each poll (there is no file watcher in a browser), backs off exponentially after a fatal error, and defers every conflict until the user answers on the vault page. Per-vault activity keeps the newest 200 events.

`scripts/verify-web-e2e.mjs` drives two headless Chromium contexts against a running dev server and local stack as two devices: sign-in with the dev code, create and connect a vault (an OPFS folder stands in for the picked one), propagation both ways, a conflict resolved with Keep both. `npx playwright install chromium` once, then `node scripts/verify-web-e2e.mjs`.

## macOS desktop

A Tauri v2 menu-bar app (`desktop/`). The React screens live in the shared `ui/` npm workspace and reach the platform through the `Backend` interface (`ui/src/backend.tsx`); `desktop/src/backend.ts` implements it with Tauri commands and events, and the browser client implements it again over a worker.

```bash
npm ci                   # at the repo root: installs the ui and desktop workspaces
cd desktop
npm run tauri dev        # run against your server (docker compose up -d for a local one)
npm run tauri build      # produce a .app/.dmg (ad hoc signed; the release workflow signs and notarizes)
```

Releases ship a universal DMG (Apple Silicon and Intel), Developer ID signed and notarized by `release.yml` on a `v*.*.*` tag, linked from the website's macOS section together with its SHA-256; the same tag publishes the universal CLI tarball that `install.sh` fetches.

App, dock, and menu-bar icons are generated from `design/icon.svg` and
`design/tray.svg` by `scripts/gen-icons.sh` (see `DESIGN.md`); rerun it after
editing either SVG and commit the rasters.

The app lives in the menu bar (no Dock icon). The server it talks to is baked in at build time from `OBSINK_SERVER_URL` (fallback `https://obsink-api.spencerjireh.com`; the old `obsink.spencerjireh.com` host is an alias of it in every client, and the website there still proxies the API); there is no URL field in the UI. Setting `OBSINK_SERVER_URL` at launch overrides it, which is how the live tests and the smoke script (`scripts/verify-desktop-smoke.mjs`) point a build at a local server. Self-hosters build the clients with their own URL.

Every vault that can sync (on this server, key in the Keychain, signed in) gets a **daemon** at launch and whenever that set changes (sign-in, add, remove, delete, sign-out): the same driver as `obsink watch`, so an edit in Obsidian is on the server a couple of seconds later and a change from another device lands within the poll interval. `Sync now` and conflict resolutions are commands to the vault's daemon, so no two cycles overlap; the daemon's events feed the same state the popover reads (`Syncing…` while a cycle runs, `n conflicts` for what it left for you, the activity log). A vault without a daemon (no key yet) still syncs through the manual path. Extra ignore patterns go in `~/.obsink/app.json` as `"ignore": ["drafts/"]` on the vault entry.

Left-clicking the tray icon toggles a **popover** under it: one row per vault with a state dot and the shared state text (`Up to date`, `n to upload`, `n to download`, `n conflicts`, `Syncing…`, `Offline`, `Session expired`, `On another server`, `Needs passphrase`), an **Open folder** button per row, a **Recent** list of the last activity events, **Sync now** (syncs every vault in turn, skipping one that is waiting for a conflict decision) and **Settings**. Clicking a row opens the settings window at that vault; the popover hides when it loses focus. The tray menu (right-click) has **Sync now / Open settings / Quit ObSink**.

The **settings window** (closing hides it) has three tabs. **Vaults**: a list on the left; the page shows the state line with the last sync time and the usage against the per-vault cap (`412 MiB of 1 GiB`), the folder path with **Open folder**, **Sync now**, notices (progress, stale remote changes, failed files), conflicts with a side-by-side **This device** / **Other device** preview (the only place conflicts are resolved), and **Manage vault**: **Remove from this device** (drops the config entry and keychain key; the folder stays and reconnecting needs the passphrase) and **Delete vault on server** behind a typed vault-name confirmation. **Add vault** is a stepped flow: **Sign in** (only when signed out; email code, the invite field appears only when the server needs one or refused a sign-up without one), **Choose vault** (the account's vaults are listed on entry; **Create** when there are none), **Folder**, **Passphrase**, then a card with the folder path and **Open folder**. **Account**: the email and storage usage, **Invite someone**, **Devices** (every session with a **This device** tag; **Sign out** on another row revokes that session), **Invites** (each code with Active / Used / Expired and a **Copy** button), and **Delete account** behind a typed confirmation (the email, or `delete`), which removes the account and all its vaults on the server and forgets the bearer and every vault entry on this Mac (vault folders stay). **Activity**: the per-vault log with a vault filter.

A vault entry whose stored URL differs from the build's server shows **On another server** and offers only **Remove from this device**. `~/.obsink/app.json` holds vault URLs/paths only; bearers live in the macOS Keychain (`bearer:<server url>`; `OBSINK_KEYRING_DIR` file fallback for tests); the activity log is `~/.obsink/activity/<vault id>.json` (last 200 events per vault, plus the last completed sync time), written after every sync and dropped with the vault. Every command reports a typed error (`kind`, `message`, `status`); a 401 forgets the bearer, the vault shows **Session expired**, and the page shows **Session expired. Sign in again.** with a **Sign in** button that opens the Account tab. Colours follow the system appearance (light and dark); labels and tokens are in `DESIGN.md`. The `#[ignore]`d `live_tests::desktop_flows_live` and `account_flow_live` cover sync, conflicts, vault states (including the foreign-server case), the activity log, sign-in, capabilities, invite gating and listing, device revocation, vault removal and deletion, account deletion, and sign-out against a server started with `AUTH_DEV_RETURN_CODE=1` (the local compose stack); they set `OBSINK_SERVER_URL` themselves and share the process environment, so run them with `--test-threads=1`.

Point Obsidian at the vault's local folder — it opens as a normal vault with no plugin.

### End-to-end verification

The desktop command layer (the exact Tauri commands the UI invokes) is covered by ignored live integration tests that run against a server. `scripts/verify-desktop-live.sh` runs them against the local compose stack (`OBSINK_PORT=18080 docker compose up -d --wait` first; `OBSINK_SERVER_URL` and `OBSINK_API_KEY` override the defaults, and a server that is not localhost is refused unless `OBSINK_LIVE_ALLOW_REMOTE=1`). Under the hood:

```bash
OBSINK_TEST_SERVER_URL=http://localhost:18080 \
OBSINK_TEST_API_KEY=dev-operator-key OBSINK_TEST_PASSPHRASE=... \
cargo test -p obsink-desktop live_tests -- --ignored --nocapture --test-threads=1
```

Both tests sandbox `HOME`, so they run one at a time. `account_flow_live` signs the same address in twice and waits out the server's 60 s email cooldown, so expect about two minutes.

`desktop_flows_live` seeds the operator bearer into the file keyring the way a sign-in would; `account_flow_live` signs in with the email code and, on a server that already has accounts, mints the invite it needs with `OBSINK_TEST_API_KEY`. CI runs only the crate's unit tests (`cargo test -p obsink-desktop`); the live tests stay a local step.

The windows and the tray are covered by `node scripts/verify-desktop-smoke.mjs`, a full-flow smoke with assertions against the same stack. Debug builds carry an automation seam (`desktop/src-tauri/src/automation.rs`, compiled only under `debug_assertions`): with `OBSINK_AUTOMATION_PORT` set, the app listens on `127.0.0.1:<port>` for one JSON line per connection (`ping`, `eval` a script body inside the `settings` or `popover` web view and get its value back, `show`, `hide`, `bounds`). The script builds the app with the frontend embedded (`cargo build -p obsink-desktop --features tauri/custom-protocol`), launches it with a sandboxed `HOME`, the file keyring and `OBSINK_SERVER_URL`, signs in and creates a vault through the same commands the UI calls, then drives the real settings window and popover by `data-testid` (the names are listed in `DESIGN.md`): Sync now, the activity log, a conflict against the CLI as the second device, Keep both, vault deletion. Every window stays hidden (the web views run either way) and no mouse or keyboard input is ever posted, so the run does not interfere with other work on the Mac. `OBSINK_SMOKE_SHOTS=<dir>` shows each window for a moment, without activating the app or taking focus, to capture the conflict page and the popover. The tray menu's items are read through System Events, which needs Accessibility permission for the terminal (without it that step is reported and the run still passes). `OBSINK_SMOKE_KEEP=1` leaves the app and sandbox running for a look. Release builds contain none of the seam.

It verifies vault create/connect + passphrase validation, the full sync cycle with cross-device propagation, all three conflict resolutions (KeepLocal / KeepRemote / KeepBoth) via a three-way conflict (another device overwrites the server copy while the local file is edited), stale-vault detection (the banner's data source), and multi-vault switching. It uses a sandboxed `HOME` and the file-backed keyring (`OBSINK_KEYRING_DIR`) so it never prompts the macOS keychain or pollutes `~/.obsink/app.json`.

## iOS

The `mobile/` crate exposes the core to Swift via UniFFI. Build everything (staticlibs, bindings, XCFramework, Xcode project) with:

```bash
# One-time prereqs: Xcode, rustup + iOS targets, and xcodegen
rustup target add aarch64-apple-ios aarch64-apple-ios-sim
brew install xcodegen
./scripts/build-ios.sh        # self-resolves rustup's cargo if a non-rustup one is on PATH
```

Then build/run on a simulator:

```bash
xcodebuild -project ios/ObSink.xcodeproj -scheme ObSink -sdk iphonesimulator \
  -destination 'platform=iOS Simulator,name=iPhone 17 Pro' CODE_SIGNING_ALLOWED=NO build
```

The SwiftUI app has two tabs. **Home** shows one card per vault: the shared state text (`Up to date`, `n to upload`, `n to download`, `n conflicts`, `Syncing…`, `Error: …`, `On another server`, `Needs passphrase`) with a state dot and `Last synced`, the usage against the per-vault cap (`412 MiB of 1 GiB`), the last result on the active vault, the stale banner, `Resolve n conflicts` (a pushed Conflicts screen with the side-by-side preview and `Apply resolutions`), a passphrase field until the key is stored, a full-width **Sync now**, and **Manage** (a vault screen: **Remove from this device** removes the cache directory, item database, File Provider location and key while the server copy stays; **Delete vault on server** behind a typed vault-name confirmation). After the first vault a one-time **Open in Obsidian** card gives the Files path. **Add vault** is a stepped sheet: sign in (only when signed out; Sign in with Apple or an email code, `GET /` decides which methods show and whether the invite code field is needed), choose (`Create` a named vault or `Connect` to one of the account's vaults, listed on entry), passphrase. **Settings** holds the account: signed in as, usage, **Devices** (every session with a **This device** tag; **Sign out** on another row revokes it), **Invites** (each code with Active / Used / Expired, **Invite someone**, **Copy**), **Sign out**, **Delete account** behind a typed confirmation (the email, or `delete`; removes the account and its vaults on the server and every vault entry on the device), or **Sign in** when signed out, and the version. The server is baked into the build from `OBSINK_SERVER_URL` (XcodeGen writes it to `Info.plist` as `ObSinkServerURL`; empty means `https://obsink-api.spencerjireh.com`; `obsink.spencerjireh.com` is an alias of it), never typed; `OBSINK_UITEST_SERVER_URL` in the launch environment overrides it for the UI tests. A vault entry on another server is read-only (`On another server`, remove only). Labels, accent colour and section symbols follow `DESIGN.md`. The app talks to the Rust core through the generated `VaultClient` and `auth*` functions. Errors cross the FFI typed (`MobileError.Unauthorized` / `.Network` / `.Server(status, message)`); a 401 forgets the bearer, the card shows `Session expired` and Settings shows **Session expired. Sign in again.** with a **Sign in** button that opens the sign-in sheet. The bearer lives in the iOS Keychain under `bearer:<server url>`; vault entries in the App Group UserDefaults carry no secrets (entries written before the server pivot under `workerURL` still decode). Sign in with Apple needs the `com.apple.developer.applesignin` entitlement (enabled on the App ID by automatic signing). Files sync into the App Group container (`group.com.obsink.shared`), one directory per vault (`Vault/<vault id>/`), and each vault is registered as its own File Provider domain (identifier = vault id, display name = vault name) so Obsidian/Files see one location per vault. The extension is **DB-backed** (`group.com.obsink.shared/items-<vault id>.sqlite` via GRDB, one database per vault): stable UUID identifiers, real `enumerateChanges` deltas (monotonic `rowVersion` + `isDeleted` tombstones), paged enumeration (500 rows per call; the working set lists every live item), folders that can be renamed, moved and deleted in Files (the store cascades the path rewrite or the tombstones to everything under them), a content version derived from (size, mtime) so a rename never re-fetches bytes, and the host app reconciles the DB + signals that vault's enumerator after each sync (also after a conflict-paused sync, whose downloads are already on disk). The extension declares `NSExtensionFileProviderSupportsPickingFolders` so Obsidian's folder picker can select the vault. If the database cannot be opened the extension reports `cannotSynchronize` instead of crashing. `fetchContents` hands the system a staged copy, never the vault file itself: the replicated File Provider consumes the file it is given. A first launch after the per-vault change moves the old single `Vault/` and `obsink.sqlite` under the active vault. When Sign in with Apple returns a token without an email claim, the app asks for a one-time code for the credential's email before the server links the two. The derived key is stored in the iOS Keychain (per vault), so the passphrase isn't re-entered each launch; keys and the bearer are saved `AfterFirstUnlock` so the background refresh can read them while the device is locked (items from earlier builds are re-saved once at launch).

**Automatic sync (OBS-107).** On launch and on every return to the foreground the app runs the stale check and then `AutoSyncPolicy` over each vault: a vault syncs, one at a time, when it has File Provider writes waiting, when the server is ahead, or when it has not synced for 15 minutes; a vault that is syncing, keyless, on another server or holding conflicts is skipped (conflicts wait for the user). A `BGAppRefreshTask` (`com.obsink.ios.refresh`, `UIBackgroundModes: fetch`) runs the same routine when iOS grants a background window, rescheduling itself each time; there is no toggle, and Settings > About shows whether Background App Refresh is on for the app. A sync in flight cannot be cancelled by the task's expiration handler, so the routine stops before its next vault and reports the task as incomplete. The simulator cannot exercise it: its `BGTaskScheduler` refuses the submission when the app goes to the background (`BGTaskSchedulerErrorDomain Code=1`, logged as `background refresh not scheduled`), so `[[BGTaskScheduler sharedScheduler] _simulateLaunchForTaskWithIdentifier:@"com.obsink.ios.refresh"]` (run from lldb attached to the running app: `lldb --batch -p <pid> -o 'expression -l objc -O -- ...' -o 'process detach'`) answers "No task request ... has been scheduled" and the handler never runs. On a device, run that expression with the app paused in Xcode after it has been backgrounded once; the handler logs `ObSink: background refresh started` and `finished success=1`.

> Slices A–E of the P4 plan are complete (21/28 items; see `docs/p4-plan.md`). The app + embedded FileProviderExt build, unit-test green (57 tests on the simulator), and install/launch cleanly.

Run the on-simulator integration tests (the live-sync test reads `OBSINK_TEST_*` env vars — server URL, API key, vault ID, and passphrase):

```bash
xcodebuild test -project ios/ObSink.xcodeproj -scheme ObSink -sdk iphonesimulator \
  -destination 'platform=iOS Simulator,name=iPhone 17 Pro' CODE_SIGNING_ALLOWED=NO
```

### Simulator E2E harness (OBS-29–34)

The Mac↔iOS scenarios are automated end to end on a simulator against a running server (start one with `docker compose up -d`):

```bash
# Uses .env.deploy (OBSINK_SERVER_URL, OBSINK_API_KEY, DEVELOPMENT_TEAM) and a throwaway vault.
OBSINK_SIM_NAME=obsink-e2e ./scripts/verify-ios-sim-e2e.sh
```

The CLI plays "device A" with the operator bearer; the same bearer is seeded into the app's Keychain (`OBSINK_UITEST_BEARER`) so both devices share one principal, and the server URL is passed at launch (`OBSINK_UITEST_SERVER_URL`) so the phases do not depend on the URL baked into the build. XCUITest phases
(`ios/UITests/SyncE2ETests.swift`) drive the app as "device B"; on-disk state is
verified through the app-group container. Verified green: Add Vault → Connect,
Mac→iOS and iOS→Mac propagation, deletions both ways, the launch auto-sync,
all three conflict resolutions, a realistic vault (`.obsidian/` config,
nested folders, binary attachment) byte-identical on both sides, and
Remove from this device (cache directory and item database gone from the
app-group container).

Known simulator limitation: the iOS 26 simulator's `fileproviderd` never
instantiates third-party replicated File Provider extensions (libxpc assertion;
the Files listing hangs at LOADING), so the harness reports the Files-app phase
as WARN. Simulator builds force-enable the domain via
`NSFileProviderDomain.testingModes` plus the Debug-only
`com.apple.developer.fileprovider.testing-mode` entitlement; the extension also
sets `ENABLE_DEBUG_DYLIB=NO` (the debug-dylib stub prevents principal-class
loading).

### TestFlight release

```bash
# .env.deploy: DEVELOPMENT_TEAM, ASC_KEY_ID, ASC_ISSUER_ID, ASC_KEY_PATH (App Store
# Connect team API key, App Manager role — drives signing and upload).
./scripts/release-ios.sh                 # archive + sign + upload; build = commit count
set -a; . ./.env.deploy; set +a
uv run scripts/testflight.py status      # processing state per build
uv run scripts/testflight.py distribute --group "Internal Testers" --encryption exempt
```

`release-ios.sh` uses automatic signing through the API key
(`-allowProvisioningUpdates`), so no Xcode account login is needed. Each build
is a unique, monotonic number (the commit count) because App Store Connect
rejects re-uploads. `distribute` waits for processing, records the
export-compliance answer, and attaches the build to the group; internal groups
with automatic build access need no assignment (and no Beta App Review).
Testers in an internal group must be App Store Connect users on the team.
Privacy manifests (`PrivacyInfo.xcprivacy`, app + extension) declare the
required-reason APIs in use (UserDefaults, file timestamps).

Remaining (device-only): the visual Files-app/Obsidian checkpoint (OBS-19),
which runs from the TestFlight install against a self-hosted server. See
`docs/p4-plan.md` and spec.md §11 for the File Provider design.
