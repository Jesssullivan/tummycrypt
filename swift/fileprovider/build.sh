#!/usr/bin/env bash
# Build script for TCFS FileProvider extension.
# Compiles Swift sources, links Rust staticlib, assembles .appex bundle.
#
# Usage:
#   ./build.sh <rust_lib_dir> <rust_header_path> <output_dir>
#
# Example:
#   ./build.sh target/release include/tcfs_file_provider.h build/

set -euo pipefail

RUST_LIB_DIR="${1:?Usage: build.sh <rust_lib_dir> <rust_header_path> <output_dir>}"
RUST_HEADER="${2:?Usage: build.sh <rust_lib_dir> <rust_header_path> <output_dir>}"
OUTPUT_DIR="${3:?Usage: build.sh <rust_lib_dir> <rust_header_path> <output_dir>}"

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
# NOTE: no -parse-as-library — main.swift provides the entry point
echo "==> Compiling FileProvider extension..."
/usr/bin/swiftc \
    -sdk "$SDKPATH" \
    -framework FileProvider \
    -framework Foundation \
    -target arm64-apple-macos${MIN_MACOS} \
    -import-objc-header "$RUST_HEADER" \
    -Xlinker -all_load \
    -L "$RUST_LIB_DIR" -ltcfs_file_provider \
    -lc++ \
    -O \
    -o TCFSFileProvider \
    "$SCRIPT_DIR/Sources/Extension/"*.swift

# --- Compile host app binary ---
echo "==> Compiling host app..."
/usr/bin/swiftc \
    -sdk "$SDKPATH" \
    -parse-as-library \
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

# --- Code sign (inside-out) ---
echo "==> Signing..."
/usr/bin/codesign -f -s - \
    --entitlements "$SCRIPT_DIR/resources/Extension.entitlements" \
    "$APP/Extensions/TCFSFileProvider.appex"

/usr/bin/codesign -f -s - \
    --entitlements "$SCRIPT_DIR/resources/HostApp.entitlements" \
    "$OUTPUT_DIR/TCFSProvider.app"

# --- Cleanup temp binaries ---
rm -f TCFSFileProvider TCFSProvider

echo "==> Done: $OUTPUT_DIR/TCFSProvider.app"
