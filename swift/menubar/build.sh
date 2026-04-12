#!/usr/bin/env bash
# Build TCFSStatus.app — menu bar conflict monitor for TCFS.
# Usage: cd swift/menubar && bash build.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SOURCES="$SCRIPT_DIR/Sources"
RESOURCES="$SCRIPT_DIR/resources"
BUILD_DIR="$SCRIPT_DIR/build"
APP_DIR="$BUILD_DIR/TCFSStatus.app"

echo "==> Cleaning previous build"
rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS"
mkdir -p "$APP_DIR/Contents/Resources"

# Use Xcode SDK directly — Nix may override xcrun to return its own SDK
# which is too old for the system Swift compiler.
MACOS_SDK="/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk"
if [ ! -d "$MACOS_SDK" ]; then
    MACOS_SDK="$(find /Applications/Xcode*.app -path '*/MacOSX.platform/Developer/SDKs/MacOSX.sdk' -maxdepth 6 2>/dev/null | head -1)"
fi
if [ ! -d "$MACOS_SDK" ]; then
    echo "ERROR: Cannot find macOS SDK. Install Xcode or Command Line Tools." >&2
    exit 1
fi

echo "==> Using SDK: $MACOS_SDK"
echo "==> Compiling Swift sources"
/usr/bin/swiftc \
    -O \
    -target arm64-apple-macosx15.0 \
    -sdk "$MACOS_SDK" \
    -framework AppKit \
    -framework SwiftUI \
    -framework UserNotifications \
    -parse-as-library \
    "$SOURCES"/TCFSStatusApp.swift \
    "$SOURCES"/ConflictMonitor.swift \
    "$SOURCES"/ConflictState.swift \
    "$SOURCES"/NotificationManager.swift \
    "$SOURCES"/ResolutionService.swift \
    "$SOURCES"/MenuBarView.swift \
    -o "$APP_DIR/Contents/MacOS/TCFSStatus"

echo "==> Copying resources"
cp "$RESOURCES/Info.plist" "$APP_DIR/Contents/Info.plist"

echo "==> Built: $APP_DIR"
echo "    Run: open $APP_DIR"
