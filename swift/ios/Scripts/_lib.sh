#!/usr/bin/env bash
# Shared constants and helpers for TCFS iOS build pipeline.
# Source this file, don't execute it directly.
#
# Usage: source "$(dirname "$0")/_lib.sh"

# ── App Store Connect API ────────────────────────────────────────────────────
ASC_KEY_ID="ZV65L9B864"
ASC_ISSUER_ID="d5db1c0a-0a82-4a50-9490-7d86be080506"
ASC_KEY_PATH="$HOME/.private_keys/AuthKey_${ASC_KEY_ID}.p8"

# ── Code Signing ─────────────────────────────────────────────────────────────
TEAM_ID="QP994XQKNH"
CODE_SIGN_IDENTITY="Apple Distribution: John Sullivan ($TEAM_ID)"

# ── Provisioning Profiles (ASC IDs + installed UUIDs) ────────────────────────
HOST_PROFILE_ASC_ID="DSJJF296HL"
HOST_PROFILE_UUID="d4a36b3a-33bd-49aa-badf-97a6f6462afb"
EXT_PROFILE_ASC_ID="XUPUW332LB"
EXT_PROFILE_UUID="df186456-5ce7-4152-bd09-dcdca0f89bfc"

# ── Paths ────────────────────────────────────────────────────────────────────
_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IOS_DIR="$(cd "$_LIB_DIR/.." && pwd)"
REPO_ROOT="$(cd "$IOS_DIR/../.." && pwd)"

ARCHIVE_PATH="$IOS_DIR/build/TCFS.xcarchive"
EXPORT_DIR="$IOS_DIR/build/export"
IPA_FILE="$EXPORT_DIR/TCFS.ipa"
EXPORT_OPTIONS="$_LIB_DIR/ExportOptions.plist"

RUST_TARGET="aarch64-apple-ios"
STATICLIB="$REPO_ROOT/target/$RUST_TARGET/release/libtcfs_file_provider.a"
GENERATED_DIR="$IOS_DIR/Generated"

# ── Clean Environment ────────────────────────────────────────────────────────
# Nix devshell sets NIX_*, CC, LD, SDKROOT, DEVELOPER_DIR, buildInputs, etc.
# that corrupt xcodebuild. We use an allowlist (env -i) not a blocklist.
CLEAN_PATH="/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin"
XCODE_DEVELOPER_DIR="/Applications/Xcode.app/Contents/Developer"

# Run a command in a pristine environment free of nix/direnv pollution.
# Usage: clean_exec <command> [args...]
clean_exec() {
    env -i \
        HOME="$HOME" \
        USER="${USER:-$(whoami)}" \
        PATH="$CLEAN_PATH" \
        TMPDIR="${TMPDIR:-/tmp}" \
        DEVELOPER_DIR="$XCODE_DEVELOPER_DIR" \
        "$@"
}

# ── Helpers ──────────────────────────────────────────────────────────────────

# Print the latest Xcode distribution log (useful after export failures)
show_dist_logs() {
    local latest_log
    latest_log=$(ls -td /var/folders/*/T/TCFS_*.xcdistributionlogs 2>/dev/null | head -1)
    if [ -n "${latest_log:-}" ]; then
        echo "    Distribution log: $latest_log"
        tail -30 "$latest_log/IDEDistribution.standard.log" 2>/dev/null || true
    fi
}

# Download provisioning profiles from ASC if not already installed
ensure_profiles() {
    local profiles_dir="$HOME/Library/MobileDevice/Provisioning Profiles"
    mkdir -p "$profiles_dir"

    # Check if both profiles are already installed
    if [ -f "$profiles_dir/$HOST_PROFILE_UUID.mobileprovision" ] && \
       [ -f "$profiles_dir/$EXT_PROFILE_UUID.mobileprovision" ]; then
        echo "    Profiles already installed"
        return 0
    fi

    # Need to download — generate JWT
    if ! command -v python3 &>/dev/null || [ ! -f "$_LIB_DIR/asc-jwt.py" ]; then
        echo "    WARNING: Cannot download profiles (python3 or asc-jwt.py not found)"
        echo "    Install profiles manually in $profiles_dir"
        return 1
    fi

    if [ ! -f "$ASC_KEY_PATH" ]; then
        echo "    WARNING: ASC API key not found at $ASC_KEY_PATH"
        return 1
    fi

    local jwt
    jwt=$(python3 "$_LIB_DIR/asc-jwt.py" "$ASC_KEY_ID" "$ASC_ISSUER_ID" "$ASC_KEY_PATH")

    if [ ! -f "$profiles_dir/$HOST_PROFILE_UUID.mobileprovision" ]; then
        echo "    Downloading host app profile..."
        local b64
        b64=$(curl -sf "https://api.appstoreconnect.apple.com/v1/profiles/$HOST_PROFILE_ASC_ID" \
            -H "Authorization: Bearer $jwt" | python3 -c "import sys,json; print(json.load(sys.stdin)['data']['attributes']['profileContent'])")
        echo "$b64" | base64 -d > "$profiles_dir/$HOST_PROFILE_UUID.mobileprovision"
    fi

    if [ ! -f "$profiles_dir/$EXT_PROFILE_UUID.mobileprovision" ]; then
        echo "    Downloading extension profile..."
        local b64
        b64=$(curl -sf "https://api.appstoreconnect.apple.com/v1/profiles/$EXT_PROFILE_ASC_ID" \
            -H "Authorization: Bearer $jwt" | python3 -c "import sys,json; print(json.load(sys.stdin)['data']['attributes']['profileContent'])")
        echo "$b64" | base64 -d > "$profiles_dir/$EXT_PROFILE_UUID.mobileprovision"
    fi

    echo "    Profiles installed"
}

# Verify signing identity is available
check_signing() {
    if clean_exec /usr/bin/security find-identity -v -p codesigning 2>&1 | grep -q "0 valid identities"; then
        echo "ERROR: No signing identities found." >&2
        echo "Run setup-signing.sh from Terminal.app first." >&2
        return 1
    fi
    echo "    Signing identity: $CODE_SIGN_IDENTITY"
}
