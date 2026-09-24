#!/usr/bin/env bash
# Finalize an already signed macOS app. This does not build, sign, or install it.
# Usage: notarize.sh <app_path> [--keychain-profile <profile>] [--result-file <path>]
# The final raw Apple JSON is published only after Accepted, stapling and assessment.
set -euo pipefail

APP_PATH="${1:?Usage: notarize.sh <app_path> [--keychain-profile <profile>] [--result-file <path>]}"
shift
[[ -d "$APP_PATH" && ! -L "$APP_PATH" ]] || { echo 'Expected a signed app directory.' >&2; exit 2; }
APP_NAME="$(basename "$APP_PATH" .app)"
RESULT_FILE="$(dirname "$APP_PATH")/${APP_NAME}.notarization.json"
KEYCHAIN_PROFILE=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --keychain-profile) KEYCHAIN_PROFILE="${2:?Missing keychain profile}"; shift 2 ;;
        --result-file) RESULT_FILE="${2:?Missing result path}"; shift 2 ;;
        *) echo 'Unknown notarization argument.' >&2; exit 2 ;;
    esac
done
AUTH=()
if [[ -n "$KEYCHAIN_PROFILE" ]]; then
    AUTH=(--keychain-profile "$KEYCHAIN_PROFILE")
else
    for required in APPLE_ID APPLE_TEAM_ID APPLE_NOTARIZE_PASSWORD; do
        [[ -n "${!required:-}" ]] || { echo "${required} is required for app notarization." >&2; exit 2; }
    done
    AUTH=(--apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$APPLE_NOTARIZE_PASSWORD")
fi
CODESIGN="${TCFS_RELEASE_CODESIGN:-/usr/bin/codesign}"
DITTO="${TCFS_RELEASE_DITTO:-/usr/bin/ditto}"
SPCTL="${TCFS_RELEASE_SPCTL:-/usr/sbin/spctl}"
ZIP_PATH="${RESULT_FILE%.json}.submission.zip"
PENDING_RESULT="${RESULT_FILE}.pending"
for output in "$RESULT_FILE" "$PENDING_RESULT" "$ZIP_PATH"; do
    [[ ! -e "$output" && ! -L "$output" ]] || { echo 'Refusing to overwrite notarization output.' >&2; exit 2; }
done
"$CODESIGN" --verify --deep --strict "$APP_PATH"
"$DITTO" -c -k --keepParent "$APP_PATH" "$ZIP_PATH"
( set -o noclobber
  xcrun notarytool submit "$ZIP_PATH" "${AUTH[@]}" \
      --wait --timeout 15m --output-format json > "$PENDING_RESULT"
)
python3 - "$PENDING_RESULT" <<'PY'
import json, sys, uuid
with open(sys.argv[1], 'rb') as source:
    raw = source.read(65537)
if len(raw) > 65536:
    raise SystemExit('App notarization response exceeds bound')
result = json.loads(raw)
if not isinstance(result, dict) or result.get('status') != 'Accepted':
    raise SystemExit('App notarization was not Accepted')
identifier = result.get('id')
if not isinstance(identifier, str) or str(uuid.UUID(identifier)) != identifier.lower():
    raise SystemExit('App notarization submission ID is invalid')
PY
xcrun stapler staple "$APP_PATH"
xcrun stapler validate -v "$APP_PATH"
"$SPCTL" --assess --type exec --verbose=2 "$APP_PATH"
# Preserve Apple's exact bytes, bounded and revalidated before exclusive publication.
python3 - "$PENDING_RESULT" "$RESULT_FILE" <<'PY'
import json, sys, uuid
with open(sys.argv[1], 'rb') as source:
    raw = source.read(65537)
if len(raw) > 65536:
    raise SystemExit('App notarization response exceeds bound')
result = json.loads(raw)
if not isinstance(result, dict) or result.get('status') != 'Accepted':
    raise SystemExit('App notarization was not Accepted')
identifier = result.get('id')
if not isinstance(identifier, str) or str(uuid.UUID(identifier)) != identifier.lower():
    raise SystemExit('App notarization submission ID is invalid')
with open(sys.argv[2], 'xb') as output:
    output.write(raw)
PY
rm -f "$ZIP_PATH" "$PENDING_RESULT"
printf 'Finalized signed app; retained Apple evidence: %s\n' "$RESULT_FILE"
