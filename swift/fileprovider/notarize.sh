#!/usr/bin/env bash
# Notarize and staple the TCFS FileProvider app bundle.
#
# Submits the signed .app to Apple's notary service, waits for approval,
# and staples the notarization ticket to the app bundle.
#
# Usage:
#   ./notarize.sh <app_path>
#   ./notarize.sh <app_path> --keychain-profile <profile>
#
# Environment variables (if not using --keychain-profile):
#   APPLE_ID                  - Apple ID email
#   APPLE_TEAM_ID             - Developer Team ID
#   APPLE_NOTARIZE_PASSWORD   - App-specific password (or @keychain: reference)
#
# Prerequisites:
#   - App must be signed with Developer ID (not ad-hoc)
#   - Xcode CLT installed (xcrun notarytool, xcrun stapler)

set -euo pipefail

APP_PATH="${1:?Usage: notarize.sh <app_path> [--keychain-profile <profile>]}"
shift

if [ ! -d "$APP_PATH" ]; then
    echo "ERROR: App not found at $APP_PATH" >&2
    exit 1
fi

APP_NAME="$(basename "$APP_PATH" .app)"
ZIP_PATH="${APP_PATH%/*}/${APP_NAME}.zip"

# --- Verify signature before submission ---
echo "==> Verifying code signature..."
if ! /usr/bin/codesign -vvv --deep "$APP_PATH" 2>&1; then
    echo "ERROR: Code signature verification failed. Sign with Developer ID first." >&2
    exit 1
fi

# --- Create submission archive ---
echo "==> Creating submission archive: $ZIP_PATH"
/usr/bin/ditto -c -k --keepParent "$APP_PATH" "$ZIP_PATH"

# --- Submit for notarization ---
echo "==> Submitting for notarization..."
if [ "${1:-}" = "--keychain-profile" ]; then
    KEYCHAIN_PROFILE="${2:?--keychain-profile requires a profile name}"
    xcrun notarytool submit "$ZIP_PATH" \
        --keychain-profile "$KEYCHAIN_PROFILE" \
        --wait
elif [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ] && [ -n "${APPLE_NOTARIZE_PASSWORD:-}" ]; then
    xcrun notarytool submit "$ZIP_PATH" \
        --apple-id "$APPLE_ID" \
        --team-id "$APPLE_TEAM_ID" \
        --password "$APPLE_NOTARIZE_PASSWORD" \
        --wait
else
    echo "ERROR: Provide --keychain-profile or set APPLE_ID, APPLE_TEAM_ID, APPLE_NOTARIZE_PASSWORD" >&2
    rm -f "$ZIP_PATH"
    exit 1
fi

# --- Staple the ticket ---
echo "==> Stapling notarization ticket..."
xcrun stapler staple "$APP_PATH"

# --- Verify Gatekeeper acceptance ---
echo "==> Verifying Gatekeeper acceptance..."
spctl --assess --type exec --verbose=2 "$APP_PATH"

# --- Cleanup ---
rm -f "$ZIP_PATH"

echo "==> Notarization complete: $APP_PATH"
