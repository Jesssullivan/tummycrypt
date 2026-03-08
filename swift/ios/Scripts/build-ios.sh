#!/usr/bin/env bash
# Build script for TCFS iOS FileProvider extension.
# Cross-compiles Rust staticlib for iOS, generates UniFFI Swift bindings,
# then builds the iOS host app + extension via xcodebuild.
#
# Usage:
#   ./build-ios.sh [--simulator] [--release]
#
# Targets:
#   Default:     aarch64-apple-ios (physical device)
#   --simulator: aarch64-apple-ios-sim (Apple Silicon simulator)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
IOS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$IOS_DIR/../.." && pwd)"

SIMULATOR=false
PROFILE="debug"

for arg in "$@"; do
    case "$arg" in
        --simulator) SIMULATOR=true ;;
        --release)   PROFILE="release" ;;
    esac
done

if $SIMULATOR; then
    RUST_TARGET="aarch64-apple-ios-sim"
    XCODE_SDK="iphonesimulator"
    XCODE_DEST="platform=iOS Simulator,name=iPhone 16,OS=latest"
else
    RUST_TARGET="aarch64-apple-ios"
    XCODE_SDK="iphoneos"
    XCODE_DEST="generic/platform=iOS"
fi

CARGO_FLAGS="--target $RUST_TARGET --features uniffi"
if [ "$PROFILE" = "release" ]; then
    CARGO_FLAGS="$CARGO_FLAGS --release"
fi

RUST_LIB_DIR="$REPO_ROOT/target/$RUST_TARGET/$PROFILE"

echo "==> Building TCFS iOS FileProvider"
echo "    Target:  $RUST_TARGET"
echo "    Profile: $PROFILE"
echo "    SDK:     $XCODE_SDK"

# --- Step 1: Install Rust target if needed ---
if ! rustup target list --installed | grep -q "$RUST_TARGET"; then
    echo "==> Installing Rust target: $RUST_TARGET"
    rustup target add "$RUST_TARGET"
fi

# --- Step 2: Build Rust staticlib ---
echo "==> Building Rust staticlib..."
cd "$REPO_ROOT"
# shellcheck disable=SC2086
cargo build -p tcfs-file-provider $CARGO_FLAGS

STATICLIB="$RUST_LIB_DIR/libtcfs_file_provider.a"
if [ ! -f "$STATICLIB" ]; then
    echo "ERROR: staticlib not found at $STATICLIB" >&2
    exit 1
fi
echo "    Staticlib: $STATICLIB ($(du -h "$STATICLIB" | cut -f1))"

# --- Step 3: Generate UniFFI Swift bindings ---
echo "==> Generating UniFFI Swift bindings..."
GENERATED_DIR="$IOS_DIR/Generated"
mkdir -p "$GENERATED_DIR"

cargo run -p tcfs-file-provider --features uniffi --bin uniffi-bindgen -- \
    generate --library "$STATICLIB" \
    --language swift \
    --out-dir "$GENERATED_DIR"

echo "    Generated: $(ls "$GENERATED_DIR"/*.swift 2>/dev/null | wc -l | tr -d ' ') Swift files"

# --- Step 4: Generate Xcode project if needed ---
XCODEPROJ="$IOS_DIR/TCFS.xcodeproj"
if [ ! -d "$XCODEPROJ" ]; then
    echo "==> Xcode project not found at $XCODEPROJ"
    echo "    Create it in Xcode: File > New > Project > App"
    echo "    Then add a File Provider extension target."
    echo ""
    echo "    Alternatively, build manually with swiftc (see below)."
    echo ""
    echo "==> Manual build (no Xcode project):"
    echo "    The staticlib and Swift bindings are ready at:"
    echo "      $STATICLIB"
    echo "      $GENERATED_DIR/"
    echo ""
    echo "    Link against the staticlib and include the generated Swift files"
    echo "    in your Xcode project's extension target."
    exit 0
fi

# --- Step 5: Build with xcodebuild ---
echo "==> Building with xcodebuild..."
xcodebuild build \
    -project "$XCODEPROJ" \
    -scheme "TCFS" \
    -sdk "$XCODE_SDK" \
    -destination "$XCODE_DEST" \
    -configuration "$([ "$PROFILE" = "release" ] && echo Release || echo Debug)" \
    LIBRARY_SEARCH_PATHS="$RUST_LIB_DIR" \
    OTHER_LDFLAGS="-ltcfs_file_provider -lc++" \
    SWIFT_INCLUDE_PATHS="$GENERATED_DIR" \
    2>&1 | tail -20

echo "==> Done"
