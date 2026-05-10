#!/usr/bin/env bash
set -euo pipefail

REMOTE=seaweedfs://100.64.48.53:8333/tcfs/neo-honey-reverse-unsynced-rehydrate-20260510T022858Z
ENDPOINT=http://100.64.48.53:8333
REGION=us-east-1
BUCKET=tcfs
PREFIX=neo-honey-reverse-unsynced-rehydrate-20260510T022858Z
TCFS_BIN=/tmp/tcfs-fleet-pilot-build-20260509T1907Z/target/debug/tcfs
HONEY_ROOT_RAW='/tmp/tcfs-neo-honey-reverse-unsynced-rehydrate-20260510T022858Z-honey/root'
RUN_DIR=/tmp/tcfs-neo-honey-reverse-unsynced-rehydrate-20260510T022858Z-honey/run
FIXTURE_FILE=Projects/shared/reverse-notes.md
INITIAL_CONTENT="${TCFS_HONEY_INITIAL_CONTENT_FILE:-/tmp/tcfs-neo-honey-reverse-unsynced-rehydrate-20260510T022858Z-honey/run/neo-initial-content.txt}"
MUTATED_CONTENT="${TCFS_HONEY_MUTATED_CONTENT_FILE:-/tmp/tcfs-neo-honey-reverse-unsynced-rehydrate-20260510T022858Z-honey/run/neo-mutated-content.txt}"

case "$HONEY_ROOT_RAW" in
  "~/"*) HONEY_ROOT="${HOME}/${HONEY_ROOT_RAW#\~/}" ;;
  *) HONEY_ROOT="$HONEY_ROOT_RAW" ;;
esac

if [[ -n "${TCFS_HONEY_ENV_FILE:-}" ]]; then
  # shellcheck disable=SC1090
  source "$TCFS_HONEY_ENV_FILE"
fi

mode="${1:-}"
[[ -n "$mode" ]] || { echo "mode required: prepare-unsync or rehydrate" >&2; exit 2; }

STATE_DIR="$RUN_DIR/honey-state"
CACHE_ROOT="$STATE_DIR/cache"
EVIDENCE_DIR="$RUN_DIR/honey-evidence"
CONFIG_PATH="$STATE_DIR/tcfs-reverse-unsynced-rehydrate.toml"
STATE_JSON="$STATE_DIR/state.json"
FIXTURE_PATH="$HONEY_ROOT/$FIXTURE_FILE"
STUB_PATH="${FIXTURE_PATH}.tc"

mkdir -p "$(dirname "$FIXTURE_PATH")" "$CACHE_ROOT" "$EVIDENCE_DIR"

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
  prepare-unsync)
    "$TCFS_BIN" --config "$CONFIG_PATH" pull "$FIXTURE_FILE" "$FIXTURE_PATH" --prefix "$PREFIX" --state "$STATE_JSON" >"$EVIDENCE_DIR/honey-initial-pull.log" 2>&1
    cmp -s "$INITIAL_CONTENT" "$FIXTURE_PATH"
    "$TCFS_BIN" --config "$CONFIG_PATH" unsync "$FIXTURE_PATH" >"$EVIDENCE_DIR/honey-unsync.out" 2>&1
    [[ ! -f "$FIXTURE_PATH" ]] || { echo "honey hydrated file still exists after unsync: $FIXTURE_PATH" >&2; exit 1; }
    [[ -f "$STUB_PATH" ]] || { echo "honey stub missing after unsync: $STUB_PATH" >&2; exit 1; }
    "$TCFS_BIN" --config "$CONFIG_PATH" sync-status "$FIXTURE_PATH" >"$EVIDENCE_DIR/honey-sync-status-after-unsync.out" 2>&1
    grep -q "sync state: not_synced" "$EVIDENCE_DIR/honey-sync-status-after-unsync.out"
    echo "honey reverse prepare unsync ok: $FIXTURE_FILE"
    ;;
  rehydrate)
    "$TCFS_BIN" --config "$CONFIG_PATH" pull "$FIXTURE_FILE" "$FIXTURE_PATH" --prefix "$PREFIX" --state "$STATE_JSON" >"$EVIDENCE_DIR/honey-rehydrate-pull.log" 2>&1
    cmp -s "$MUTATED_CONTENT" "$FIXTURE_PATH"
    "$TCFS_BIN" --config "$CONFIG_PATH" sync-status "$FIXTURE_PATH" >"$EVIDENCE_DIR/honey-sync-status-after-rehydrate.out" 2>&1
    grep -q "sync state: synced" "$EVIDENCE_DIR/honey-sync-status-after-rehydrate.out"
    if [[ -e "$STUB_PATH" ]]; then
      {
        echo "stub_after_pull=present"
        echo "stub_path=$STUB_PATH"
      } >"$EVIDENCE_DIR/honey-stub-status.env"
      echo "stale honey stub still present after rehydrate: $STUB_PATH" >&2
      exit 1
    fi
    {
      echo "stub_after_pull=absent"
      echo "stub_path=$STUB_PATH"
    } >"$EVIDENCE_DIR/honey-stub-status.env"
    echo "honey reverse rehydrate ok: $FIXTURE_FILE"
    echo "stub_after_pull=absent"
    ;;
  *)
    echo "unknown mode: $mode" >&2
    exit 2
    ;;
esac
