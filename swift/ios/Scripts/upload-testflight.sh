#!/usr/bin/env bash
# Upload TCFS iOS app to TestFlight.
#
# Prerequisites:
#   1. Apple Developer Program membership (developer.apple.com)
#   2. App IDs registered: io.tinyland.tcfs.ios + io.tinyland.tcfs.ios.fileprovider
#   3. App Group: group.io.tinyland.tcfs
#   4. Keychain Access Group configured in both App IDs
#   5. Apple Distribution certificate in Keychain (or Xcode auto-manages)
#   6. App created in App Store Connect (appstoreconnect.apple.com)
#
# Usage:
#   ./upload-testflight.sh [--api-key <key_id> --api-issuer <issuer_id>]
#
# Authentication:
#   Either pass --api-key/--api-issuer for App Store Connect API key,
#   or ensure you're signed into Xcode with your Apple ID.

set -euo pipefail

if [ -f "$HOME/.cargo/env" ]; then
    # shellcheck source=/dev/null
    . "$HOME/.cargo/env"
fi

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
IOS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$IOS_DIR/../.." && pwd)"

API_KEY=""
API_ISSUER=""

for arg in "$@"; do
    case "$arg" in
        --api-key)    shift; API_KEY="$1"; shift ;;
        --api-issuer) shift; API_ISSUER="$1"; shift ;;
    esac
done

RUST_TARGET="aarch64-apple-ios"
PROFILE="release"
RUST_LIB_DIR="$REPO_ROOT/target/$RUST_TARGET/$PROFILE"

echo "==> Building TCFS for TestFlight"
echo "    Target:  $RUST_TARGET"
echo "    Profile: $PROFILE"

# --- Step 1: Build Rust staticlib (release) ---
echo "==> Building Rust staticlib (release)..."
cd "$REPO_ROOT"
if ! rustup target list --installed | grep -q "$RUST_TARGET"; then
    rustup target add "$RUST_TARGET"
fi
cargo build -p tcfs-file-provider --target "$RUST_TARGET" --features uniffi --release

STATICLIB="$RUST_LIB_DIR/libtcfs_file_provider.a"
if [ ! -f "$STATICLIB" ]; then
    echo "ERROR: staticlib not found at $STATICLIB" >&2
    exit 1
fi
echo "    Staticlib: $STATICLIB ($(du -h "$STATICLIB" | cut -f1))"

# --- Step 2: Generate UniFFI Swift bindings ---
echo "==> Generating UniFFI Swift bindings..."
GENERATED_DIR="$IOS_DIR/Generated"
mkdir -p "$GENERATED_DIR"

cargo run -p tcfs-file-provider --features uniffi --bin uniffi-bindgen -- \
    generate --library "$STATICLIB" \
    --language swift \
    --out-dir "$GENERATED_DIR"

# --- Step 3: Generate Xcode project ---
echo "==> Generating Xcode project..."
cd "$IOS_DIR"
if command -v xcodegen &>/dev/null; then
    xcodegen generate
else
    echo "ERROR: xcodegen not found. Install with: brew install xcodegen" >&2
    exit 1
fi

# --- Step 4: Archive ---
echo "==> Archiving..."
ARCHIVE_PATH="$IOS_DIR/build/TCFS.xcarchive"

xcodebuild archive \
    -project "$IOS_DIR/TCFS.xcodeproj" \
    -scheme "TCFS" \
    -sdk iphoneos \
    -configuration Release \
    -archivePath "$ARCHIVE_PATH" \
    LIBRARY_SEARCH_PATHS="$RUST_LIB_DIR" \
    ONLY_ACTIVE_ARCH=NO \
    SKIP_INSTALL=NO \
    BUILD_LIBRARY_FOR_DISTRIBUTION=YES

echo "    Archive: $ARCHIVE_PATH"

# --- Step 5: Export IPA ---
echo "==> Exporting IPA..."
EXPORT_DIR="$IOS_DIR/build/export"
EXPORT_OPTIONS="$IOS_DIR/Scripts/ExportOptions.plist"

if [ ! -f "$EXPORT_OPTIONS" ]; then
    echo "ERROR: ExportOptions.plist not found at $EXPORT_OPTIONS" >&2
    echo "Create it with method=app-store and teamID=QP994XQKNH" >&2
    exit 1
fi

xcodebuild -exportArchive \
    -archivePath "$ARCHIVE_PATH" \
    -exportPath "$EXPORT_DIR" \
    -exportOptionsPlist "$EXPORT_OPTIONS"

IPA_FILE="$EXPORT_DIR/TCFS.ipa"
echo "    IPA: $IPA_FILE ($(du -h "$IPA_FILE" | cut -f1))"

# --- Step 6: Upload to TestFlight ---
echo "==> Uploading to TestFlight..."

if [ -n "$API_KEY" ] && [ -n "$API_ISSUER" ]; then
    xcrun altool --upload-app \
        -f "$IPA_FILE" \
        -t ios \
        --apiKey "$API_KEY" \
        --apiIssuer "$API_ISSUER"
else
    xcrun altool --upload-app \
        -f "$IPA_FILE" \
        -t ios \
        -u "${APPLE_ID:-}" \
        -p "${APPLE_APP_SPECIFIC_PASSWORD:-}"
fi

echo "==> Done! Check App Store Connect for TestFlight processing."
echo "    https://appstoreconnect.apple.com"
