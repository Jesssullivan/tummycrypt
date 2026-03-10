#!/usr/bin/env bash
# Build Rust staticlib + UniFFI Swift bindings for iOS.
#
# This script is designed to run INSIDE a nix devshell (for cargo/rustc).
# It resolves Xcode SDK paths explicitly to avoid nix SDKROOT pollution.
#
# Usage:
#   nix develop -c bash swift/ios/Scripts/build-rust.sh
#   # or, if already in devshell:
#   ./Scripts/build-rust.sh
#
# Flags:
#   --force    Rebuild even if staticlib exists
#   --clean    Remove debug artifacts first (saves disk on PZM)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=_lib.sh
source "$SCRIPT_DIR/_lib.sh"

FORCE=false
CLEAN=false
for arg in "$@"; do
    case "$arg" in
        --force) FORCE=true ;;
        --clean) CLEAN=true ;;
    esac
done

echo "==> Rust Staticlib + UniFFI Bindings"
echo "    Target:    $RUST_TARGET"
echo "    Staticlib: $STATICLIB"

# ── Clean debug artifacts if requested (saves ~2GB on PZM) ───────────────
if $CLEAN; then
    for d in "$REPO_ROOT/target/debug" "$REPO_ROOT/target/$RUST_TARGET/debug"; do
        if [ -d "$d" ]; then
            echo "    Cleaning $d..."
            rm -rf "$d"
        fi
    done
fi

# ── Skip if already built (unless --force) ───────────────────────────────
if [ -f "$STATICLIB" ] && ! $FORCE; then
    echo "    Staticlib exists ($(du -h "$STATICLIB" | cut -f1)), skipping build."
    echo "    Use --force to rebuild."
else
    # Resolve Xcode paths using system xcrun (not nix-wrapped)
    XCRUN="env -u SDKROOT DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer /usr/bin/xcrun"
    IOS_SDK=$($XCRUN --sdk iphoneos --show-sdk-path)
    XCODE_CLANG=$($XCRUN --find clang)
    AR_PATH=$($XCRUN --find ar)

    echo "    iOS SDK: $IOS_SDK"
    echo "    Clang:   $XCODE_CLANG"

    cd "$REPO_ROOT"

    export SDKROOT="$IOS_SDK"
    export CC_aarch64_apple_ios="$XCODE_CLANG"
    export AR_aarch64_apple_ios="$AR_PATH"
    export CFLAGS_aarch64_apple_ios="--target=arm64-apple-ios -isysroot $IOS_SDK"

    echo "    Building (this may take a few minutes)..."
    cargo build -p tcfs-file-provider --target "$RUST_TARGET" --features uniffi --release

    if [ ! -f "$STATICLIB" ]; then
        echo "ERROR: Staticlib not found after build at $STATICLIB" >&2
        exit 1
    fi
    echo "    Built: $(du -h "$STATICLIB" | cut -f1)"
fi

# ── Generate UniFFI Swift bindings ───────────────────────────────────────
echo "==> Generating UniFFI Swift bindings..."
mkdir -p "$GENERATED_DIR"
cd "$REPO_ROOT"

cargo run -p tcfs-file-provider --features uniffi --bin uniffi-bindgen -- \
    generate --library "$STATICLIB" \
    --language swift \
    --out-dir "$GENERATED_DIR"

echo "    Generated: $(ls "$GENERATED_DIR"/*.swift 2>/dev/null | wc -l | tr -d ' ') Swift files"
echo "==> Rust build complete"
