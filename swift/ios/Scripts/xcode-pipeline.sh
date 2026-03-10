#!/usr/bin/env bash
# Xcode pipeline: xcodegen → archive → export IPA.
#
# Must run from Terminal.app (keychain codesign requires GUI session).
# Must NOT run inside nix devshell (xcodebuild breaks with nix env vars).
#
# Usage:
#   ./Scripts/xcode-pipeline.sh              # Full: xcodegen + archive + export
#   ./Scripts/xcode-pipeline.sh --skip-archive   # Export only (archive must exist)
#
# Prerequisites:
#   - Rust staticlib built (run build-rust.sh first)
#   - Signing identity in keychain (run setup-signing.sh first)
#   - Provisioning profiles installed (auto-downloaded if ASC key available)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=_lib.sh
source "$SCRIPT_DIR/_lib.sh"

trap 'echo ""; echo "FAILED at line $LINENO"; show_dist_logs' ERR

SKIP_ARCHIVE=false
for arg in "$@"; do
    case "$arg" in
        --skip-archive) SKIP_ARCHIVE=true ;;
    esac
done

# ── Preflight checks ────────────────────────────────────────────────────
echo "==> Xcode Pipeline"

if ! $SKIP_ARCHIVE && [ ! -f "$STATICLIB" ]; then
    echo "ERROR: Rust staticlib not found at $STATICLIB" >&2
    echo "Run: just ios-rust   (or: nix develop -c bash Scripts/build-rust.sh)" >&2
    exit 1
fi

echo "    Checking signing identity..."
check_signing

echo "    Checking provisioning profiles..."
ensure_profiles

if [ ! -f "$ASC_KEY_PATH" ]; then
    echo "ERROR: ASC API key not found at $ASC_KEY_PATH" >&2
    echo "Download from App Store Connect → Users and Access → Keys" >&2
    exit 1
fi

# ── Step 1: Generate Xcode project ───────────────────────────────────────
if ! $SKIP_ARCHIVE; then
    echo ""
    echo "==> Stamping git SHA..."
    GIT_SHA=$(cd "$REPO_ROOT" && git rev-parse --short HEAD 2>/dev/null || echo "unknown")
    # Patch GIT_COMMIT_SHA in project.yml so xcodegen bakes it into the build settings
    sed -i '' "s/GIT_COMMIT_SHA: .*/GIT_COMMIT_SHA: \"$GIT_SHA\"/" "$IOS_DIR/project.yml"
    echo "    SHA: $GIT_SHA"

    echo ""
    echo "==> Generating Xcode project..."
    cd "$IOS_DIR"
    clean_exec /opt/homebrew/bin/xcodegen generate
fi

# ── Step 2: Archive ──────────────────────────────────────────────────────
if $SKIP_ARCHIVE; then
    if [ ! -d "$ARCHIVE_PATH" ]; then
        echo "ERROR: No archive found at $ARCHIVE_PATH" >&2
        echo "Run without --skip-archive to build it." >&2
        exit 1
    fi
    echo "    Using existing archive: $ARCHIVE_PATH"
else
    echo ""
    echo "==> Archiving..."
    rm -rf "$ARCHIVE_PATH"

    clean_exec /usr/bin/xcrun xcodebuild archive \
        -project "$IOS_DIR/TCFS.xcodeproj" \
        -scheme "TCFS" \
        -sdk iphoneos \
        -configuration Release \
        -archivePath "$ARCHIVE_PATH" \
        CODE_SIGN_STYLE=Manual \
        "CODE_SIGN_IDENTITY=$CODE_SIGN_IDENTITY" \
        DEVELOPMENT_TEAM="$TEAM_ID" \
        ONLY_ACTIVE_ARCH=NO \
        SKIP_INSTALL=NO

    echo "    Archive: $ARCHIVE_PATH"
fi

# ── Step 3: Export IPA ───────────────────────────────────────────────────
echo ""
echo "==> Exporting IPA..."
rm -rf "$EXPORT_DIR"

clean_exec /usr/bin/xcrun xcodebuild -exportArchive \
    -archivePath "$ARCHIVE_PATH" \
    -exportPath "$EXPORT_DIR" \
    -exportOptionsPlist "$EXPORT_OPTIONS"

if [ ! -f "$IPA_FILE" ]; then
    echo "ERROR: IPA not found at $IPA_FILE" >&2
    show_dist_logs
    exit 1
fi

echo "    IPA: $IPA_FILE ($(du -h "$IPA_FILE" | cut -f1))"
echo "==> Xcode pipeline complete"
