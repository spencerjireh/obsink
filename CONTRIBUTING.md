# Contributing

Conventions for agents and humans live in [AGENTS.md](AGENTS.md); this is the short version.

## Prerequisites

- Rust via rustup (the version is pinned in `rust-toolchain.toml`; `rustup toolchain install`
  in the checkout installs it), Node 22+, Docker (server tests), Xcode 26 (iOS).
- `brew install lefthook xcodegen`, then `lefthook install` once per clone. The hooks run
  rustfmt and Prettier on staged files and check the commit message.
- `cargo install cargo-deny` to run the dependency policy locally.

## Branches, commits, pull requests

- Never push to `main`; it only accepts rebase-merged pull requests with green CI.
- Branch: `<type>/obs-<n>-<slug>` (e.g. `fix/obs-91-batch-invite-reuse`).
- Commit: `<type>(<scope>)?: <subject> (OBS-<n>)`. Types
  `feat|fix|refactor|test|perf|build|chore|docs|ci`; `(OBS-<n>)` is required except for
  `chore|docs|ci`. `scripts/check-commit-msg.sh` is the rule.
- Rebase onto `main` instead of merging it in; merge commits fail the commit check.
- Fill in the pull request template. All CI jobs must pass; merges are rebase-only.

## Before you push

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace && cargo deny check
(cd desktop && npm run lint && npm run format:check && npm run build)
```

Security issues: see [SECURITY.md](SECURITY.md).
