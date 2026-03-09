#!/usr/bin/env bash
# Steps 3-6: xcodegen, archive, export, upload
# Run from Terminal.app (needs keychain entitlements)
set -euo pipefail

IOS_DIR="$(cd "$(dirname "$0")/.." && pwd)"

# === Nuke the Nix devshell environment ===
# direnv/nix sets CC, LD, SDKROOT, NIX_* etc. which corrupt xcodebuild.
# We must run xcodebuild in a pristine environment.
CLEAN_PATH="/usr/bin:/bin:/usr/sbin:/sbin:/usr/local/bin:/opt/homebrew/bin"

# Resolve tool paths with clean env
XCODEBUILD=$(env -i PATH="$CLEAN_PATH" /usr/bin/xcrun --find xcodebuild)
ALTOOL=$(env -i PATH="$CLEAN_PATH" /usr/bin/xcrun --find altool 2>/dev/null || echo "xcrun altool")
XCODEGEN=$(command -v xcodegen || echo "/opt/homebrew/bin/xcodegen")

echo "    xcodebuild: $XCODEBUILD"
echo "    altool:     $ALTOOL"
echo "    xcodegen:   $XCODEGEN"

echo ""
echo "==> Checking signing identity..."
security find-identity -v -p codesigning
if security find-identity -v -p codesigning 2>&1 | grep -q "0 valid identities"; then
  echo ""
  echo "ERROR: No signing identities found. Run setup-signing.sh first from Terminal.app"
  exit 1
fi

echo ""
echo "==> Step 3: Generating Xcode project..."
cd "$IOS_DIR"
"$XCODEGEN" generate

echo ""
echo "==> Step 4: Archiving..."
ARCHIVE_PATH="$IOS_DIR/build/TCFS.xcarchive"
rm -rf "$ARCHIVE_PATH"

# Run xcodebuild with env -i to strip ALL Nix/direnv pollution
env -i \
  HOME="$HOME" \
  USER="$USER" \
  PATH="$CLEAN_PATH" \
  TMPDIR="${TMPDIR:-/tmp}" \
  "$XCODEBUILD" archive \
    -project "$IOS_DIR/TCFS.xcodeproj" \
    -scheme "TCFS" \
    -sdk iphoneos \
    -configuration Release \
    -archivePath "$ARCHIVE_PATH" \
    CODE_SIGN_STYLE=Manual \
    "CODE_SIGN_IDENTITY=Apple Distribution: John Sullivan (QP994XQKNH)" \
    DEVELOPMENT_TEAM=QP994XQKNH \
    ONLY_ACTIVE_ARCH=NO \
    SKIP_INSTALL=NO

echo "    Archive: $ARCHIVE_PATH"

echo ""
echo "==> Step 5: Exporting IPA..."
EXPORT_DIR="$IOS_DIR/build/export"
rm -rf "$EXPORT_DIR"

if ! env -i \
  HOME="$HOME" \
  USER="$USER" \
  PATH="/usr/bin:/bin:/usr/sbin:/sbin" \
  TMPDIR="${TMPDIR:-/tmp}" \
  "$XCODEBUILD" -exportArchive \
    -archivePath "$ARCHIVE_PATH" \
    -exportPath "$EXPORT_DIR" \
    -exportOptionsPlist "$IOS_DIR/Scripts/ExportOptions.plist" \
    -allowProvisioningUpdates \
    -authenticationKeyPath "$HOME/.private_keys/AuthKey_ZV65L9B864.p8" \
    -authenticationKeyID ZV65L9B864 \
    -authenticationKeyIssuerID d5db1c0a-0a82-4a50-9490-7d86be080506; then
  echo ""
  echo "==> EXPORT FAILED — checking distribution logs..."
  LATEST_LOG=$(ls -td /var/folders/*/T/TCFS_*.xcdistributionlogs 2>/dev/null | head -1)
  if [ -n "$LATEST_LOG" ]; then
    echo "    Log: $LATEST_LOG"
    cat "$LATEST_LOG/IDEDistribution.standard.log" 2>/dev/null | tail -30
  fi
  exit 1
fi

IPA_FILE="$EXPORT_DIR/TCFS.ipa"
echo "    IPA: $IPA_FILE ($(du -h "$IPA_FILE" | cut -f1))"

echo ""
echo "==> Step 6: Uploading to TestFlight..."
env -i \
  HOME="$HOME" \
  USER="$USER" \
  PATH="$CLEAN_PATH" \
  TMPDIR="${TMPDIR:-/tmp}" \
  /usr/bin/xcrun altool --upload-app \
    -f "$IPA_FILE" \
    -t ios \
    --apiKey ZV65L9B864 \
    --apiIssuer d5db1c0a-0a82-4a50-9490-7d86be080506

echo ""
echo "==> Done! Check App Store Connect for TestFlight processing."
echo "    https://appstoreconnect.apple.com"
