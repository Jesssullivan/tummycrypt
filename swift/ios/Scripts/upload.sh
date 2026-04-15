#!/usr/bin/env bash
# Upload an experimental IPA build to TestFlight.
#
# Usage:
#   ./Scripts/upload.sh              # Upload build/export/TCFS.ipa
#   ./Scripts/upload.sh /path/to.ipa # Upload specific IPA
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=_lib.sh
source "$SCRIPT_DIR/_lib.sh"

# Allow overriding IPA path via argument
UPLOAD_IPA="${1:-$IPA_FILE}"

if [ ! -f "$UPLOAD_IPA" ]; then
    echo "ERROR: IPA not found at $UPLOAD_IPA" >&2
    echo "Run xcode-pipeline.sh first to build the IPA." >&2
    exit 1
fi

if [ ! -f "$ASC_KEY_PATH" ]; then
    echo "ERROR: ASC API key not found at $ASC_KEY_PATH" >&2
    exit 1
fi

echo "==> Uploading experimental build to TestFlight"
echo "    IPA: $UPLOAD_IPA ($(du -h "$UPLOAD_IPA" | cut -f1))"

clean_exec /usr/bin/xcrun altool --upload-app \
    -f "$UPLOAD_IPA" \
    -t ios \
    --apiKey "$ASC_KEY_ID" \
    --apiIssuer "$ASC_ISSUER_ID"

echo ""
echo "==> Experimental upload complete. Check App Store Connect for processing."
echo "    https://appstoreconnect.apple.com"
