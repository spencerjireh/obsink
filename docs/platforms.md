# Platform Setup

ObSink shares one Rust core across every client. This page covers per-platform setup and current status. See [progress.md](../progress.md) for the live checklist.

| Platform | Status | Notes |
|---|---|---|
| CLI (`obsink`) | ✅ Complete | Reference client; macOS Keychain for key storage |
| macOS desktop | ✅ Feature-complete in code | Tauri v2 menu-bar app |
| Linux desktop | 🟡 Builds in CI | `.deb` bundled in CI; keychain backend (libsecret) not yet wired |
| Windows desktop | ⬜ Planned | Needs DPAPI/Credential Manager keychain |
| iOS | 🟡 Core cross-compiles | App + File Provider extension not yet built |
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

## iOS (in progress)

The Rust core cross-compiles to `aarch64-apple-ios` and `aarch64-apple-ios-sim`:

```bash
rustup target add aarch64-apple-ios aarch64-apple-ios-sim
cargo build -p obsink-core --target aarch64-apple-ios
```

Still to do: a UniFFI binding layer over the (async) core API, an XCFramework, and the Xcode App + File Provider extension targets with an App Group. See spec.md §11 for the File Provider design.

## Android & Windows (planned)

Not started. Android targets Tauri mobile (with a React Native + Rust fallback); Windows needs a DPAPI/Credential Manager key store. Tracked in [progress.md](../progress.md).
