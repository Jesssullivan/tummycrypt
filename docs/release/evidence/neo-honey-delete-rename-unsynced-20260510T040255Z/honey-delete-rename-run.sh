#!/usr/bin/env bash
set -euo pipefail

ENDPOINT=http://100.64.48.53:8333
REGION=us-east-1
BUCKET=tcfs
PREFIX=neo-honey-delete-rename-unsynced-20260510T040255Z
TCFS_BIN=/tmp/tcfs-fleet-pilot-build-20260509T1907Z/target/debug/tcfs
HONEY_ROOT_RAW='/tmp/tcfs-delete-rename-unsynced-20260510T040255Z-55529-honey/root'
RUN_DIR=/tmp/tcfs-delete-rename-unsynced-20260510T040255Z-55529-honey/run
DELETE_FILE=Projects/shared/delete-me.md
RENAME_OLD_FILE=Projects/shared/rename-old.md
RENAME_NEW_FILE=Projects/shared/rename-new.md
DELETE_CONTENT="${TCFS_HONEY_DELETE_CONTENT_FILE:-/tmp/tcfs-delete-rename-unsynced-20260510T040255Z-55529-honey/run/delete-initial-content.txt}"
RENAME_CONTENT="${TCFS_HONEY_RENAME_CONTENT_FILE:-/tmp/tcfs-delete-rename-unsynced-20260510T040255Z-55529-honey/run/rename-content.txt}"

case "$HONEY_ROOT_RAW" in
  "~/"*) HONEY_ROOT="${HOME}/${HONEY_ROOT_RAW#\~/}" ;;
  *) HONEY_ROOT="$HONEY_ROOT_RAW" ;;
esac

if [[ -n "${TCFS_HONEY_ENV_FILE:-}" ]]; then
  # shellcheck disable=SC1090
  source "$TCFS_HONEY_ENV_FILE"
fi

mode="${1:-}"
[[ -n "$mode" ]] || { echo "mode required: prepare-unsync, verify-delete, or verify-rename" >&2; exit 2; }

STATE_DIR="$RUN_DIR/honey-state"
CACHE_ROOT="$STATE_DIR/cache"
EVIDENCE_DIR="$RUN_DIR/honey-evidence"
CONFIG_PATH="$STATE_DIR/tcfs-delete-rename-unsynced.toml"
STATE_JSON="$STATE_DIR/state.json"
DELETE_PATH="$HONEY_ROOT/$DELETE_FILE"
RENAME_OLD_PATH="$HONEY_ROOT/$RENAME_OLD_FILE"
RENAME_NEW_PATH="$HONEY_ROOT/$RENAME_NEW_FILE"
DELETE_STUB="${DELETE_PATH}.tc"
RENAME_OLD_STUB="${RENAME_OLD_PATH}.tc"
RENAME_NEW_STUB="${RENAME_NEW_PATH}.tc"

mkdir -p "$(dirname "$DELETE_PATH")" "$CACHE_ROOT" "$EVIDENCE_DIR"

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
    "$TCFS_BIN" --config "$CONFIG_PATH" pull "$DELETE_FILE" "$DELETE_PATH" --prefix "$PREFIX" --state "$STATE_JSON" >"$EVIDENCE_DIR/honey-delete-initial-pull.log" 2>&1
    cmp -s "$DELETE_CONTENT" "$DELETE_PATH"
    "$TCFS_BIN" --config "$CONFIG_PATH" pull "$RENAME_OLD_FILE" "$RENAME_OLD_PATH" --prefix "$PREFIX" --state "$STATE_JSON" >"$EVIDENCE_DIR/honey-rename-initial-pull.log" 2>&1
    cmp -s "$RENAME_CONTENT" "$RENAME_OLD_PATH"
    "$TCFS_BIN" --config "$CONFIG_PATH" unsync "$DELETE_PATH" >"$EVIDENCE_DIR/honey-delete-unsync.out" 2>&1
    "$TCFS_BIN" --config "$CONFIG_PATH" unsync "$RENAME_OLD_PATH" >"$EVIDENCE_DIR/honey-rename-unsync.out" 2>&1
    [[ ! -f "$DELETE_PATH" && -f "$DELETE_STUB" ]] || { echo "delete target did not become stub-only" >&2; exit 1; }
    [[ ! -f "$RENAME_OLD_PATH" && -f "$RENAME_OLD_STUB" ]] || { echo "rename target did not become stub-only" >&2; exit 1; }
    "$TCFS_BIN" --config "$CONFIG_PATH" sync-status "$DELETE_PATH" >"$EVIDENCE_DIR/honey-delete-status-after-unsync.out" 2>&1
    "$TCFS_BIN" --config "$CONFIG_PATH" sync-status "$RENAME_OLD_PATH" >"$EVIDENCE_DIR/honey-rename-status-after-unsync.out" 2>&1
    grep -q "sync state: not_synced" "$EVIDENCE_DIR/honey-delete-status-after-unsync.out"
    grep -q "sync state: not_synced" "$EVIDENCE_DIR/honey-rename-status-after-unsync.out"
    echo "honey delete/rename prepare unsync ok"
    ;;
  verify-delete)
    if "$TCFS_BIN" --config "$CONFIG_PATH" pull "$DELETE_FILE" "$DELETE_PATH" --prefix "$PREFIX" --state "$STATE_JSON" >"$EVIDENCE_DIR/honey-delete-pull-after-peer-delete.log" 2>&1; then
      echo "delete old path unexpectedly rehydrated" >&2
      exit 1
    fi
    "$TCFS_BIN" --config "$CONFIG_PATH" sync-status "$DELETE_PATH" >"$EVIDENCE_DIR/honey-delete-status-after-peer-delete.out" 2>&1 || true
    if [[ -e "$DELETE_STUB" ]]; then
      delete_stub_after_failed_pull=present
    else
      delete_stub_after_failed_pull=absent
    fi
    {
      echo "delete_old_pull=failed_as_expected"
      echo "delete_stub_after_failed_pull=$delete_stub_after_failed_pull"
      echo "delete_stub_path=$DELETE_STUB"
    } >"$EVIDENCE_DIR/honey-delete-peer-result.env"
    echo "honey peer-delete verify ok: $DELETE_FILE"
    echo "delete_old_pull=failed_as_expected"
    echo "delete_stub_after_failed_pull=$delete_stub_after_failed_pull"
    ;;
  verify-rename)
    if "$TCFS_BIN" --config "$CONFIG_PATH" pull "$RENAME_OLD_FILE" "$RENAME_OLD_PATH" --prefix "$PREFIX" --state "$STATE_JSON" >"$EVIDENCE_DIR/honey-rename-old-pull-after-peer-rename.log" 2>&1; then
      echo "rename old path unexpectedly rehydrated" >&2
      exit 1
    fi
    "$TCFS_BIN" --config "$CONFIG_PATH" pull "$RENAME_NEW_FILE" "$RENAME_NEW_PATH" --prefix "$PREFIX" --state "$STATE_JSON" >"$EVIDENCE_DIR/honey-rename-new-pull.log" 2>&1
    cmp -s "$RENAME_CONTENT" "$RENAME_NEW_PATH"
    "$TCFS_BIN" --config "$CONFIG_PATH" sync-status "$RENAME_NEW_PATH" >"$EVIDENCE_DIR/honey-rename-new-status.out" 2>&1
    grep -q "sync state: synced" "$EVIDENCE_DIR/honey-rename-new-status.out"
    if [[ -e "$RENAME_OLD_STUB" ]]; then
      rename_old_stub_after_new_pull=present
    else
      rename_old_stub_after_new_pull=absent
    fi
    if [[ -e "$RENAME_NEW_STUB" ]]; then
      rename_new_stub_after_pull=present
    else
      rename_new_stub_after_pull=absent
    fi
    {
      echo "rename_old_pull=failed_as_expected"
      echo "rename_new_pull=synced"
      echo "rename_old_stub_after_new_pull=$rename_old_stub_after_new_pull"
      echo "rename_new_stub_after_pull=$rename_new_stub_after_pull"
      echo "rename_old_stub_path=$RENAME_OLD_STUB"
      echo "rename_new_stub_path=$RENAME_NEW_STUB"
    } >"$EVIDENCE_DIR/honey-rename-peer-result.env"
    echo "honey peer-rename verify ok: $RENAME_OLD_FILE -> $RENAME_NEW_FILE"
    echo "rename_old_pull=failed_as_expected"
    echo "rename_new_pull=synced"
    echo "rename_old_stub_after_new_pull=$rename_old_stub_after_new_pull"
    ;;
  *)
    echo "unknown mode: $mode" >&2
    exit 2
    ;;
esac
