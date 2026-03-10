#!/usr/bin/env bash
# Generate a TCFS bootstrap QR code for iOS device enrollment.
#
# Usage:
#   ./gen-bootstrap-qr.sh                    # uses sops-nix secrets from current host
#   ./gen-bootstrap-qr.sh --device-id myphone
#
# Requires: qrencode (brew install qrencode)
set -euo pipefail

DEVICE_ID="${1:-ios-$(hostname -s | tr '[:upper:]' '[:lower:]')}"

# Source hm-session-vars for TCFS_S3_*_FILE paths
HM="$HOME/.nix-profile/etc/profile.d/hm-session-vars.sh"
if [ -f "$HM" ]; then
    # hm-session-vars.sh checks $__HM_SESS_VARS_SOURCED — can't use set -u before sourcing
    set +u; source "$HM"; set -u
fi

# Read S3 credentials from sops-nix secret files
if [ -n "${TCFS_S3_ACCESS_KEY_FILE:-}" ] && [ -f "$TCFS_S3_ACCESS_KEY_FILE" ]; then
    ACCESS_KEY="$(cat "$TCFS_S3_ACCESS_KEY_FILE")"
else
    echo "ERROR: TCFS_S3_ACCESS_KEY_FILE not set or missing" >&2
    exit 1
fi

if [ -n "${TCFS_S3_SECRET_KEY_FILE:-}" ] && [ -f "$TCFS_S3_SECRET_KEY_FILE" ]; then
    S3_SECRET="$(cat "$TCFS_S3_SECRET_KEY_FILE")"
else
    echo "ERROR: TCFS_S3_SECRET_KEY_FILE not set or missing" >&2
    exit 1
fi

# Read endpoint + bucket from tcfs config
CONFIG="${HOME}/.config/tcfs/config.toml"
if [ -f "$CONFIG" ]; then
    ENDPOINT=$(grep '^endpoint' "$CONFIG" | head -1 | sed 's/.*= *"//;s/".*//')
    BUCKET=$(grep '^bucket' "$CONFIG" | head -1 | sed 's/.*= *"//;s/".*//')
else
    ENDPOINT="${TCFS_S3_ENDPOINT:-http://212.2.245.145:8333}"
    BUCKET="tcfs"
fi

# Read encryption passphrase from sops-nix secret file (optional — plaintext mode if absent)
ENCRYPTION_PASSPHRASE=""
if [ -n "${TCFS_ENCRYPTION_KEY_FILE:-}" ] && [ -f "${TCFS_ENCRYPTION_KEY_FILE:-}" ]; then
    ENCRYPTION_PASSPHRASE="$(cat "$TCFS_ENCRYPTION_KEY_FILE")"
fi

# Read encryption salt: env var > config.toml [crypto] section > empty (plaintext mode)
ENCRYPTION_SALT="${TCFS_ENCRYPTION_SALT:-}"
if [ -z "$ENCRYPTION_SALT" ] && [ -f "$CONFIG" ]; then
    ENCRYPTION_SALT=$(grep '^salt' "$CONFIG" | head -1 | sed 's/.*= *"//;s/".*//')
fi

# Build encryption JSON fields (omitted entirely if passphrase is empty)
ENCRYPTION_JSON=""
if [ -n "$ENCRYPTION_PASSPHRASE" ]; then
    ENCRYPTION_JSON=$(printf ',"encryption_passphrase":"%s","encryption_salt":"%s"' "$ENCRYPTION_PASSPHRASE" "$ENCRYPTION_SALT")
fi

# Build JSON payload
JSON=$(cat <<EOF
{"type":"tcfs-bootstrap","s3_endpoint":"${ENDPOINT}","s3_bucket":"${BUCKET}","access_key":"${ACCESS_KEY}","s3_secret":"${S3_SECRET}","remote_prefix":"default","device_id":"${DEVICE_ID}"${ENCRYPTION_JSON}}
EOF
)

echo "==> Bootstrap config for device: $DEVICE_ID"
echo "    Endpoint: $ENDPOINT"
echo "    Bucket:   $BUCKET"
if [ -n "$ENCRYPTION_PASSPHRASE" ]; then
    echo "    Encryption: enabled (passphrase from TCFS_ENCRYPTION_KEY_FILE)"
    echo "    Salt:     ${ENCRYPTION_SALT:-(empty)}"
else
    echo "    Encryption: disabled (no TCFS_ENCRYPTION_KEY_FILE)"
fi
echo ""

if command -v qrencode &>/dev/null; then
    OUT="/tmp/tcfs-bootstrap-qr.png"
    echo "$JSON" | qrencode -o "$OUT" -s 8 -l H
    echo "==> QR code saved to: $OUT"

    # Try to open it
    if command -v open &>/dev/null; then
        open "$OUT"
    elif command -v xdg-open &>/dev/null; then
        xdg-open "$OUT"
    fi
else
    echo "==> Install qrencode to generate QR image:"
    echo "    brew install qrencode"
    echo ""
    echo "==> Raw JSON (paste into any QR generator):"
    echo "$JSON"
fi
