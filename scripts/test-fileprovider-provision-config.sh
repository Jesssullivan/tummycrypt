#!/usr/bin/env bash
#
# Regression tests for swift/fileprovider/provision-config.sh.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="${REPO_ROOT}/swift/fileprovider/provision-config.sh"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-fileprovider-provision-test.XXXXXX")"
trap 'rm -rf "$TMPDIR"' EXIT

HOME_DIR="${TMPDIR}/home"
CONFIG_TOML="${TMPDIR}/tcfs.toml"
MASTER_KEY_FILE="${TMPDIR}/master.key"
OUTPUT_JSON="${HOME_DIR}/.config/tcfs/fileprovider/config.json"
APP_GROUP_JSON="${HOME_DIR}/Library/Group Containers/group.io.tinyland.tcfs/config.json"

mkdir -p "$HOME_DIR" "$(dirname "$APP_GROUP_JSON")"
printf '01234567890123456789012345678901' >"$MASTER_KEY_FILE"

cat >"$CONFIG_TOML" <<EOF
[daemon]
fileprovider_endpoint = "http://127.0.0.1:19101"

[storage]
endpoint = "http://example.invalid:8333"
bucket = "tcfs"
remote_prefix = "data"

[crypto]
master_key_file = "$MASTER_KEY_FILE"
EOF

HOME="$HOME_DIR" \
AWS_ACCESS_KEY_ID="test-access" \
AWS_SECRET_ACCESS_KEY="test-secret" \
bash "$SCRIPT" "$CONFIG_TOML" >"${TMPDIR}/provision.out"

jq -e \
  --arg endpoint "http://example.invalid:8333" \
  --arg bucket "tcfs" \
  --arg access "test-access" \
  --arg secret "test-secret" \
  --arg prefix "data" \
  --arg endpoint_target "http://127.0.0.1:19101" \
  --arg master_key_file "$MASTER_KEY_FILE" \
  '
    .s3_endpoint == $endpoint and
    .s3_bucket == $bucket and
    .s3_access == $access and
    .s3_secret == $secret and
    .remote_prefix == $prefix and
    .daemon_endpoint == $endpoint_target and
    (has("daemon_socket") | not) and
    .master_key_file == $master_key_file and
    (has("master_key_base64") | not)
  ' "$OUTPUT_JSON" >/dev/null

grep -Fq "Master key file: present" "${TMPDIR}/provision.out"
grep -Fq "Endpoint: http://127.0.0.1:19101" "${TMPDIR}/provision.out"
grep -Fq "Also written to $APP_GROUP_JSON" "${TMPDIR}/provision.out"
grep -Fq "TCFS_FILEPROVIDER_APP_GROUP_COPY_TIMEOUT" "$SCRIPT"
grep -Fq "TCFS_FILEPROVIDER_SKIP_APP_GROUP_COPY" "$SCRIPT"

jq -e \
  --arg master_key_file "$MASTER_KEY_FILE" \
  '
    .master_key_file == $master_key_file and
    (has("master_key_base64") | not)
  ' "$APP_GROUP_JSON" >/dev/null

rm -f "$APP_GROUP_JSON"
HOME="$HOME_DIR" \
AWS_ACCESS_KEY_ID="test-access" \
AWS_SECRET_ACCESS_KEY="test-secret" \
TCFS_FILEPROVIDER_SKIP_APP_GROUP_COPY=1 \
bash "$SCRIPT" "$CONFIG_TOML" >"${TMPDIR}/provision-skip.out"

grep -Fq "Skipping App Group config mirror" "${TMPDIR}/provision-skip.out"
if [[ -e "$APP_GROUP_JSON" ]]; then
  printf 'expected skip mode not to write %s\n' "$APP_GROUP_JSON" >&2
  exit 1
fi

printf 'FileProvider provision config tests passed\n'
