# AGENTS.md — ObSink

Standing rules for any coding agent working in this repo. These override your
defaults. When a rule here conflicts with something you'd normally do, follow
this file. When this file conflicts with `spec.md`, ask.

ObSink is a free, self-hosted, end-to-end encrypted sync engine for
[Obsidian](https://obsidian.md) vaults. A shared Rust core drives a CLI, a
Tauri desktop app (macOS), and (in progress) an iOS client; a
Cloudflare Worker (TS) + R2 + KV is the backend. **`spec.md` is the spec source
of truth** — read it before your first task. Architecture/wire-format details
live in `docs/architecture.md`; per-platform status in `docs/platforms.md`.

## Tech (pinned versions)

- **Rust** — stable, edition 2021 (Cargo workspace: `core`, `cli`,
  `desktop/src-tauri`, `mobile`). Core deps: `aes-gcm` 0.10, `argon2` 0.5,
  `hkdf` 0.12, `hmac` 0.12, `sha2` 0.10, `reqwest` 0.12 (rustls-tls),
  `tokio` 1, `tracing` 0.1.
- **Worker** — Cloudflare Workers, `wrangler` 4.11, TypeScript 5.8,
  Vitest 3.2. Bindings: R2 (`obsink-files`), KV (`META`), secret `API_KEY`,
  two cron triggers.
- **Desktop** — Tauri v2 (`@tauri-apps` 2.0), React 18.3, Vite 5.4, TypeScript 5.6.
- **iOS** — Swift/SwiftUI + File Provider extension; Rust via **UniFFI 0.28**
  (`mobile/` crate). Project generated with XcodeGen (`ios/project.yml`).
- **Infra** — Terraform (`infra/terraform/`) for R2 + KV.

## Project structure

```
obsink/
  AGENTS.md              # this file (canonical — CLAUDE.md points here)
  CLAUDE.md              # one-line pointer to AGENTS.md
  spec.md                # spec source of truth
  core/                  # Rust sync engine: crypto, hasher, manifest, api_client, sync_engine
  cli/                   # `obsink` CLI (reference client)
  worker/                # Cloudflare Worker (TS): storage, manifest, conflict gating, cron
  desktop/               # Tauri v2 + React app (src-tauri/ + src/)
  mobile/                # UniFFI facade over core (staticlib/cdylib for iOS)
  ios/                   # Xcode project: ObSink app + FileProvider ext + Tests (XcodeGen)
  infra/terraform/       # R2 bucket + KV namespace provisioning
  scripts/               # render-worker-config, build-ios, verify-* deploy scripts
  docs/                  # self-hosting, deploy, architecture, platforms, troubleshooting
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
2. **Manual sync only.** No file watching, no background sync, no daemons. One
   button triggers the full pull→diff→download→resolve→upload cycle. Do not add
   auto-sync.
3. **Conflict-aware — never silently overwrite.** `PUT`/`DELETE` require
   `X-Parent-Hash`; on mismatch the Worker returns `409` and the client surfaces
   the conflict to the UI (keep local / keep remote / keep both). See spec §5.
4. **One key per vault.** AES-256-GCM with a random 96-bit nonce per file; blob
   = `[12-byte nonce][ciphertext][16-byte tag]`. Argon2id parameters are
   64 MiB / 3 / 1 (exceeds OWASP 2024) — do not weaken.
5. **No key recovery.** Lost passphrase = lost data. This is deliberate for v1;
   do not add recovery without an explicit decision.
6. **50 MB upload limit.** The Worker rejects larger files; the batch endpoint
   excludes large files (they go through individual `PUT`s).
7. **Tests stay green.** `cargo test --workspace` for Rust; `npm test` +
   `npm run typecheck` in `worker/`. Run them before considering work done.
8. **No new dependencies without a one-line justification.** The core crypto
   stack (aes-gcm, argon2, hkdf, hmac, sha2) is fixed — do not swap it out.

## Commands

```bash
# Rust core + CLI tests
cargo test --workspace

# Worker (TS): typecheck + Vitest
(cd worker && npm ci && npm test && npm run typecheck)

# Desktop (build the web bundle, then check the Tauri Rust)
(cd desktop && npm ci && npm run build && cargo check -p obsink-desktop)

# Build iOS: device+simulator staticlibs, UniFFI bindings, xcframework, XcodeGen
scripts/build-ios.sh

# Run the CLI against your deployed Worker (logs to stderr)
RUST_LOG=obsink_core=debug cargo run -p obsink -- sync

# Deploy the Worker (requires wrangler auth + rendered wrangler.toml)
(cd worker && npm run deploy)
```

## Local credentials (gitignored)

The deployed test Worker and its client bearer live in a **gitignored** `.env`
at the repo root (never commit it; `.gitignore` covers `.env`/`.env.*`/`worker/.dev.vars`).
Source it for the CLI and the `scripts/verify-*` harnesses:

```bash
set -a; . ./.env; set +a
RUST_LOG=obsink_core=debug cargo run -p obsink -- sync
```

- `WORKER_URL` — `https://obsink-worker.spencer-080.workers.dev` (account
  `Spencer` / `080eb52a3c7398cf1e99d39f2c664bc8`).
- `WORKER_API_KEY` — the Worker's `API_KEY` secret (client bearer; rotated
  2026-07-30). Cloudflare secrets are write-only, so this is the only copy.

The Cloudflare **account API token** used for `wrangler`/deploys is **not**
stored here — create one in the dashboard on demand and revoke it after. A
`#[ignore]`d live desktop test reads `OBSINK_TEST_WORKER_URL` /
`OBSINK_TEST_API_KEY` (see `docs/platforms.md`).

## Workflow expectations

- Rules questions: check `spec.md` first; if the spec is ambiguous, say so and
  propose a clarification instead of guessing. Log the outcome on the decision
  log (see below).
- Prefer small commits per session/task. Reference the Plane work item in the
  message: `P4: file-provider enumerateChanges (OBS-12)`.
- Crypto changes require matching test updates (round-trip, wrong-key rejection,
  tamper detection). Never ship crypto without tests.

## Current status and project management

Status, tasks, decisions, and session logs live in the Plane project **OBS**
("ObSink"), reachable via the plane MCP tools. Conventions:

- `spec.md` phases **P1–P6** are Plane *modules*; module status tracks phase
  progression (P1/P2/P3 completed; P4/P6 in-progress; P5 backlog).
- Work items are session-sized; move to **In Progress** when starting, comment
  outcomes (e.g. test output or a deploy URL), then mark **Done**. Reference the
  item in commits: `P4: file-provider enumerateChanges (OBS-12)`.
- Decisions and session notes go as comments on the pinned `[Log]` work items —
  `[Log] Decision log` (OBS-74) and `[Log] Session log` (OBS-73); one comment
  per entry, newest last. The specs themselves stay in this repo (`spec.md`);
  the logs record the choices around them.
- The backlog is fully detailed for all phases P1–P6.
