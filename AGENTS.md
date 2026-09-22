# AGENTS.md — ObSink

Standing rules for any coding agent working in this repo. These override your
defaults. When a rule here conflicts with something you'd normally do, follow
this file. When this file conflicts with `spec.md`, ask.

ObSink is a free, self-hosted, end-to-end encrypted sync engine for
[Obsidian](https://obsidian.md) vaults. A shared Rust core drives a CLI, a
Tauri desktop app (macOS), and an iOS client; the backend is a Rust server
(`server/`, axum + Postgres + an encrypted blob volume) that operators run with
`docker compose up` behind their own TLS proxy. Accounts are invite-only
(email one-time code everywhere, Sign in with Apple on iOS). **`spec.md` is the
spec source of truth** — read it before your first task. Architecture/wire-format
details live in `docs/architecture.md`; per-platform status in `docs/platforms.md`;
deployment in `docs/self-hosting.md`.

## Tech (pinned versions)

- **Rust** — stable, edition 2021 (Cargo workspace: `core`, `core-wasm`, `cli`,
  `desktop/src-tauri`, `mobile`, `server`). Core deps: `aes-gcm` 0.10, `argon2` 0.5,
  `hkdf` 0.12, `hmac` 0.12, `sha2` 0.10, `reqwest` 0.12 (rustls-tls),
  `tokio` 1, `tracing` 0.1; `security-framework` 3 behind the `keychain`
  feature (CLI + desktop keychain access without a `security` argv).
- **Server** — `server/` crate (`obsink-server`): `axum` 0.8 (HTTP +
  multipart), `tokio`, `tower-http` (tracing), `sqlx` 0.8 (Postgres, runtime
  queries, embedded migrations), `lettre` 0.11 (SMTP one-time codes),
  `jsonwebtoken` 9 (Apple identity tokens), `reqwest` (JWKS fetch,
  healthcheck), `subtle` (constant-time operator key compare), `rand`/`uuid`
  (tokens, ids), `walkdir` (retention), `clap` (subcommands); envelope crypto
  uses the same `aes-gcm`/`hkdf`/`hmac`/`sha2` versions as core. Dev:
  `tempfile`, `rsa` (forges Apple tokens in tests). Env vars are listed in
  `server/src/config.rs`; deployment in `docs/self-hosting.md`; API in spec §4.
- **Desktop** — Tauri v2 (`@tauri-apps` 2.0), React 18.3, Vite 5.4, TypeScript 5.6.
- **Web** — npm workspaces `ui` (the React screens and the `Backend` interface,
  shared with desktop), `desktop` (the Tauri `Backend`) and `web` (the browser
  client: a Web Worker over `core-wasm` (wasm-bindgen 0.2, wasm-pack 0.15),
  `fetch` and the File System Access API, IndexedDB for state; Vite, vitest,
  Playwright for the e2e harness).
- **iOS** — Swift/SwiftUI + File Provider extension; Rust via **UniFFI 0.28**
  (`mobile/` crate). Project generated with XcodeGen (`ios/project.yml`).
- **Infra** — `server/Dockerfile` (cargo-chef, distroless) and `web/Dockerfile`
  (wasm-pack + npm build, Caddy: the landing page from `site/`, the client at
  `/app`, and a proxy for the API paths; `web/Caddyfile`). `docker-compose.yml`
  (local: server and web built from the checkout, Postgres 16, Mailpit) and
  `docker-compose.coolify.yml` (production: Coolify builds both images from the
  branch on every deploy, Postgres; `web` takes the site domain, `server` the
  API domain). TLS is the operator's proxy (Coolify Traefik). Windows, Linux,
  and Android clients are out of scope.

## Project structure

```
obsink/
  AGENTS.md              # this file (canonical — CLAUDE.md points here)
  CLAUDE.md              # one-line pointer to AGENTS.md
  spec.md                # spec source of truth
  DESIGN.md              # UI rules: principles, brand, tokens, components, copy, platform mapping
  design/                # icon.svg + tray.svg (icon sources; rasters come from scripts/gen-icons.sh)
  core/                  # Rust sync engine: crypto, hasher, manifest, api_client, auth, sync_engine
                         #   (crypto, manifest, ignore, sync_rules, pacing, server_url also build for wasm32)
  core-wasm/             # wasm-bindgen bindings over the pure core modules for the browser client
  cli/                   # `obsink` CLI (reference client)
  server/                # obsink-server (axum): accounts, vaults, files, batch, retention; Dockerfile
  ui/                    # shared React screens + the Backend interface (npm workspace, TS source)
  desktop/               # Tauri v2 shell (src-tauri/) + the Tauri Backend and entry (src/)
  web/                   # browser client at /app: worker Backend (File System Access + core-wasm + fetch);
                         #   Dockerfile + Caddyfile for the website container
  site/                  # landing page (index.html, icon.svg) and install.sh, served by the web container
  mobile/                # UniFFI facade over core (staticlib/cdylib for iOS)
  ios/                   # Xcode project: ObSink app + FileProvider ext + Tests (XcodeGen)
  docker-compose.yml     # local stack; docker-compose.coolify.yml for production
  scripts/               # build-ios, release-ios, testflight.py, ci-import-signing-cert, gen-icons,
                         #   verify-* harnesses (server, CLI, iOS sim, web e2e, desktop live + smoke),
                         #   test-web-container, test-install-sh, check-commit-msg, merge-pr
  docs/                  # self-hosting, architecture, platforms, troubleshooting, p4-plan
  .github/               # ci.yml, release.yml, rulesets/main.json, PR template
  lefthook.yml           # git hooks: rustfmt, prettier, commit message
  deny.toml              # cargo-deny policy (advisories, licenses, bans, sources)
  rust-toolchain.toml    # pinned Rust version (CI and local)
```

## Hard rules (non-negotiables)

1. **The server never sees plaintext.** Wire format v2: the Argon2id master key
   is *only* HKDF input. Four purpose-separated sub-keys are derived — content
   encryption, content MAC, path token, path encryption. Manifest entries are
   keyed by `HMAC(path_token_key, path)`; the manifest `hash` is
   `HMAC(content_mac_key, plaintext)`; an `encPath` (AES-GCM of the real path)
   lets a fresh device recover filenames. **Never** put plaintext content hashes
   or plaintext paths on the wire (that was v1's information leak — do not
   reintroduce it).
2. **Sync is driven, not ambient.** The engine (`prepare_sync` /
   `complete_sync`) stays a pure, caller-triggered cycle with no timers and no
   watcher inside it: one call runs the full pull→diff→download→resolve→upload
   cycle. Automatic syncing lives in drivers outside the engine (the iOS
   foreground and background refresh; `core/src/daemon.rs` for the desktop
   app and `obsink watch`) that decide *when* to call it. Nothing in
   `core/src/sync_engine.rs` may grow a clock or a file watcher, and no driver
   may resolve a conflict on its own (the daemon defers every conflict to the
   user).
3. **Conflict-aware — never silently overwrite.** `PUT`/`DELETE` require
   `X-Parent-Hash`; on mismatch the server returns `409` and the client surfaces
   the conflict to the UI (keep local / keep remote / keep both). The check runs
   in one Postgres transaction that locks the vault row. See spec §5.
4. **One key per vault.** AES-256-GCM with a random 96-bit nonce per file; blob
   = `[12-byte nonce][ciphertext][16-byte tag]`. Argon2id parameters are
   64 MiB / 3 / 1 (exceeds OWASP 2024) — do not weaken.
5. **No key recovery.** Lost passphrase = lost data. This is deliberate for v1;
   do not add recovery without an explicit decision.
6. **50 MB upload limit.** The server rejects larger files (`413`); a
   per-account byte budget answers `507`, which the sync engine treats as fatal.
7. **Tests and lints stay green.** `cargo test --workspace` (server integration
   tests skip without `DATABASE_URL`; run them against Postgres before touching
   `server/`), `cargo clippy --workspace --all-targets -- -D warnings`,
   `cargo deny check`, and at the repo root `npm run lint && npm run format:check
   && npm run typecheck -w ui && npm run build -w desktop && npm run typecheck -w web
   && npm run test -w web && npm run build -w web` (the web steps need
   `wasm-pack build core-wasm --target web` first). CI enforces all of them on
   every PR, plus the container route checks (`scripts/test-web-container.sh`),
   the install script test (`scripts/test-install-sh.sh`) and the desktop unit
   tests (`cargo test -p obsink-desktop`); run them before considering work
   done. The desktop's live command tests and the window smoke run locally
   against the compose stack: `scripts/verify-desktop-live.sh` and
   `node scripts/verify-desktop-smoke.mjs` (optional, not a release gate).
8. **No new dependencies without a one-line justification.** The core crypto
   stack (aes-gcm, argon2, hkdf, hmac, sha2) is fixed — do not swap it out.

## Commands

```bash
# Once per clone: git hooks (rustfmt, prettier, commit-message check)
brew install lefthook xcodegen && lefthook install

# Rust core + CLI tests
cargo test --workspace

# Lints CI enforces
cargo clippy --workspace --all-targets -- -D warnings && cargo deny check

# The core for the browser (wasm32 comes from rust-toolchain.toml; pin matches ci.yml)
cargo install wasm-pack --locked --version 0.15.0
wasm-pack build core-wasm --target web && wasm-pack test --node --release core-wasm

# Server integration tests need Postgres (any throwaway instance):
docker run -d --name obsink-test-pg -e POSTGRES_PASSWORD=postgres -p 5433:5432 postgres:16-alpine
DATABASE_URL=postgres://postgres:postgres@localhost:5433/postgres OBSINK_TEST_REQUIRE_DB=1 cargo test -p obsink-server

# Local server stack (server built from this checkout + Postgres + Mailpit on :8025)
docker compose up -d --wait     # OBSINK_PORT=18080 if 8080 is taken; never reads .env.deploy
docker compose exec server obsink-server invite

# Desktop (lint + format check, build the web bundle, then check the Tauri Rust)
npm ci && npm run lint && npm run format:check && npm run typecheck -w ui && npm run build -w desktop && cargo test -p obsink-desktop
scripts/verify-desktop-live.sh          # ignored live command tests against the compose stack on :18080
node scripts/verify-desktop-smoke.mjs   # real windows + tray through the debug automation seam

# Browser client (needs the wasm-pack build above first)
npm run typecheck -w web && npm run test -w web && npm run build -w web

# Build iOS: device+simulator staticlibs, UniFFI bindings, xcframework, XcodeGen
scripts/build-ios.sh

# Run the CLI against a server (logs to stderr)
RUST_LOG=obsink_core=debug cargo run -p obsink -- sync

# Contract + two-device checks against a running server (uses .env.deploy)
scripts/verify-server-deploy.sh && scripts/verify-cli-deployed-sync.sh
```

## Local credentials (gitignored)

The server URL and operator bearer used by the CLI and the `scripts/verify-*`
harnesses live in a **gitignored** `.env.deploy` at the repo root (copy
`.env.deploy.example`; `.gitignore` covers `.env` and `.env.*`). Docker compose
does not read it, so the local stack always runs with its own defaults
(`dev-operator-key`, Mailpit). Source it:

```bash
set -a; . ./.env.deploy; set +a
RUST_LOG=obsink_core=debug cargo run -p obsink -- sync
```

- `OBSINK_SERVER_URL` — the server to test against (`http://localhost:8080`
  for the local compose stack; the Coolify domain for production).
- `OBSINK_API_KEY` — the operator bearer; the same value is the server's
  `OBSINK_API_KEY` env var. Never hand it to a tester — they sign up with an
  invite (`obsink invite`).
- `DEVELOPMENT_TEAM`, `ASC_KEY_ID`, `ASC_ISSUER_ID`, `ASC_KEY_PATH` — Apple
  signing and App Store Connect for `scripts/build-ios.sh` / `release-ios.sh`.
- Clients store bearers in the OS keychain (service `obsink`, account
  `bearer:<server url>`), never in `config.toml` / `app.json` / UserDefaults.
- Production secrets (`OBSINK_SERVER_KEY`, `POSTGRES_PASSWORD`, `SMTP_*`) live
  only in Coolify's environment. Back up `OBSINK_SERVER_KEY`: without it the
  server's metadata is unreadable.

The Cloudflare Worker, R2 bucket, KV namespace, and Resend key from before the
P8 pivot are decommissioned; nothing in the repo references them.

## Workflow expectations

- UI, copy, or icon changes follow `DESIGN.md` (shared labels, tokens, the
  XCUITest identifiers it lists as API). Regenerate rasters with
  `scripts/gen-icons.sh`; never hand-edit a PNG or `.icns`.
- Rules questions: check `spec.md` first; if the spec is ambiguous, say so and
  propose a clarification instead of guessing. Log the outcome on the decision
  log (see below).
- Prefer small commits per session/task. Reference the Plane work item in the
  message: `feat(ios): file-provider enumerateChanges (OBS-12)` (format below).
- Crypto changes require matching test updates (round-trip, wrong-key rejection,
  tamper detection). Never ship crypto without tests.

## Git workflow

- **Never push to `main`.** A ruleset (`.github/rulesets/main.json`, applied with
  `gh api`) requires a pull request with every CI job green on a branch that is
  up to date with `main`, allows rebase merges only, and blocks force-pushes
  and deletion; there are no bypass actors. Branch -> PR -> CI green ->
  `scripts/merge-pr.sh <n> [<n> ...]`, which rebases each PR onto `main` in a
  worktree, waits for its checks and rebase-merges it, in order (GitHub's own
  merge queue is not offered to user-owned repositories). Nobody rebases a
  chain of PRs by hand.
- **Branches:** `<type>/obs-<n>-<slug>`, e.g. `feat/obs-95-batch-undo`,
  `ci/obs-94-repo-hygiene`.
- **Commits:** `<type>(<scope>)?: <subject> (OBS-<n>)`. Types
  `feat|fix|refactor|test|perf|build|chore|docs|ci`; scope optional, lowercase
  (`core`, `cli,desktop`); `(OBS-<n>)` required for
  `feat|fix|refactor|test|perf|build`, optional for `chore|docs|ci`. Enforced by
  `scripts/check-commit-msg.sh`: the lefthook `commit-msg` hook locally and the
  `commits` CI job over the PR range. Regex:
  `^(feat|fix|refactor|test|perf|build)(\([a-z0-9-]+([,/][a-z0-9-]+)*\))?: .+ \(OBS-[0-9]+(, OBS-[0-9]+)*\)$`
  or `^(chore|docs|ci)(\([a-z0-9-]+([,/][a-z0-9-]+)*\))?: .+$`. Rebase branches
  onto `main`; merge commits fail the check.
- **PRs:** fill in `.github/pull_request_template.md` (Plane item, summary, how
  tested, spec impact). Rebase merge only, through `scripts/merge-pr.sh`; the
  branch is deleted on merge.
  Required checks are the seven CI jobs by display name (`commits`,
  `core + CLI + mobile`, `cargo deny`, `server (postgres)`, `web`,
  `desktop (macOS)`, `ios (simulator)`); renaming or adding a job means updating `rulesets/main.json` and
  re-applying it (`gh api -X PUT repos/spencerjireh/obsink/rulesets/<id> --input .github/rulesets/main.json`).
- **Hooks:** `brew install lefthook && lefthook install` once per clone.
- **Version bump and release:** in one PR (`build: bump version to X.Y.Z (OBS-<n>)`),
  set `[workspace.package].version` in `Cargo.toml`, run
  `npm version --no-git-tag-version X.Y.Z` in `desktop/`, `ui/` and `web/`, set
  `version` in `desktop/src-tauri/tauri.conf.json`, and set `MARKETING_VERSION`
  in `ios/project.yml` (TestFlight builds read it). After it merges:
  `git fetch origin && git tag -a vX.Y.Z -m "ObSink X.Y.Z" origin/main && git push origin vX.Y.Z`.
  The Release workflow fails if any of those six versions differs from the tag,
  then builds the universal (Apple Silicon + Intel) CLI tarball and the
  Developer ID signed universal DMG (the app and the disk image both notarized
  and stapled, the CLI binary notarized), and one `publish` job creates the
  GitHub release with generated notes from both artifacts. `workflow_dispatch`
  runs the same pipeline as a dry run (artifacts on the run, no release) to
  prove signing and notarization before a tag; `dry_run=false` with an
  existing tag republishes it. It needs the `APPLE_CERTIFICATE` (base64 .p12),
  `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_API_KEY`,
  `APPLE_API_ISSUER` and `APPLE_API_KEY_P8` repository secrets; every release
  build bakes `https://obsink-api.spencerjireh.com`. iOS ships separately with
  `scripts/release-ios.sh` (which runs `build-ios.sh` when the device slice is
  missing) and `scripts/testflight.py`.

## Current status and project management

Status, tasks, decisions, and session logs live in the Plane project **OBS**
("ObSink"), reachable via the plane MCP tools. Conventions:

- `spec.md` phases are Plane *modules*; module status tracks phase
  progression (P1/P2/P3/P7 completed; P4/P6/P8 in-progress; P5 cancelled). P8 =
  the self-hosted server pivot (OBS-82..89); P7 (Cloudflare accounts) is
  superseded by it.
- Work items are session-sized; move to **In Progress** when starting, comment
  outcomes (e.g. test output or a deploy URL), then mark **Done**. Reference the
  item in commits: `feat(ios): file-provider enumerateChanges (OBS-12)`.
- Decisions and session notes go as comments on the pinned `[Log]` work items —
  `[Log] Decision log` (OBS-74) and `[Log] Session log` (OBS-73); one comment
  per entry, newest last. The specs themselves stay in this repo (`spec.md`);
  the logs record the choices around them.
- The backlog is fully detailed for all phases P1–P6.
