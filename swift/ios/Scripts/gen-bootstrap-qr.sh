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

# Expiry: 1 hour from now (Unix timestamp)
CREATED_AT=$(date +%s)
EXPIRES_AT=$((CREATED_AT + 3600))

# Build the signable payload (everything except signature itself)
PAYLOAD=$(cat <<EOF
{"type":"tcfs-bootstrap","s3_endpoint":"${ENDPOINT}","s3_bucket":"${BUCKET}","access_key":"${ACCESS_KEY}","s3_secret":"${S3_SECRET}","remote_prefix":"default","device_id":"${DEVICE_ID}","created_at":${CREATED_AT},"expires_at":${EXPIRES_AT}${ENCRYPTION_JSON}}
EOF
)

# Sign with BLAKE3-keyed-MAC if signing key is available
SIGNATURE=""
SIGNING_KEY=""
# Derive signing key from master key file or sops-nix secret
if [ -f "${HOME}/.config/tcfs/master.key" ] && command -v b3sum &>/dev/null; then
    # master.key is 32 raw bytes; hex-encode for b3sum --keyed
    SIGNING_KEY=$(xxd -p -c 64 "${HOME}/.config/tcfs/master.key")
elif [ -n "${TCFS_ENCRYPTION_KEY_FILE:-}" ] && [ -f "${TCFS_ENCRYPTION_KEY_FILE:-}" ] && command -v b3sum &>/dev/null; then
    # Derive signing key from encryption passphrase via BLAKE3 hash (deterministic 32 bytes)
    SIGNING_KEY=$(printf '%s' "tcfs-bootstrap-signing" | b3sum --derive-key "$(cat "$TCFS_ENCRYPTION_KEY_FILE")" | cut -d' ' -f1)
fi

if [ -n "$SIGNING_KEY" ]; then
    SIGNATURE=$(printf '%s' "$PAYLOAD" | b3sum --keyed <<< "$SIGNING_KEY" | cut -d' ' -f1)
fi

# Build final JSON with signature
SIGNATURE_JSON=""
if [ -n "$SIGNATURE" ]; then
    SIGNATURE_JSON=$(printf ',"signature":"%s"' "$SIGNATURE")
fi

JSON=$(cat <<EOF
{"type":"tcfs-bootstrap","s3_endpoint":"${ENDPOINT}","s3_bucket":"${BUCKET}","access_key":"${ACCESS_KEY}","s3_secret":"${S3_SECRET}","remote_prefix":"default","device_id":"${DEVICE_ID}","created_at":${CREATED_AT},"expires_at":${EXPIRES_AT}${ENCRYPTION_JSON}${SIGNATURE_JSON}}
EOF
)

echo "==> Bootstrap config for device: $DEVICE_ID"
echo "    Endpoint: $ENDPOINT"
echo "    Bucket:   $BUCKET"
echo "    Created:  $(date -r "$CREATED_AT" 2>/dev/null || date -d "@$CREATED_AT" 2>/dev/null || echo "$CREATED_AT")"
echo "    Expires:  $(date -r "$EXPIRES_AT" 2>/dev/null || date -d "@$EXPIRES_AT" 2>/dev/null || echo "$EXPIRES_AT")"
if [ -n "$ENCRYPTION_PASSPHRASE" ]; then
    echo "    Encryption: enabled (passphrase from TCFS_ENCRYPTION_KEY_FILE)"
    echo "    Salt:     ${ENCRYPTION_SALT:-(empty)}"
else
    echo "    Encryption: disabled (no TCFS_ENCRYPTION_KEY_FILE)"
fi
if [ -n "$SIGNATURE" ]; then
    echo "    Signature: ${SIGNATURE:0:16}..."
else
    echo "    Signature: unsigned (b3sum or master key not available)"
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
