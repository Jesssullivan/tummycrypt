#!/usr/bin/env bash
# Provision TCFS FileProvider config into the App Group shared container.
#
# The FileProvider extension reads config.json from the App Group container
# (group.io.tinyland.tcfs) since sandboxed extensions can't read env vars
# or arbitrary filesystem paths.
#
# Usage:
#   ./provision-config.sh                    # Uses TCFS_CONFIG or defaults
#   ./provision-config.sh /path/to/config.toml  # Explicit config path
#
# Reads S3 credentials from environment or sops secrets:
#   AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY
#   TCFS_S3_ACCESS_KEY_FILE / TCFS_S3_SECRET_KEY_FILE

set -euo pipefail

# --- Locate TCFS config ---
CONFIG_TOML="${1:-${TCFS_CONFIG:-$HOME/.config/tcfs/config.toml}}"

if [ ! -f "$CONFIG_TOML" ]; then
    echo "ERROR: TCFS config not found at $CONFIG_TOML" >&2
    echo "Set TCFS_CONFIG or pass path as argument" >&2
    exit 1
fi

echo "==> Reading config from $CONFIG_TOML"

# --- Parse S3 endpoint from config.toml ---
# Simple grep-based parsing (avoids toml parser dependency)
# sed -E for extended regex on macOS; extract value between quotes
extract_toml() {
    grep -E "^[[:space:]]*$1[[:space:]]*=" "$CONFIG_TOML" 2>/dev/null | head -1 | sed -E 's/.*=[[:space:]]*"([^"]*)".*/\1/' || true
}
S3_ENDPOINT="$(extract_toml endpoint)"
S3_BUCKET="$(extract_toml bucket)"
DEVICE_ID="$(extract_toml device_id)"

S3_ENDPOINT="${S3_ENDPOINT:-http://212.2.245.145:8333}"
S3_BUCKET="${S3_BUCKET:-tcfs}"
DEVICE_ID="${DEVICE_ID:-$(hostname -s)}"

# --- Resolve S3 credentials ---
if [ -n "${AWS_ACCESS_KEY_ID:-}" ] && [ -n "${AWS_SECRET_ACCESS_KEY:-}" ]; then
    S3_ACCESS="$AWS_ACCESS_KEY_ID"
    S3_SECRET="$AWS_SECRET_ACCESS_KEY"
elif [ -n "${TCFS_S3_ACCESS_KEY_FILE:-}" ] && [ -f "${TCFS_S3_ACCESS_KEY_FILE:-}" ]; then
    S3_ACCESS="$(cat "$TCFS_S3_ACCESS_KEY_FILE")"
    S3_SECRET="$(cat "${TCFS_S3_SECRET_KEY_FILE:-}")"
else
    # Try sourcing hm-session-vars for sops secrets
    HM_VARS="$HOME/.nix-profile/etc/profile.d/hm-session-vars.sh"
    if [ -f "$HM_VARS" ]; then
        set +u
        . "$HM_VARS"
        set -u
    fi

    if [ -n "${TCFS_S3_ACCESS_KEY_FILE:-}" ] && [ -f "${TCFS_S3_ACCESS_KEY_FILE:-}" ]; then
        S3_ACCESS="$(cat "$TCFS_S3_ACCESS_KEY_FILE")"
        S3_SECRET="$(cat "${TCFS_S3_SECRET_KEY_FILE:-}")"
    else
        echo "ERROR: No S3 credentials found" >&2
        echo "Set AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY or TCFS_S3_ACCESS_KEY_FILE/TCFS_S3_SECRET_KEY_FILE" >&2
        exit 1
    fi
fi

# --- Write config to both locations ---
# The .appex reads from the App Group container:
#   ~/Library/Group Containers/group.io.tinyland.tcfs/
# For development, also write to XDG config path:
#   ~/.config/tcfs/fileprovider/
DEV_DIR="$HOME/.config/tcfs/fileprovider"
mkdir -p "$DEV_DIR"
GROUP_CONTAINER="$DEV_DIR"

CONFIG_JSON="$GROUP_CONTAINER/config.json"

cat > "$CONFIG_JSON" <<CONFIGEOF
{
  "s3_endpoint": "$S3_ENDPOINT",
  "s3_bucket": "$S3_BUCKET",
  "s3_access": "$S3_ACCESS",
  "s3_secret": "$S3_SECRET",
  "remote_prefix": "devices/$DEVICE_ID",
  "device_id": "$DEVICE_ID"
}
CONFIGEOF

chmod 600 "$CONFIG_JSON"

echo "==> Config written to $CONFIG_JSON"
echo "    Endpoint: $S3_ENDPOINT"
echo "    Bucket:   $S3_BUCKET"
echo "    Device:   $DEVICE_ID"
echo "    Credentials: $(echo "$S3_ACCESS" | head -c 4)****"

# Also copy to App Group container if it already exists (for sandboxed .appex)
APP_GROUP_DIR="$HOME/Library/Group Containers/group.io.tinyland.tcfs"
if [ -d "$APP_GROUP_DIR" ]; then
    cp "$CONFIG_JSON" "$APP_GROUP_DIR/config.json" 2>/dev/null && \
        chmod 600 "$APP_GROUP_DIR/config.json" 2>/dev/null && \
        echo "==> Also written to $APP_GROUP_DIR/config.json" || true
fi
