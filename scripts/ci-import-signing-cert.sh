#!/usr/bin/env bash
#
# Import the Developer ID Application certificate into a throwaway keychain on
# a CI runner so `codesign` can use it (release.yml, the CLI job; Tauri's
# bundler does the same on its own for the desktop job).
#
#   APPLE_CERTIFICATE           base64 of the exported .p12
#   APPLE_CERTIFICATE_PASSWORD  its password
#
# The keychain lives under $RUNNER_TEMP and is added to the search list; only
# codesign and security may use the key. The job deletes the keychain in an
# `if: always()` step (release.yml), and the runner is discarded anyway.

set -euo pipefail

: "${APPLE_CERTIFICATE:?set APPLE_CERTIFICATE (base64 .p12)}"
: "${APPLE_CERTIFICATE_PASSWORD:?set APPLE_CERTIFICATE_PASSWORD}"

KEYCHAIN="${RUNNER_TEMP:-/tmp}/signing.keychain-db"
KEYCHAIN_PASSWORD="$(openssl rand -hex 16)"
CERT="${RUNNER_TEMP:-/tmp}/certificate.p12"

printf '%s' "$APPLE_CERTIFICATE" | base64 --decode > "$CERT"
security create-keychain -p "$KEYCHAIN_PASSWORD" "$KEYCHAIN"
security set-keychain-settings -lut 21600 "$KEYCHAIN"
security unlock-keychain -p "$KEYCHAIN_PASSWORD" "$KEYCHAIN"
security import "$CERT" -P "$APPLE_CERTIFICATE_PASSWORD" -t cert -f pkcs12 -k "$KEYCHAIN" \
    -T /usr/bin/codesign -T /usr/bin/security
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$KEYCHAIN_PASSWORD" "$KEYCHAIN" >/dev/null
security list-keychain -d user -s "$KEYCHAIN" login.keychain
rm -f "$CERT"

security find-identity -v -p codesigning "$KEYCHAIN"
