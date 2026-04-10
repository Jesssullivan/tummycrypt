#!/usr/bin/env bash
# Build TCFSDaemon.app bundle — wraps the tcfsd binary for TCC persistence.
#
# macOS Sequoia TCC grants are tied to bundle ID + CDHash. Bare binaries
# in /nix/store/ lose grants on every rebuild. This bundle provides a
# stable identity (io.tinyland.tcfsd) so TCC grants persist.
#
# Usage:
#   ./build.sh <tcfsd_binary> <output_dir> [signing_identity]
#
# Examples:
#   ./build.sh target/release/tcfsd build/
#   ./build.sh /nix/store/.../bin/tcfsd build/ auto
#   ./build.sh target/release/tcfsd build/ "Developer ID Application: ..."

set -euo pipefail

TCFSD_BINARY="${1:?Usage: build.sh <tcfsd_binary> <output_dir> [signing_identity]}"
OUTPUT_DIR="${2:?Usage: build.sh <tcfsd_binary> <output_dir> [signing_identity]}"
SIGNING_IDENTITY="${3:--}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
RESOURCES_DIR="$SCRIPT_DIR/resources"

# Auto-detect Developer ID from Keychain
if [ "$SIGNING_IDENTITY" = "auto" ]; then
  SIGNING_IDENTITY=$(security find-identity -v -p codesigning | grep "Developer ID Application" | head -1 | sed 's/.*"\(.*\)".*/\1/' || true)
  if [ -z "$SIGNING_IDENTITY" ]; then
    echo "WARNING: No Developer ID found, falling back to ad-hoc" >&2
    SIGNING_IDENTITY="-"
  fi
fi

APP_NAME="TCFSDaemon"
APP_BUNDLE="$OUTPUT_DIR/${APP_NAME}.app"

echo "==> Building ${APP_NAME}.app"
echo "    Binary:    $TCFSD_BINARY"
echo "    Output:    $APP_BUNDLE"

# ── Verify inputs ──────────────────────────────────────────────────────
if [ ! -f "$TCFSD_BINARY" ]; then
  echo "ERROR: tcfsd binary not found at $TCFSD_BINARY" >&2
  exit 1
fi

# ── Assemble bundle ────────────────────────────────────────────────────
rm -rf "$APP_BUNDLE"
mkdir -p "$APP_BUNDLE/Contents/MacOS"

cp "$TCFSD_BINARY" "$APP_BUNDLE/Contents/MacOS/tcfsd"
cp "$RESOURCES_DIR/Info.plist" "$APP_BUNDLE/Contents/Info.plist"

# ── Resolve entitlements ───────────────────────────────────────────────
ENTITLEMENTS="$APP_BUNDLE/Contents/tcfsd.entitlements"
if [ "$SIGNING_IDENTITY" != "-" ]; then
  # Extract TeamID from signing identity for entitlements substitution
  TEAM_ID=$(echo "$SIGNING_IDENTITY" | grep -oE '\([A-Z0-9]+\)$' | tr -d '()' || true)
  if [ -n "$TEAM_ID" ]; then
    echo "    TeamID:    $TEAM_ID"
    sed "s/\$(TeamIdentifierPrefix)/${TEAM_ID}./" "$RESOURCES_DIR/tcfsd.entitlements" > "$ENTITLEMENTS"
  else
    cp "$RESOURCES_DIR/tcfsd.entitlements" "$ENTITLEMENTS"
  fi
else
  # Ad-hoc: strip team prefix requirement
  sed 's/\$(TeamIdentifierPrefix)//' "$RESOURCES_DIR/tcfsd.entitlements" > "$ENTITLEMENTS"
fi

# ── Code sign ──────────────────────────────────────────────────────────
# Move entitlements out of bundle before signing (codesign treats files
# in Contents/ as subcomponents that must themselves be signed).
SIGN_ENTITLEMENTS="/tmp/tcfsd-sign-$$.entitlements"
mv "$ENTITLEMENTS" "$SIGN_ENTITLEMENTS"

echo "==> Signing..."
if [ "$SIGNING_IDENTITY" = "-" ]; then
  echo "    Identity: ad-hoc"
  codesign -f -s - --options runtime --entitlements "$SIGN_ENTITLEMENTS" "$APP_BUNDLE"
else
  echo "    Identity: $SIGNING_IDENTITY"
  codesign -f -s "$SIGNING_IDENTITY" --options runtime --timestamp --entitlements "$SIGN_ENTITLEMENTS" "$APP_BUNDLE"
fi
rm -f "$SIGN_ENTITLEMENTS"

# ── Validate ───────────────────────────────────────────────────────────
echo "==> Validating..."
if codesign -v "$APP_BUNDLE" 2>/dev/null; then
  echo "    Signature: OK"
else
  echo "    Signature: FAILED" >&2
  exit 1
fi

echo "==> Done: $APP_BUNDLE"
echo ""
echo "Install to /Applications/ for TCC persistence:"
echo "  cp -R $APP_BUNDLE /Applications/"
echo ""
echo "Then update launchd to reference:"
echo "  /Applications/${APP_NAME}.app/Contents/MacOS/tcfsd"
