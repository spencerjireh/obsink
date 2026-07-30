# Platform Setup

ObSink shares one Rust core across every client. This page covers per-platform setup and current status. The live task/phase checklist lives in the Plane project `OBS` — see [AGENTS.md](../AGENTS.md) for the conventions.

| Platform | Status | Notes |
|---|---|---|
| CLI (`obsink`) | ✅ Complete | Reference client; macOS Keychain for key storage |
| macOS desktop | ✅ Feature-complete in code | Tauri v2 menu-bar app |
| Linux desktop | 🟡 Builds in CI | `.deb` bundled in CI; keychain backend (libsecret) not yet wired |
| Windows desktop | ⬜ Planned | Needs DPAPI/Credential Manager keychain |
| iOS | 🟡 App builds + runs on simulator | SwiftUI app syncs via Rust core (verified); File Provider is a scaffold; TestFlight needs signing |
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
brew install xcodegen          # one-time
./scripts/build-ios.sh
```

Then build/run on a simulator:

```bash
xcodebuild -project ios/ObSink.xcodeproj -scheme ObSink -sdk iphonesimulator \
  -destination 'platform=iOS Simulator,name=iPhone 17 Pro' CODE_SIGNING_ALLOWED=NO build
```

The SwiftUI app (sync button, status, vault setup, conflict resolution) talks to the Rust core through the generated `VaultClient`. Files sync into the App Group container (`group.com.obsink.shared`), which the File Provider extension exposes to Obsidian/Files.

Run the on-simulator integration tests (the live-sync test reads `TEST_RUNNER_OBSINK_TEST_*` env for a worker URL, API key, vault ID, and passphrase):

```bash
xcodebuild test -project ios/ObSink.xcodeproj -scheme ObSink -sdk iphonesimulator \
  -destination 'platform=iOS Simulator,name=iPhone 17 Pro' CODE_SIGNING_ALLOWED=NO
```

Remaining: iOS Keychain key storage, a full replicated File Provider (UUID identifiers, change tracking, `signalEnumerator`), and code-signing + TestFlight (select your team in Xcode — the project uses automatic signing). See spec.md §11 for the File Provider design.

## Android & Windows (planned)

Not started. Android targets Tauri mobile (with a React Native + Rust fallback); Windows needs a DPAPI/Credential Manager key store. Tracked in the Plane project `OBS` (phases P5/P6).
