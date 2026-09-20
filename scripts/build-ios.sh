#!/usr/bin/env bash
#
# Build the iOS artifacts from the Rust core:
#   1. Compile the obsink-mobile staticlib for device + simulator.
#   2. Generate the Swift UniFFI bindings.
#   3. Assemble the ObSinkMobile.xcframework.
#   4. Generate the Xcode project with XcodeGen.
#
# These outputs are build artifacts (git-ignored); run this after cloning or
# whenever the FFI surface changes. Requires: rustup + the iOS targets, Xcode, xcodegen.
#
#   --simulator-only   skip the device slice (CI: xcodebuild test on a simulator)

set -euo pipefail

SIMULATOR_ONLY=0
for arg in "$@"; do
    case "$arg" in
        --simulator-only) SIMULATOR_ONLY=1 ;;
        *) echo "unknown argument: $arg" >&2; exit 2 ;;
    esac
done

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
IOS_DIR="$REPO_ROOT/ios"
TARGET_DIR="${CARGO_TARGET_DIR:-$REPO_ROOT/target}"

# Prefer a rustup-managed toolchain when present: the iOS targets (added below)
# are only shipped via rustup, and a non-rustup cargo first on PATH (e.g. Homebrew)
# would fail the cross-compile. Checks the standard install location so it works
# even when rustup isn't on PATH (e.g. installed with --no-modify-path).
if [ -x "$HOME/.cargo/bin/rustup" ]; then
    export PATH="$HOME/.cargo/bin:$PATH"
fi

cd "$REPO_ROOT"

TARGETS=(aarch64-apple-ios-sim)
[ "$SIMULATOR_ONLY" = 1 ] || TARGETS+=(aarch64-apple-ios)

echo "==> Ensuring iOS Rust targets are installed (${TARGETS[*]})"
rustup target add "${TARGETS[@]}" >/dev/null

echo "==> Building obsink-mobile staticlib (${TARGETS[*]})"
for t in "${TARGETS[@]}"; do
    cargo build --release -p obsink-mobile --target "$t"
done

echo "==> Generating Swift bindings"
rm -rf "$IOS_DIR/Generated"
mkdir -p "$IOS_DIR/Generated"
cargo run -q -p obsink-mobile --bin uniffi-bindgen -- generate \
  --library "$TARGET_DIR/aarch64-apple-ios-sim/release/libobsink_mobile.a" \
  --language swift \
  --out-dir "$IOS_DIR/Generated"

echo "==> Assembling ObSinkMobile.xcframework"
HEADERS="$(mktemp -d)"
cp "$IOS_DIR/Generated/obsink_mobileFFI.h" "$HEADERS/"
cp "$IOS_DIR/Generated/obsink_mobileFFI.modulemap" "$HEADERS/module.modulemap"
rm -rf "$IOS_DIR/Frameworks/ObSinkMobile.xcframework"
mkdir -p "$IOS_DIR/Frameworks"
XCFRAMEWORK_ARGS=()
for t in "${TARGETS[@]}"; do
    XCFRAMEWORK_ARGS+=(-library "$TARGET_DIR/$t/release/libobsink_mobile.a" -headers "$HEADERS")
done
xcodebuild -create-xcframework "${XCFRAMEWORK_ARGS[@]}" \
  -output "$IOS_DIR/Frameworks/ObSinkMobile.xcframework"
rm -rf "$HEADERS"

echo "==> Generating Xcode project"
# DEVELOPMENT_TEAM and OBSINK_SERVER_URL live in the gitignored .env (not in
# project.yml, so the repo stays team-agnostic). XcodeGen substitutes both
# from the environment; empty is fine for simulator builds (no signing; the
# app falls back to its built-in server URL).
if [ -z "${DEVELOPMENT_TEAM:-}" ] && [ -f "$REPO_ROOT/.env" ]; then
    set -a; . "$REPO_ROOT/.env"; set +a
fi
export DEVELOPMENT_TEAM="${DEVELOPMENT_TEAM:-}"
export OBSINK_SERVER_URL="${OBSINK_SERVER_URL:-}"
( cd "$IOS_DIR" && xcodegen generate )

echo "Done. Open ios/ObSink.xcodeproj or build with:"
echo "  xcodebuild -project ios/ObSink.xcodeproj -scheme ObSink -sdk iphonesimulator \\"
echo "    -destination 'platform=iOS Simulator,name=iPhone 17 Pro' CODE_SIGNING_ALLOWED=NO build"
