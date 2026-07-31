# Platform Setup

ObSink shares one Rust core across every client. This page covers per-platform setup and current status. The live task/phase checklist lives in the Plane project `OBS` — see [AGENTS.md](../AGENTS.md) for the conventions.

| Platform | Status | Notes |
|---|---|---|
| CLI (`obsink`) | ✅ Complete | Reference client; macOS Keychain for key storage |
| macOS desktop | ✅ Verified end-to-end | Tauri v2 menu-bar app; flows covered by `live_tests::desktop_flows_live` (`#[ignore]`) |
| Linux desktop | 🟡 Builds in CI | `.deb` bundled in CI; keychain backend (libsecret) not yet wired |
| Windows desktop | ⬜ Planned | Needs DPAPI/Credential Manager keychain |
| iOS | 🟡 App + File Provider code-complete | DB-backed replicated FP (UUID IDs, change deltas), Keychain, multi-vault, conflict preview; needs E2E + TestFlight |
| Android | ⬜ Planned | Tauri mobile (React Native + Rust fallback) |

## CLI

The CLI is the simplest way to use ObSink and the easiest to script.

```bash
obsink init    --worker-url <url> --api-key <key> --vault-name <name> --directory <path> [--passphrase <p>]
obsink connect --worker-url <url> --api-key <key> --vault-id <id>     --directory <path> [--passphrase <p>]
obsink sync                       # full sync cycle; prompts to resolve conflicts
obsink status [--directory <path>]
```

- Config lives at `~/.obsink/config.toml`. Set `OBSINK_HOME` to relocate it (used for per-device isolation in tests).
- The encryption key is stored in the macOS Keychain (service `obsink`, account = vault ID).
- `RUST_LOG=obsink_core=debug obsink sync` prints request/sync logging to stderr.

If you omit `--passphrase`, the CLI prompts for it interactively.

## macOS desktop

A Tauri v2 + React menu-bar app (`desktop/`).

```bash
cd desktop
npm ci
npm run tauri dev        # run against your deployed Worker
npm run tauri build      # produce a .app/.dmg
```

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

The SwiftUI app (sync button, status, vault setup, multi-vault picker, conflict resolution with side-by-side preview) talks to the Rust core through the generated `VaultClient`. Files sync into the App Group container (`group.com.obsink.shared`), which the File Provider extension exposes to Obsidian/Files. The extension is **DB-backed** (`group.com.obsink.shared/obsink.sqlite` via GRDB): stable UUID identifiers, real `enumerateChanges` deltas (monotonic `rowVersion` + `isDeleted` tombstones), and the host app reconciles the DB + signals the enumerator after each sync. The derived key is stored in the iOS Keychain (per vault), so the passphrase isn't re-entered each launch.

> Slices A–E of the P4 plan are complete (21/28 items; see `docs/p4-plan.md`). The app + embedded FileProviderExt build, unit-test green (17 tests on the simulator), and install/launch cleanly.

Run the on-simulator integration tests (the live-sync test reads `OBSINK_TEST_*` env vars — worker URL, API key, vault ID, and passphrase):

```bash
xcodebuild test -project ios/ObSink.xcodeproj -scheme ObSink -sdk iphonesimulator \
  -destination 'platform=iOS Simulator,name=iPhone 17 Pro' CODE_SIGNING_ALLOWED=NO
```

Remaining (Slice F — manual validation + signing): a visual check that synced files appear in the iOS Files app / Obsidian (OBS-19), Mac↔iOS end-to-end scenarios (OBS-29–34), and code-signing + TestFlight — set `DEVELOPMENT_TEAM` in `ios/project.yml` and upload via Xcode (the project uses automatic signing). See `docs/p4-plan.md` and spec.md §11 for the File Provider design.

## Android & Windows (planned)

Not started. Android targets Tauri mobile (with a React Native + Rust fallback); Windows needs a DPAPI/Credential Manager key store. Tracked in the Plane project `OBS` (phases P5/P6).
