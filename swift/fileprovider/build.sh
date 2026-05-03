#!/usr/bin/env bash
# Build script for the experimental TCFS macOS FileProvider extension.
# Compiles Swift sources, links Rust staticlib, assembles .appex bundle.
#
# Usage:
#   ./build.sh <rust_lib_dir> <rust_header_path> <output_dir> [signing_identity]
#
# Examples:
#   ./build.sh target/release include/tcfs_file_provider.h build/
#   ./build.sh target/release include/tcfs_file_provider.h build/ "Developer ID Application: ..."
#   ./build.sh target/release include/tcfs_file_provider.h build/ auto
#   ./build.sh target/release include/tcfs_file_provider.h build/ auto-development
#
# Signing identity:
#   - omitted or "-":   ad-hoc signing (development)
#   - "auto":           auto-detect Developer ID Application from Keychain
#   - "auto-development": auto-detect Apple Development from Keychain
#   - other string:     use as explicit codesign identity

set -euo pipefail

RUST_LIB_DIR="${1:?Usage: build.sh <rust_lib_dir> <rust_header_path> <output_dir> [signing_identity]}"
RUST_HEADER="${2:?Usage: build.sh <rust_lib_dir> <rust_header_path> <output_dir> [signing_identity]}"
OUTPUT_DIR="${3:?Usage: build.sh <rust_lib_dir> <rust_header_path> <output_dir> [signing_identity]}"
SIGNING_IDENTITY="${4:--}"
HOST_PROVISIONING_PROFILE="${TCFS_HOST_PROVISIONING_PROFILE:-${TCFS_PROVISIONING_PROFILE:-}}"
EXTENSION_PROVISIONING_PROFILE="${TCFS_EXTENSION_PROVISIONING_PROFILE:-${TCFS_FILEPROVIDER_PROVISIONING_PROFILE:-${TCFS_PROVISIONING_PROFILE:-}}}"
AUTO_PROVISIONING_PROFILES="${TCFS_AUTO_PROVISIONING_PROFILES:-0}"
EMBED_FILEPROVIDER_CONFIG="${TCFS_EMBED_FILEPROVIDER_CONFIG:-}"
FILEPROVIDER_TESTING_MODE_ENTITLEMENT="${TCFS_FILEPROVIDER_TESTING_MODE_ENTITLEMENT:-0}"
CODESIGN_KEYCHAIN="${TCFS_CODESIGN_KEYCHAIN:-}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

SYSTEM_XCRUN="${TCFS_XCRUN:-/usr/bin/xcrun}"
if [ ! -x "$SYSTEM_XCRUN" ]; then
    SYSTEM_XCRUN="$(command -v xcrun)"
fi

if [ -n "${TCFS_XCODE_DEVELOPER_DIR:-}" ]; then
    XCODE_DEVELOPER_DIR="$TCFS_XCODE_DEVELOPER_DIR"
elif [ -d "/Applications/Xcode.app/Contents/Developer" ]; then
    XCODE_DEVELOPER_DIR="/Applications/Xcode.app/Contents/Developer"
else
    XCODE_DEVELOPER_DIR=""
fi

xcrun_xcode() {
    if [ -n "$XCODE_DEVELOPER_DIR" ]; then
        env -u SDKROOT DEVELOPER_DIR="$XCODE_DEVELOPER_DIR" "$SYSTEM_XCRUN" "$@"
    else
        env -u SDKROOT -u DEVELOPER_DIR "$SYSTEM_XCRUN" "$@"
    fi
}

if [ -n "$XCODE_DEVELOPER_DIR" ]; then
    export DEVELOPER_DIR="$XCODE_DEVELOPER_DIR"
else
    unset DEVELOPER_DIR
fi
unset SDKROOT

SECURITY_IDENTITY_ARGS=()
CODESIGN_KEYCHAIN_ARGS=()
if [ -n "$CODESIGN_KEYCHAIN" ]; then
    if [ ! -f "$CODESIGN_KEYCHAIN" ]; then
        echo "ERROR: TCFS_CODESIGN_KEYCHAIN does not exist: $CODESIGN_KEYCHAIN" >&2
        exit 1
    fi
    SECURITY_IDENTITY_ARGS=("$CODESIGN_KEYCHAIN")
    CODESIGN_KEYCHAIN_ARGS=(--keychain "$CODESIGN_KEYCHAIN")
fi

# Auto-detect a signing identity from Keychain.
if [ "$SIGNING_IDENTITY" = "auto" ] || [ "$SIGNING_IDENTITY" = "auto-development" ]; then
  if [ "$SIGNING_IDENTITY" = "auto-development" ]; then
    IDENTITY_PATTERN="Apple Development"
  else
    IDENTITY_PATTERN="Developer ID Application"
  fi

  SIGNING_IDENTITY=$(security find-identity -v -p codesigning "${SECURITY_IDENTITY_ARGS[@]}" | grep "$IDENTITY_PATTERN" | head -1 | sed 's/.*"\(.*\)".*/\1/' || true)
  if [ -z "$SIGNING_IDENTITY" ]; then
    echo "WARNING: No $IDENTITY_PATTERN identity found in Keychain, falling back to ad-hoc" >&2
    SIGNING_IDENTITY="-"
  else
    echo "==> Auto-detected signing identity: $SIGNING_IDENTITY"
  fi
fi

if [ "$SIGNING_IDENTITY" != "-" ] && [ "$AUTO_PROVISIONING_PROFILES" = "1" ] && {
    [ -z "$HOST_PROVISIONING_PROFILE" ] || [ -z "$EXTENSION_PROVISIONING_PROFILE" ]
}; then
    echo "==> Auto-detecting TCFS provisioning profiles..."
    PROFILE_ENV="$(bash "$REPO_ROOT/scripts/macos-fileprovider-profile-inventory.sh" --env-only --strict)"
    eval "$PROFILE_ENV"
    HOST_PROVISIONING_PROFILE="${TCFS_HOST_PROVISIONING_PROFILE:-${TCFS_PROVISIONING_PROFILE:-}}"
    EXTENSION_PROVISIONING_PROFILE="${TCFS_EXTENSION_PROVISIONING_PROFILE:-${TCFS_FILEPROVIDER_PROVISIONING_PROFILE:-${TCFS_PROVISIONING_PROFILE:-}}}"
fi

SDKPATH="$(xcrun_xcode --sdk macosx --show-sdk-path)"
SWIFTC="${TCFS_SWIFTC:-$(xcrun_xcode --find swiftc)}"
CLANG="${TCFS_CLANG:-$(xcrun_xcode --find clang)}"
MIN_MACOS="15.0"

echo "==> Building TCFS FileProvider extension"
echo "    Rust lib:   $RUST_LIB_DIR"
echo "    Header:     $RUST_HEADER"
echo "    SDK:        $SDKPATH"
if [ -n "$XCODE_DEVELOPER_DIR" ]; then
    echo "    Xcode:      $XCODE_DEVELOPER_DIR"
fi
echo "    Swift:      $SWIFTC"
echo "    Output:     $OUTPUT_DIR"

if [ "${TCFS_CODESIGN_TIMESTAMP:-1}" = "0" ]; then
    CODESIGN_TIMESTAMP_ARG="--timestamp=none"
else
    CODESIGN_TIMESTAMP_ARG="--timestamp"
fi

if [ -z "$EMBED_FILEPROVIDER_CONFIG" ]; then
    if [ "${TCFS_REQUIRE_PRODUCTION_SIGNING:-0}" = "1" ]; then
        EMBED_FILEPROVIDER_CONFIG=0
    else
        EMBED_FILEPROVIDER_CONFIG=1
    fi
fi

case "$EMBED_FILEPROVIDER_CONFIG" in
    1|true|yes|on) EMBED_FILEPROVIDER_CONFIG=1 ;;
    0|false|no|off) EMBED_FILEPROVIDER_CONFIG=0 ;;
    *)
        echo "ERROR: TCFS_EMBED_FILEPROVIDER_CONFIG must be 0/1, got: $EMBED_FILEPROVIDER_CONFIG" >&2
        exit 1
        ;;
esac

case "$FILEPROVIDER_TESTING_MODE_ENTITLEMENT" in
    1|true|yes|on) FILEPROVIDER_TESTING_MODE_ENTITLEMENT=1 ;;
    0|false|no|off) FILEPROVIDER_TESTING_MODE_ENTITLEMENT=0 ;;
    *)
        echo "ERROR: TCFS_FILEPROVIDER_TESTING_MODE_ENTITLEMENT must be 0/1, got: $FILEPROVIDER_TESTING_MODE_ENTITLEMENT" >&2
        exit 1
        ;;
esac

if [ "${TCFS_REQUIRE_PRODUCTION_SIGNING:-0}" = "1" ] \
    && [ "$EMBED_FILEPROVIDER_CONFIG" = "1" ] \
    && [ "${TCFS_ALLOW_PRODUCTION_EMBEDDED_CONFIG:-0}" != "1" ]; then
    echo "ERROR: production FileProvider builds must not embed config by default" >&2
    echo "       Set TCFS_EMBED_FILEPROVIDER_CONFIG=0, or set TCFS_ALLOW_PRODUCTION_EMBEDDED_CONFIG=1 only for diagnostic evidence." >&2
    exit 1
fi

# --- Generate embedded config ---
# Diagnostic builds may bake config.json into the extension binary. Production
# signing disables this by default so the Keychain/App Group path is actually
# exercised by Finder/FileProvider acceptance.
CONFIG_PATH="${TCFS_FP_CONFIG:-$HOME/.config/tcfs/fileprovider/config.json}"
mkdir -p "$OUTPUT_DIR"
EMBEDDED_CONFIG_SWIFT="$OUTPUT_DIR/EmbeddedConfig.generated.swift"

if [ "$EMBED_FILEPROVIDER_CONFIG" = "1" ] && [ -f "$CONFIG_PATH" ]; then
    CONFIG_B64=$(base64 < "$CONFIG_PATH" | tr -d '\n')
    cat > "$EMBEDDED_CONFIG_SWIFT" << SWIFT
// Auto-generated by build.sh — DO NOT EDIT
import Foundation
let embeddedConfigBase64: String? = "$CONFIG_B64"
SWIFT
    echo "==> Embedded config from $CONFIG_PATH"
else
    cat > "$EMBEDDED_CONFIG_SWIFT" << SWIFT
// Auto-generated by build.sh — DO NOT EDIT
import Foundation
let embeddedConfigBase64: String? = nil
SWIFT
    if [ "$EMBED_FILEPROVIDER_CONFIG" = "1" ]; then
        echo "WARNING: No config at $CONFIG_PATH — embedded config will be nil" >&2
    else
        echo "==> Embedded config disabled"
    fi
    echo "    Extension will fall back to Keychain/XDG/App Group"
fi

# Validate config has daemon_socket for gRPC backend
if [ "$EMBED_FILEPROVIDER_CONFIG" = "1" ] && [ -f "$CONFIG_PATH" ]; then
    if ! grep -q '"daemon_socket"' "$CONFIG_PATH" 2>/dev/null; then
        echo "WARNING: config.json has no daemon_socket — gRPC backend needs a socket path" >&2
        echo "         Extension will fall back to XDG or App Group container path" >&2
    fi
fi

# --- Compile extension binary ---
# The ObjC entry point (extension_main.m) calls NSExtensionMain() which
# handles XPC listener setup, principal class discovery, and main loop.
# Swift sources are compiled with -parse-as-library since the C entry
# point provides main().
echo "==> Compiling FileProvider extension..."

# Generate combined bridging header (cbindgen + supplementary progress callback)
cat > "$OUTPUT_DIR/tcfs_combined_bridge.h" << BRIDGE
/* Combined bridging header for Swift — auto-generated by build.sh */
#include <stdint.h>
$(cat "$RUST_HEADER")

/* Supplementary: progress callback (cbindgen doesn't export complex fn ptr types) */
typedef void (*TcfsProgressCallback)(uint64_t completed, uint64_t total, const void *context);

enum TcfsError tcfs_provider_fetch_with_progress(
    struct TcfsProvider *provider,
    const char *item_id,
    const char *dest_path,
    TcfsProgressCallback callback,
    const void *callback_context
);

char *tcfs_provider_last_error(struct TcfsProvider *provider);
BRIDGE

# Compile ObjC entry point
"$CLANG" -c \
    -isysroot "$SDKPATH" \
    -target arm64-apple-macos${MIN_MACOS} \
    -fobjc-arc \
    -O2 \
    -o extension_main.o \
    "$SCRIPT_DIR/Sources/Extension/extension_main.m"

# Compile Swift sources + link everything
# -disable-modules-validate-system-headers: work around CLT packaging bug where
# the compiler version (e.g. Swift 6.2.4) is newer than the SDK's pre-built
# Swift module interfaces (e.g. Swift 6.2). Safe for production builds.
"$SWIFTC" \
    -sdk "$SDKPATH" \
    -parse-as-library \
    -framework FileProvider \
    -framework Foundation \
    -target arm64-apple-macos${MIN_MACOS} \
    -import-objc-header "$OUTPUT_DIR/tcfs_combined_bridge.h" \
    -Xfrontend -disable-modules-validate-system-headers \
    -Xlinker -all_load \
    -L "$RUST_LIB_DIR" -ltcfs_file_provider \
    -lc++ \
    -O \
    -o TCFSFileProvider \
    "$SCRIPT_DIR/Sources/Extension/"*.swift \
    "$EMBEDDED_CONFIG_SWIFT" \
    extension_main.o

# --- Compile host app binary ---
echo "==> Compiling host app..."
"$SWIFTC" \
    -sdk "$SDKPATH" \
    -parse-as-library \
    -framework FileProvider \
    -framework Foundation \
    -target arm64-apple-macos${MIN_MACOS} \
    -Xfrontend -disable-modules-validate-system-headers \
    -O \
    -o TCFSProvider \
    "$SCRIPT_DIR/Sources/HostApp/"*.swift

# --- Compile Finder Sync extension (status badges) ---
FINDER_SYNC_DIR="$SCRIPT_DIR/Sources/FinderSync"
if [ -d "$FINDER_SYNC_DIR" ]; then
    echo "==> Compiling Finder Sync extension..."

    # FinderSync is an AppKit-based in-process plugin (NOT an XPC service).
    # Uses NSApplicationMain entry point (NOT NSExtensionMain).
    "$CLANG" -c \
        -isysroot "$SDKPATH" \
        -target arm64-apple-macos${MIN_MACOS} \
        -fobjc-arc \
        -O2 \
        -o finder_sync_main.o \
        "$FINDER_SYNC_DIR/finder_sync_main.m"

    "$SWIFTC" \
        -sdk "$SDKPATH" \
        -parse-as-library \
        -framework FinderSync \
        -framework Foundation \
        -framework AppKit \
        -target arm64-apple-macos${MIN_MACOS} \
        -Xfrontend -disable-modules-validate-system-headers \
        -O \
        -o TCFSFinderSync \
        "$FINDER_SYNC_DIR/"*.swift \
        finder_sync_main.o
    HAVE_FINDER_SYNC=true
else
    echo "==> No FinderSync sources found, skipping"
    HAVE_FINDER_SYNC=false
fi

# --- Assemble bundle ---
echo "==> Assembling bundle..."
rm -rf "$OUTPUT_DIR/TCFSProvider.app"
APP="$OUTPUT_DIR/TCFSProvider.app/Contents"
EXT="$APP/Extensions/TCFSFileProvider.appex/Contents"

mkdir -p "$APP/MacOS" "$EXT/MacOS"

cp TCFSProvider "$APP/MacOS/"
cp "$SCRIPT_DIR/resources/HostApp-Info.plist" "$APP/Info.plist"

cp TCFSFileProvider "$EXT/MacOS/"
cp "$SCRIPT_DIR/resources/Extension-Info.plist" "$EXT/Info.plist"

if [ "$SIGNING_IDENTITY" != "-" ]; then
    if [ -n "$HOST_PROVISIONING_PROFILE" ]; then
        if [ ! -f "$HOST_PROVISIONING_PROFILE" ]; then
            echo "ERROR: host provisioning profile not found: $HOST_PROVISIONING_PROFILE" >&2
            exit 1
        fi
        cp "$HOST_PROVISIONING_PROFILE" "$APP/embedded.provisionprofile"
        echo "==> Embedded host provisioning profile"
    else
        echo "WARNING: no host provisioning profile provided; restricted entitlements may fail at launch" >&2
    fi

    if [ -n "$EXTENSION_PROVISIONING_PROFILE" ]; then
        if [ ! -f "$EXTENSION_PROVISIONING_PROFILE" ]; then
            echo "ERROR: extension provisioning profile not found: $EXTENSION_PROVISIONING_PROFILE" >&2
            exit 1
        fi
        cp "$EXTENSION_PROVISIONING_PROFILE" "$EXT/embedded.provisionprofile"
        echo "==> Embedded extension provisioning profile"
    else
        echo "WARNING: no extension provisioning profile provided; restricted entitlements may fail at launch" >&2
    fi
fi

# Finder Sync extension (if compiled)
if [ "$HAVE_FINDER_SYNC" = "true" ]; then
    FSEXT="$APP/Extensions/TCFSFinderSync.appex/Contents"
    mkdir -p "$FSEXT/MacOS"
    cp TCFSFinderSync "$FSEXT/MacOS/"
    cp "$SCRIPT_DIR/resources/FinderSync-Info.plist" "$FSEXT/Info.plist"
fi

# --- Resolve entitlements (expand TeamID for keychain-access-groups) ---
echo "==> Resolving entitlements..."
EXT_ENTITLEMENTS="$SCRIPT_DIR/resources/Extension.entitlements"
HOST_ENTITLEMENTS="$SCRIPT_DIR/resources/HostApp.entitlements"

if [ "$SIGNING_IDENTITY" != "-" ]; then
    # Extract TeamID from the signing certificate.
    TEAM_ID=$(/usr/bin/security find-identity -v -p codesigning \
        "${SECURITY_IDENTITY_ARGS[@]}" \
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
else
    # `keychain-access-groups` is a restricted entitlement. Ad-hoc development
    # builds must omit it so launchd/amfid will still run the app/extension.
    EXT_ENTITLEMENTS="/tmp/tcfs-ext-entitlements.$$.plist"
    HOST_ENTITLEMENTS="/tmp/tcfs-host-entitlements.$$.plist"
    cp "$SCRIPT_DIR/resources/Extension.entitlements" "$EXT_ENTITLEMENTS"
    cp "$SCRIPT_DIR/resources/HostApp.entitlements" "$HOST_ENTITLEMENTS"
    /usr/libexec/PlistBuddy -c 'Delete :keychain-access-groups' "$EXT_ENTITLEMENTS" 2>/dev/null || true
    /usr/libexec/PlistBuddy -c 'Delete :keychain-access-groups' "$HOST_ENTITLEMENTS" 2>/dev/null || true
fi

if [ "$FILEPROVIDER_TESTING_MODE_ENTITLEMENT" = "1" ]; then
    echo "==> Enabling host app FileProvider testing-mode entitlement"
    /usr/libexec/PlistBuddy \
        -c 'Add :com.apple.developer.fileprovider.testing-mode bool true' \
        "$HOST_ENTITLEMENTS" 2>/dev/null \
        || /usr/libexec/PlistBuddy \
            -c 'Set :com.apple.developer.fileprovider.testing-mode true' \
            "$HOST_ENTITLEMENTS"
fi

# --- Code sign (inside-out: extensions first, then host app) ---
echo "==> Signing..."
FINDER_SYNC_ENTITLEMENTS="$SCRIPT_DIR/resources/FinderSync.entitlements"

if [ "$SIGNING_IDENTITY" != "-" ]; then
    echo "    Identity: $SIGNING_IDENTITY"
    /usr/bin/codesign -f -s "$SIGNING_IDENTITY" \
        "${CODESIGN_KEYCHAIN_ARGS[@]}" \
        --options runtime "$CODESIGN_TIMESTAMP_ARG" \
        --entitlements "$EXT_ENTITLEMENTS" \
        "$APP/Extensions/TCFSFileProvider.appex"

    if [ "$HAVE_FINDER_SYNC" = "true" ]; then
        /usr/bin/codesign -f -s "$SIGNING_IDENTITY" \
            "${CODESIGN_KEYCHAIN_ARGS[@]}" \
            --options runtime "$CODESIGN_TIMESTAMP_ARG" \
            --entitlements "$FINDER_SYNC_ENTITLEMENTS" \
            "$APP/Extensions/TCFSFinderSync.appex"
    fi

    /usr/bin/codesign -f -s "$SIGNING_IDENTITY" \
        "${CODESIGN_KEYCHAIN_ARGS[@]}" \
        --options runtime "$CODESIGN_TIMESTAMP_ARG" \
        --entitlements "$HOST_ENTITLEMENTS" \
        "$OUTPUT_DIR/TCFSProvider.app"
else
    echo "    Identity: ad-hoc (development)"
    /usr/bin/codesign -f -s - \
        --entitlements "$EXT_ENTITLEMENTS" \
        "$APP/Extensions/TCFSFileProvider.appex"

    if [ "$HAVE_FINDER_SYNC" = "true" ]; then
        /usr/bin/codesign -f -s - \
            --entitlements "$FINDER_SYNC_ENTITLEMENTS" \
            "$APP/Extensions/TCFSFinderSync.appex"
    fi

    /usr/bin/codesign -f -s - \
        --entitlements "$HOST_ENTITLEMENTS" \
        "$OUTPUT_DIR/TCFSProvider.app"
fi

# --- Validate outputs ---
echo "==> Validating..."
if [ ! -f "$APP/MacOS/TCFSProvider" ]; then
    echo "ERROR: host app binary missing: $APP/MacOS/TCFSProvider" >&2
    exit 1
fi
if [ ! -f "$EXT/MacOS/TCFSFileProvider" ]; then
    echo "ERROR: extension binary missing: $EXT/MacOS/TCFSFileProvider" >&2
    exit 1
fi
/usr/bin/codesign -v "$OUTPUT_DIR/TCFSProvider.app" 2>/dev/null || {
    echo "ERROR: code signature validation failed for TCFSProvider.app" >&2
    exit 1
}
/usr/bin/codesign -v "$APP/Extensions/TCFSFileProvider.appex" 2>/dev/null || {
    echo "ERROR: code signature validation failed for TCFSFileProvider.appex" >&2
    exit 1
}
echo "    Host app:  OK"
echo "    Extension: OK"

if [ "${TCFS_REQUIRE_PRODUCTION_SIGNING:-0}" = "1" ]; then
    echo "==> Running production signing preflight..."
    bash "$REPO_ROOT/scripts/macos-fileprovider-preflight.sh" \
        --signing-only \
        --require-production-signing \
        --app-path "$OUTPUT_DIR/TCFSProvider.app"
fi

# --- Cleanup temp binaries ---
rm -f TCFSFileProvider TCFSProvider TCFSFinderSync extension_main.o finder_sync_main.o
rm -f "$EMBEDDED_CONFIG_SWIFT"
rm -f "/tmp/tcfs-ext-entitlements.$$.plist" "/tmp/tcfs-host-entitlements.$$.plist"

echo "==> Done: $OUTPUT_DIR/TCFSProvider.app"
