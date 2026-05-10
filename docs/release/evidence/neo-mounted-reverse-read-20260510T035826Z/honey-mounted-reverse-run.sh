#!/usr/bin/env bash
set -euo pipefail

ENDPOINT=http://100.64.48.53:8333
REGION=us-east-1
BUCKET=tcfs
PREFIX=neo-mounted-reverse-read-20260510T035826Z
TCFS_BIN=/tmp/tcfs-fleet-pilot-build-20260509T1907Z/target/debug/tcfs
HONEY_ROOT_RAW='/tmp/tcfs-mounted-reverse-read-20260510T035826Z-46418-honey/root'
RUN_DIR=/tmp/tcfs-mounted-reverse-read-20260510T035826Z-46418-honey/run
FIXTURE_FILE=Projects/shared/mounted-reverse-notes.md
INITIAL_CONTENT="${TCFS_HONEY_INITIAL_CONTENT_FILE:-/tmp/tcfs-mounted-reverse-read-20260510T035826Z-46418-honey/run/honey-initial-content.txt}"
MUTATED_CONTENT="${TCFS_HONEY_MUTATED_CONTENT_FILE:-/tmp/tcfs-mounted-reverse-read-20260510T035826Z-46418-honey/run/honey-mutated-content.txt}"

case "$HONEY_ROOT_RAW" in
  "~/"*) HONEY_ROOT="${HOME}/${HONEY_ROOT_RAW#\~/}" ;;
  *) HONEY_ROOT="$HONEY_ROOT_RAW" ;;
esac

if [[ -n "${TCFS_HONEY_ENV_FILE:-}" ]]; then
  # shellcheck disable=SC1090
  source "$TCFS_HONEY_ENV_FILE"
fi

mode="${1:-}"
[[ -n "$mode" ]] || { echo "mode required: push-initial or push-mutated" >&2; exit 2; }

STATE_DIR="$RUN_DIR/honey-state"
CACHE_ROOT="$STATE_DIR/cache"
CONFIG_PATH="$STATE_DIR/tcfs-mounted-reverse-read.toml"
STATE_JSON="$STATE_DIR/state.json"
FIXTURE_PATH="$HONEY_ROOT/$FIXTURE_FILE"

mkdir -p "$(dirname "$FIXTURE_PATH")" "$CACHE_ROOT"

cat >"$CONFIG_PATH" <<REMOTE_CONFIG
[daemon]
socket = "$STATE_DIR/no-daemon.sock"

[storage]
endpoint = "$ENDPOINT"
region = "$REGION"
bucket = "$BUCKET"
remote_prefix = "$PREFIX"
enforce_tls = false

[sync]
state_db = "$STATE_DIR/state.db"
sync_root = "$HONEY_ROOT"
nats_url = "nats://localhost:4222"
nats_tls = false
sync_empty_dirs = true

[fuse]
cache_dir = "$CACHE_ROOT"
cache_max_mb = 64
negative_cache_ttl_secs = 1

[crypto]
enabled = false
REMOTE_CONFIG

case "$mode" in
  push-initial)
    cp "$INITIAL_CONTENT" "$FIXTURE_PATH"
    "$TCFS_BIN" --config "$CONFIG_PATH" push "$HONEY_ROOT" --prefix "$PREFIX" --state "$STATE_JSON"
    echo "honey mounted reverse initial push ok: $FIXTURE_FILE"
    ;;
  push-mutated)
    cp "$MUTATED_CONTENT" "$FIXTURE_PATH"
    "$TCFS_BIN" --config "$CONFIG_PATH" push "$HONEY_ROOT" --prefix "$PREFIX" --state "$STATE_JSON"
    echo "honey mounted reverse mutated push ok: $FIXTURE_FILE"
    ;;
  *)
    echo "unknown mode: $mode" >&2
    exit 2
    ;;
esac
