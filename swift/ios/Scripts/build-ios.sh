#!/usr/bin/env bash
# Build script for TCFS iOS FileProvider extension.
# Cross-compiles Rust staticlib for iOS, generates UniFFI Swift bindings,
# then type-checks Swift sources and optionally builds via xcodebuild.
#
# Usage:
#   ./build-ios.sh [--simulator] [--release] [--typecheck-only]
#
# Targets:
#   Default:         aarch64-apple-ios (physical device)
#   --simulator:     aarch64-apple-ios-sim (Apple Silicon simulator)
#   --typecheck-only: Skip Rust build, just validate Swift sources compile

set -euo pipefail

# Ensure Rust toolchain is on PATH
if [ -f "$HOME/.cargo/env" ]; then
    # shellcheck source=/dev/null
    . "$HOME/.cargo/env"
fi

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
IOS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$IOS_DIR/../.." && pwd)"

SIMULATOR=false
PROFILE="debug"
TYPECHECK_ONLY=false

for arg in "$@"; do
    case "$arg" in
        --simulator)      SIMULATOR=true ;;
        --release)        PROFILE="release" ;;
        --typecheck-only) TYPECHECK_ONLY=true ;;
    esac
done

if $SIMULATOR; then
    RUST_TARGET="aarch64-apple-ios-sim"
    XCODE_SDK="iphonesimulator"
    SWIFT_TARGET="arm64-apple-ios17.0-simulator"
else
    RUST_TARGET="aarch64-apple-ios"
    XCODE_SDK="iphoneos"
    SWIFT_TARGET="arm64-apple-ios17.0"
fi

SDKPATH="$(xcrun --sdk "$XCODE_SDK" --show-sdk-path)"

echo "==> Building TCFS iOS FileProvider"
echo "    Target:  $RUST_TARGET"
echo "    Profile: $PROFILE"
echo "    SDK:     $SDKPATH"

if $TYPECHECK_ONLY; then
    echo "==> Type-checking Swift sources (no Rust build)..."

    echo "    Extension sources..."
    /usr/bin/swiftc \
        -sdk "$SDKPATH" \
        -target "$SWIFT_TARGET" \
        -parse-as-library \
        -typecheck \
        -framework FileProvider \
        -framework Foundation \
        -import-objc-header "$IOS_DIR/Generated/tcfs_file_providerFFI.h" \
        "$IOS_DIR/Generated/tcfs_file_provider.swift" \
        "$IOS_DIR/Extension/"*.swift

    echo "    Host app sources..."
    /usr/bin/swiftc \
        -sdk "$SDKPATH" \
        -target "$SWIFT_TARGET" \
        -parse-as-library \
        -typecheck \
        -framework FileProvider \
        -framework Foundation \
        -framework SwiftUI \
        "$IOS_DIR/HostApp/"*.swift

    echo "==> Type-check passed"
    exit 0
fi

CARGO_FLAGS="--target $RUST_TARGET --features uniffi"
if [ "$PROFILE" = "release" ]; then
    CARGO_FLAGS="$CARGO_FLAGS --release"
fi

RUST_LIB_DIR="$REPO_ROOT/target/$RUST_TARGET/$PROFILE"

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

# --- Step 4: Type-check Swift sources ---
echo "==> Type-checking Swift sources against iOS SDK..."

/usr/bin/swiftc \
    -sdk "$SDKPATH" \
    -target "$SWIFT_TARGET" \
    -parse-as-library \
    -typecheck \
    -framework FileProvider \
    -framework Foundation \
    -import-objc-header "$GENERATED_DIR/tcfs_file_providerFFI.h" \
    "$GENERATED_DIR/tcfs_file_provider.swift" \
    "$IOS_DIR/Extension/"*.swift

/usr/bin/swiftc \
    -sdk "$SDKPATH" \
    -target "$SWIFT_TARGET" \
    -parse-as-library \
    -typecheck \
    -framework FileProvider \
    -framework Foundation \
    -framework SwiftUI \
    "$IOS_DIR/HostApp/"*.swift

echo "    Type-check passed"

# --- Step 5: Build with xcodebuild (if project exists) ---
XCODEPROJ="$IOS_DIR/TCFS.xcodeproj"
if [ ! -d "$XCODEPROJ" ]; then
    # Try xcodegen if available
    if command -v xcodegen &>/dev/null && [ -f "$IOS_DIR/project.yml" ]; then
        echo "==> Generating Xcode project with xcodegen..."
        cd "$IOS_DIR"
        xcodegen generate
    else
        echo "==> Xcode project not found. Rust staticlib + Swift bindings ready at:"
        echo "      $STATICLIB"
        echo "      $GENERATED_DIR/"
        echo ""
        echo "    To generate project: brew install xcodegen && cd swift/ios && xcodegen"
        echo "    Or create manually in Xcode."
        exit 0
    fi
fi

echo "==> Building with xcodebuild..."
XCODE_DEST="platform=iOS Simulator,name=iPhone 16,OS=latest"
if ! $SIMULATOR; then
    XCODE_DEST="generic/platform=iOS"
fi

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
