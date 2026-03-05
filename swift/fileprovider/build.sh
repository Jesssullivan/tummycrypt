#!/usr/bin/env bash
# Build script for TCFS FileProvider extension.
# Compiles Swift sources, links Rust staticlib, assembles .appex bundle.
#
# Usage:
#   ./build.sh <rust_lib_dir> <rust_header_path> <output_dir> [signing_identity]
#
# Examples:
#   ./build.sh target/release include/tcfs_file_provider.h build/
#   ./build.sh target/release include/tcfs_file_provider.h build/ "Developer ID Application: ..."
#   ./build.sh target/release include/tcfs_file_provider.h build/ auto
#
# Signing identity:
#   - omitted or "-":   ad-hoc signing (development)
#   - "auto":           auto-detect Developer ID Application from Keychain
#   - other string:     use as explicit codesign identity

set -euo pipefail

RUST_LIB_DIR="${1:?Usage: build.sh <rust_lib_dir> <rust_header_path> <output_dir> [signing_identity]}"
RUST_HEADER="${2:?Usage: build.sh <rust_lib_dir> <rust_header_path> <output_dir> [signing_identity]}"
OUTPUT_DIR="${3:?Usage: build.sh <rust_lib_dir> <rust_header_path> <output_dir> [signing_identity]}"
SIGNING_IDENTITY="${4:--}"

# Auto-detect Developer ID from Keychain
if [ "$SIGNING_IDENTITY" = "auto" ]; then
  SIGNING_IDENTITY=$(security find-identity -v -p codesigning | grep "Developer ID Application" | head -1 | sed 's/.*"\(.*\)".*/\1/' || true)
  if [ -z "$SIGNING_IDENTITY" ]; then
    echo "WARNING: No Developer ID Application found in Keychain, falling back to ad-hoc" >&2
    SIGNING_IDENTITY="-"
  else
    echo "==> Auto-detected signing identity: $SIGNING_IDENTITY"
  fi
fi

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SDKPATH="$(xcrun --sdk macosx --show-sdk-path)"
VERSION="0.1.0"
MIN_MACOS="15.0"

echo "==> Building TCFS FileProvider extension"
echo "    Rust lib:   $RUST_LIB_DIR"
echo "    Header:     $RUST_HEADER"
echo "    SDK:        $SDKPATH"
echo "    Output:     $OUTPUT_DIR"

# --- Compile extension binary ---
# The ObjC entry point (extension_main.m) calls NSExtensionMain() which
# handles XPC listener setup, principal class discovery, and main loop.
# Swift sources are compiled with -parse-as-library since the C entry
# point provides main().
echo "==> Compiling FileProvider extension..."

# Compile ObjC entry point
/usr/bin/clang -c \
    -isysroot "$SDKPATH" \
    -target arm64-apple-macos${MIN_MACOS} \
    -fobjc-arc \
    -O2 \
    -o extension_main.o \
    "$SCRIPT_DIR/Sources/Extension/extension_main.m"

# Compile Swift sources + link everything
/usr/bin/swiftc \
    -sdk "$SDKPATH" \
    -parse-as-library \
    -framework FileProvider \
    -framework Foundation \
    -target arm64-apple-macos${MIN_MACOS} \
    -import-objc-header "$RUST_HEADER" \
    -Xlinker -all_load \
    -L "$RUST_LIB_DIR" -ltcfs_file_provider \
    -lc++ \
    -O \
    -o TCFSFileProvider \
    "$SCRIPT_DIR/Sources/Extension/"*.swift \
    extension_main.o

# --- Compile host app binary ---
echo "==> Compiling host app..."
/usr/bin/swiftc \
    -sdk "$SDKPATH" \
    -parse-as-library \
    -framework FileProvider \
    -framework Foundation \
    -target arm64-apple-macos${MIN_MACOS} \
    -O \
    -o TCFSProvider \
    "$SCRIPT_DIR/Sources/HostApp/"*.swift

# --- Assemble bundle ---
echo "==> Assembling bundle..."
APP="$OUTPUT_DIR/TCFSProvider.app/Contents"
EXT="$APP/Extensions/TCFSFileProvider.appex/Contents"

mkdir -p "$APP/MacOS" "$EXT/MacOS"

cp TCFSProvider "$APP/MacOS/"
cp "$SCRIPT_DIR/resources/HostApp-Info.plist" "$APP/Info.plist"

cp TCFSFileProvider "$EXT/MacOS/"
cp "$SCRIPT_DIR/resources/Extension-Info.plist" "$EXT/Info.plist"

# --- Resolve entitlements (expand TeamID for keychain-access-groups) ---
echo "==> Resolving entitlements..."
EXT_ENTITLEMENTS="$SCRIPT_DIR/resources/Extension.entitlements"
HOST_ENTITLEMENTS="$SCRIPT_DIR/resources/HostApp.entitlements"

if [ "$SIGNING_IDENTITY" != "-" ]; then
    # Extract TeamID from the signing certificate.
    TEAM_ID=$(/usr/bin/security find-identity -v -p codesigning \
        | grep "$SIGNING_IDENTITY" \
        | head -1 \
        | sed -E 's/.*\(([A-Z0-9]{10})\).*/\1/')
    if [ -n "$TEAM_ID" ]; then
        echo "    TeamID: $TEAM_ID"
        # Generate entitlements with resolved keychain-access-groups
        EXT_ENTITLEMENTS="/tmp/tcfs-ext-entitlements.$$.plist"
        HOST_ENTITLEMENTS="/tmp/tcfs-host-entitlements.$$.plist"
        sed "s/\$(AppIdentifierPrefix)/${TEAM_ID}./g" \
            "$SCRIPT_DIR/resources/Extension.entitlements" > "$EXT_ENTITLEMENTS"
        sed "s/\$(AppIdentifierPrefix)/${TEAM_ID}./g" \
            "$SCRIPT_DIR/resources/HostApp.entitlements" > "$HOST_ENTITLEMENTS"
    else
        echo "    WARNING: Could not extract TeamID, using entitlements as-is"
    fi
fi

# --- Code sign (inside-out: extension first, then host app) ---
echo "==> Signing..."
if [ "$SIGNING_IDENTITY" != "-" ]; then
    echo "    Identity: $SIGNING_IDENTITY (Developer ID)"
    /usr/bin/codesign -f -s "$SIGNING_IDENTITY" \
        --options runtime --timestamp \
        --entitlements "$EXT_ENTITLEMENTS" \
        "$APP/Extensions/TCFSFileProvider.appex"

    /usr/bin/codesign -f -s "$SIGNING_IDENTITY" \
        --options runtime --timestamp \
        --entitlements "$HOST_ENTITLEMENTS" \
        "$OUTPUT_DIR/TCFSProvider.app"
else
    echo "    Identity: ad-hoc (development)"
    /usr/bin/codesign -f -s - \
        --entitlements "$EXT_ENTITLEMENTS" \
        "$APP/Extensions/TCFSFileProvider.appex"

    /usr/bin/codesign -f -s - \
        --entitlements "$HOST_ENTITLEMENTS" \
        "$OUTPUT_DIR/TCFSProvider.app"
fi

# --- Cleanup temp binaries ---
rm -f TCFSFileProvider TCFSProvider extension_main.o

echo "==> Done: $OUTPUT_DIR/TCFSProvider.app"
