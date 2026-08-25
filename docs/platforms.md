# Platform Setup

ObSink shares one Rust core across every client. This page covers per-platform setup and current status. The live task/phase checklist lives in the Plane project `OBS` — see [AGENTS.md](../AGENTS.md) for the conventions.

| Platform | Status | Notes |
|---|---|---|
| CLI (`obsink`) | ✅ Complete | Reference client; macOS Keychain for key storage |
| macOS desktop | ✅ Verified end-to-end | Tauri v2 menu-bar app; flows covered by `live_tests::desktop_flows_live` (`#[ignore]`) |
| Linux desktop | 🟡 Builds in CI | `.deb` bundled in CI; keychain backend (libsecret) not yet wired |
| Windows desktop | ⬜ Planned | Needs DPAPI/Credential Manager keychain |
| iOS | 🟡 E2E-verified on simulator | Mac↔iOS sync, conflicts, deletions, stale banner all pass on-simulator (`scripts/verify-ios-sim-e2e.sh`); Files/Obsidian visual check + TestFlight need a device |

## CLI

The CLI is the simplest way to use ObSink and the easiest to script.

```bash
# ObSink Cloud (default backend): sign in once per machine, then work with vaults
obsink login [--email <you@example.com>]          # emailed 6-digit code
obsink whoami                                     # account + signed-in devices
obsink vaults
obsink init    --vault-name <name> --directory <path> [--passphrase <p>]
obsink connect --vault-id <id>     --directory <path> [--passphrase <p>]
obsink logout

# Self-hosted Worker: pass the URL and (once) the API key; both are remembered
obsink init    --worker-url <url> --api-key <key> --vault-name <name> --directory <path>
obsink connect --worker-url <url> --api-key <key> --vault-id <id>     --directory <path>

obsink sync                       # full sync cycle; prompts to resolve conflicts
obsink status [--directory <path>]
```

- `--worker-url` / `--api-key` also read `OBSINK_WORKER_URL` / `OBSINK_API_KEY`; `OBSINK_HOSTED_URL` points "ObSink Cloud" at another Worker (harnesses, staging).
- Config lives at `~/.obsink/config.toml` (Worker URL, vault ID, local path — **no secrets**). Set `OBSINK_HOME` to relocate it (used for per-device isolation in tests). A pre-accounts config containing `api_key` is migrated into the keychain on first use.
- The macOS Keychain (service `obsink`) holds the encryption key (account = vault ID) and the server bearer (account = `bearer:<worker url>`). `OBSINK_KEYRING_DIR=<dir>` swaps it for a directory of files (Linux, CI).
- `RUST_LOG=obsink_core=debug obsink sync` prints request/sync logging to stderr.

If you omit `--passphrase`, the CLI prompts for it interactively.

## macOS desktop

A Tauri v2 + React menu-bar app (`desktop/`).

```bash
cd desktop
npm ci
npm run tauri dev        # run against ObSink Cloud or your deployed Worker
npm run tauri build      # produce a .app/.dmg
```

Vault Setup offers **ObSink Cloud** (email + code sign-in; the account line shows the signed-in email and a Sign out button) or **Self-hosted** (Worker URL + API key, remembered per URL). Connect lists the account's vaults in a dropdown. `~/.obsink/app.json` holds vault URLs/paths only; bearers live in the macOS Keychain (`bearer:<worker url>`; `OBSINK_KEYRING_DIR` file fallback for tests). The `#[ignore]`d `live_tests::account_flow_live` covers the Cloud path against a `wrangler dev` Worker started with `AUTH_DEV_RETURN_CODE=1`.

Behavior:
- Tray icon with **Sync Now / Show ObSink / Quit**; left-click surfaces the window.
- Closing the window **hides to the tray** so background state persists (menu-bar app).
- Configure a vault in the UI (Worker URL, API key, create/connect, passphrase, local folder), then Sync. Conflicts open a side-by-side preview before you choose a winner.

Point Obsidian at the vault's local folder — it opens as a normal vault with no plugin.

### End-to-end verification

The desktop command layer (the exact Tauri commands the UI invokes) is covered by an ignored live integration test that runs against a deployed Worker:

```bash
OBSINK_TEST_WORKER_URL=https://obsink-worker.<subdomain>.workers.dev \
OBSINK_TEST_API_KEY=... OBSINK_TEST_PASSPHRASE=... \
cargo test -p obsink-desktop live_tests -- --ignored --nocapture
```

It verifies vault create/connect + passphrase validation, the full sync cycle with cross-device propagation, all three conflict resolutions (KeepLocal / KeepRemote / KeepBoth) via an equal-mtime conflict, stale-vault detection (the banner's data source), and multi-vault switching. It uses a sandboxed `HOME` and the file-backed keyring (`OBSINK_KEYRING_DIR`) so it never prompts the macOS keychain or pollutes `~/.obsink/app.json`.

## Linux desktop

CI builds a `.deb` bundle on every push (see `.github/workflows/ci.yml`, job `desktop bundle (linux .deb)`). To build locally:

```bash
sudo apt-get install -y libwebkit2gtk-4.1-dev libgtk-3-dev libappindicator3-dev librsvg2-dev patchelf
cd desktop && npm ci && npm run tauri build -- --bundles deb
```

> Known gap: key storage currently shells out to the macOS `security` tool. A Linux secret-store backend (libsecret/kwallet) is still to be implemented, so the Linux build compiles and bundles but key storage is not yet functional there.

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

The SwiftUI app (sync button, status, Add Vault with an ObSink Cloud / Self-hosted choice — Sign in with Apple or email code for Cloud, URL + API key for self-hosted — multi-vault picker, conflict resolution with side-by-side preview) talks to the Rust core through the generated `VaultClient` and `auth*` functions. The bearer lives in the iOS Keychain under `bearer:<worker url>`; vault entries in the App Group UserDefaults carry no secrets (a legacy `apiKey` is migrated on launch). Sign in with Apple needs the `com.apple.developer.applesignin` entitlement (enabled on the App ID by automatic signing). Files sync into the App Group container (`group.com.obsink.shared`), which the File Provider extension exposes to Obsidian/Files. The extension is **DB-backed** (`group.com.obsink.shared/obsink.sqlite` via GRDB): stable UUID identifiers, real `enumerateChanges` deltas (monotonic `rowVersion` + `isDeleted` tombstones), and the host app reconciles the DB + signals the enumerator after each sync. The derived key is stored in the iOS Keychain (per vault), so the passphrase isn't re-entered each launch.

> Slices A–E of the P4 plan are complete (21/28 items; see `docs/p4-plan.md`). The app + embedded FileProviderExt build, unit-test green (17 tests on the simulator), and install/launch cleanly.

Run the on-simulator integration tests (the live-sync test reads `OBSINK_TEST_*` env vars — worker URL, API key, vault ID, and passphrase):

```bash
xcodebuild test -project ios/ObSink.xcodeproj -scheme ObSink -sdk iphonesimulator \
  -destination 'platform=iOS Simulator,name=iPhone 17 Pro' CODE_SIGNING_ALLOWED=NO
```

### Simulator E2E harness (OBS-29–34)

The Mac↔iOS scenarios are automated end to end on a simulator:

```bash
# Uses .env (WORKER_URL, WORKER_API_KEY, DEVELOPMENT_TEAM) and a throwaway vault.
OBSINK_SIM_NAME=obsink-e2e ./scripts/verify-ios-sim-e2e.sh
```

The CLI plays "device A" against the live Worker; XCUITest phases
(`ios/UITests/SyncE2ETests.swift`) drive the app as "device B"; on-disk state is
verified through the app-group container. Verified green: Add Vault → Connect,
Mac→iOS and iOS→Mac propagation, deletions both ways, the stale-vault banner,
all three conflict resolutions, and a realistic vault (`.obsidian/` config,
nested folders, binary attachment) byte-identical on both sides.

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
# .env: DEVELOPMENT_TEAM, ASC_KEY_ID, ASC_ISSUER_ID, ASC_KEY_PATH (App Store
# Connect team API key, App Manager role — drives signing and upload).
./scripts/release-ios.sh                 # archive + sign + upload; build = commit count
set -a; . ./.env; set +a
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
which runs from the TestFlight install. See `docs/p4-plan.md` and spec.md §11
for the File Provider design.

## Windows (planned)

Not started. Windows needs a DPAPI/Credential Manager key store. Tracked in the Plane project `OBS` (phase P5).
