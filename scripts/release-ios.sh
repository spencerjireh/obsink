#!/usr/bin/env bash
#
# Archive, sign, and upload the iOS app to App Store Connect (TestFlight).
#
# Credentials come from the gitignored .env:
#   DEVELOPMENT_TEAM   Apple team ID (also used by XcodeGen for signing)
#   ASC_KEY_ID         App Store Connect API key ID
#   ASC_ISSUER_ID      App Store Connect API issuer ID
#   ASC_KEY_PATH       path to the AuthKey_<KEY_ID>.p8 file
# The API key drives both automatic provisioning (-allowProvisioningUpdates)
# and the upload, so no Xcode account login is needed.
#
# Usage: scripts/release-ios.sh [--version 0.1.0] [--build N] [--no-upload]
#   --build defaults to the commit count on HEAD (monotonic, unique per commit).
#   --no-upload exports a signed .ipa to ios/build/export instead of uploading.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
IOS_DIR="$REPO_ROOT/ios"

set -a; . "$REPO_ROOT/.env"; set +a
: "${DEVELOPMENT_TEAM:?set DEVELOPMENT_TEAM in .env}"
: "${ASC_KEY_ID:?set ASC_KEY_ID in .env}"
: "${ASC_ISSUER_ID:?set ASC_ISSUER_ID in .env}"
: "${ASC_KEY_PATH:?set ASC_KEY_PATH in .env}"
[ -f "$ASC_KEY_PATH" ] || { echo "ASC_KEY_PATH not found: $ASC_KEY_PATH"; exit 1; }
export DEVELOPMENT_TEAM

VERSION="$(sed -n 's/^ *MARKETING_VERSION: "\(.*\)"/\1/p' "$IOS_DIR/project.yml")"
BUILD="$(git -C "$REPO_ROOT" rev-list --count HEAD)"
UPLOAD=1
while [ $# -gt 0 ]; do
    case "$1" in
        --version) VERSION="$2"; shift 2 ;;
        --build) BUILD="$2"; shift 2 ;;
        --no-upload) UPLOAD=0; shift ;;
        *) echo "unknown arg: $1"; exit 1 ;;
    esac
done

AUTH=(-allowProvisioningUpdates
      -authenticationKeyPath "$ASC_KEY_PATH"
      -authenticationKeyID "$ASC_KEY_ID"
      -authenticationKeyIssuerID "$ASC_ISSUER_ID")
ARCHIVE="$IOS_DIR/build/ObSink-$VERSION-$BUILD.xcarchive"
EXPORT_DIR="$IOS_DIR/build/export"

if [ ! -d "$IOS_DIR/Frameworks/ObSinkMobile.xcframework" ]; then
    echo "==> No xcframework yet; running scripts/build-ios.sh"
    "$SCRIPT_DIR/build-ios.sh"
fi

echo "==> Generating Xcode project (team $DEVELOPMENT_TEAM)"
( cd "$IOS_DIR" && xcodegen generate >/dev/null )

echo "==> Archiving ObSink $VERSION ($BUILD) for iOS devices"
xcodebuild archive \
    -project "$IOS_DIR/ObSink.xcodeproj" -scheme ObSink -configuration Release \
    -destination 'generic/platform=iOS' \
    -archivePath "$ARCHIVE" \
    MARKETING_VERSION="$VERSION" CURRENT_PROJECT_VERSION="$BUILD" \
    "${AUTH[@]}" | grep -E "error|warning: .*(entitle|sign)|ARCHIVE" || true
[ -d "$ARCHIVE" ] || { echo "archive failed"; exit 1; }

# Inject the team ID; the checked-in ExportOptions.plist stays team-agnostic.
OPTS="$(mktemp -t obsink-export).plist"
cp "$IOS_DIR/ExportOptions.plist" "$OPTS"
plutil -replace teamID -string "$DEVELOPMENT_TEAM" "$OPTS"
if [ "$UPLOAD" = 0 ]; then
    plutil -replace destination -string export "$OPTS"
fi

echo "==> Exporting$([ "$UPLOAD" = 1 ] && echo ' + uploading to App Store Connect')"
rm -rf "$EXPORT_DIR"
xcodebuild -exportArchive \
    -archivePath "$ARCHIVE" \
    -exportOptionsPlist "$OPTS" \
    -exportPath "$EXPORT_DIR" \
    "${AUTH[@]}" | grep -E "error|EXPORT|Upload|upload" || true
rm -f "$OPTS"

if [ "$UPLOAD" = 1 ]; then
    echo "Uploaded ObSink $VERSION ($BUILD). App Store Connect processes it in ~5-15 min;"
    echo "then add it to a TestFlight group (scripts/testflight.py) or in the ASC UI."
else
    echo "Exported to $EXPORT_DIR"
fi
