#!/usr/bin/env bash
#
# Build the iOS artifacts from the Rust core:
#   1. Compile the obsink-mobile staticlib for device + simulator.
#   2. Generate the Swift UniFFI bindings.
#   3. Assemble the ObSinkMobile.xcframework.
#   4. Generate the Xcode project with XcodeGen.
#
# These outputs are build artifacts (git-ignored); run this after cloning or
# whenever the FFI surface changes. Requires: rustup iOS targets, xcodegen.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
IOS_DIR="$REPO_ROOT/ios"
TARGET_DIR="${CARGO_TARGET_DIR:-$REPO_ROOT/target}"

cd "$REPO_ROOT"

echo "==> Ensuring iOS Rust targets are installed"
rustup target add aarch64-apple-ios aarch64-apple-ios-sim >/dev/null

echo "==> Building obsink-mobile staticlib (device + simulator)"
cargo build --release -p obsink-mobile --target aarch64-apple-ios
cargo build --release -p obsink-mobile --target aarch64-apple-ios-sim

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
xcodebuild -create-xcframework \
  -library "$TARGET_DIR/aarch64-apple-ios/release/libobsink_mobile.a" -headers "$HEADERS" \
  -library "$TARGET_DIR/aarch64-apple-ios-sim/release/libobsink_mobile.a" -headers "$HEADERS" \
  -output "$IOS_DIR/Frameworks/ObSinkMobile.xcframework"
rm -rf "$HEADERS"

echo "==> Generating Xcode project"
( cd "$IOS_DIR" && xcodegen generate )

echo "Done. Open ios/ObSink.xcodeproj or build with:"
echo "  xcodebuild -project ios/ObSink.xcodeproj -scheme ObSink -sdk iphonesimulator \\"
echo "    -destination 'platform=iOS Simulator,name=iPhone 17 Pro' CODE_SIGNING_ALLOWED=NO build"
