#!/usr/bin/env bash
set -euo pipefail

ENDPOINT=http://100.64.48.53:8333
REGION=us-east-1
BUCKET=tcfs
PREFIX=neo-honey-conflict-keep-both-20260510T045908Z
TCFS_BIN=/tmp/tcfs-fleet-pilot-build-20260509T1907Z/target/debug/tcfs
HONEY_ROOT_RAW='/tmp/tcfs-neo-honey-conflict-20260510T045910Z-48352-honey/root'
RUN_DIR=/tmp/tcfs-neo-honey-conflict-20260510T045910Z-48352-honey/run
FIXTURE_FILE=Projects/shared/conflict-notes.md
CONFLICT_COPY_FILE=Projects/shared/conflict-notes.conflict-honey.md
BASE_CONTENT="${TCFS_HONEY_BASE_CONTENT_FILE:-/tmp/tcfs-neo-honey-conflict-20260510T045910Z-48352-honey/run/base-content.txt}"
NEO_CONTENT="${TCFS_HONEY_NEO_CONTENT_FILE:-/tmp/tcfs-neo-honey-conflict-20260510T045910Z-48352-honey/run/neo-conflict-content.txt}"
HONEY_CONTENT="${TCFS_HONEY_CONFLICT_CONTENT_FILE:-/tmp/tcfs-neo-honey-conflict-20260510T045910Z-48352-honey/run/honey-conflict-content.txt}"

case "$HONEY_ROOT_RAW" in
  "~/"*) HONEY_ROOT="${HOME}/${HONEY_ROOT_RAW#\~/}" ;;
  *) HONEY_ROOT="$HONEY_ROOT_RAW" ;;
esac

if [[ -n "${TCFS_HONEY_ENV_FILE:-}" ]]; then
  # shellcheck disable=SC1090
  source "$TCFS_HONEY_ENV_FILE"
fi

mode="${1:-}"
[[ -n "$mode" ]] || { echo "mode required: prepare, push-conflict, or recover-keep-both" >&2; exit 2; }

STATE_DIR="$RUN_DIR/honey-state"
CACHE_ROOT="$STATE_DIR/cache"
EVIDENCE_DIR="$RUN_DIR/honey-evidence"
CONFIG_PATH="$STATE_DIR/tcfs-neo-honey-conflict.toml"
STATE_JSON="$STATE_DIR/state.json"
DEVICE_REGISTRY="$RUN_DIR/device-registry.json"
FIXTURE_PATH="$HONEY_ROOT/$FIXTURE_FILE"
CONFLICT_COPY_PATH="$HONEY_ROOT/$CONFLICT_COPY_FILE"

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
device_identity = "$DEVICE_REGISTRY"
device_name = "honey-conflict"

[fuse]
cache_dir = "$CACHE_ROOT"
cache_max_mb = 64
negative_cache_ttl_secs = 1

[crypto]
enabled = false
REMOTE_CONFIG

case "$mode" in
  prepare)
    "$TCFS_BIN" --config "$CONFIG_PATH" pull "$FIXTURE_FILE" "$FIXTURE_PATH" --prefix "$PREFIX" --state "$STATE_JSON" >"$EVIDENCE_DIR/honey-initial-pull.log" 2>&1
    cmp -s "$BASE_CONTENT" "$FIXTURE_PATH"
    cp "$HONEY_CONTENT" "$FIXTURE_PATH"
    "$TCFS_BIN" --config "$CONFIG_PATH" sync-status "$FIXTURE_PATH" >"$EVIDENCE_DIR/honey-sync-status-before-conflict.out" 2>&1
    echo "honey conflict prepare ok: $FIXTURE_FILE"
    ;;
  push-conflict)
    set +e
    "$TCFS_BIN" --config "$CONFIG_PATH" push "$FIXTURE_PATH" --prefix "$PREFIX" --state "$STATE_JSON" >"$EVIDENCE_DIR/honey-conflict-push.log" 2>&1
    push_rc="$?"
    set -e
    cat "$EVIDENCE_DIR/honey-conflict-push.log"
    [[ "$push_rc" -eq 0 ]] || { echo "honey conflict push command failed: $push_rc" >&2; exit "$push_rc"; }
    grep -q "CONFLICT:" "$EVIDENCE_DIR/honey-conflict-push.log"
    grep -q "skipped (unchanged since last sync)" "$EVIDENCE_DIR/honey-conflict-push.log"
    cmp -s "$HONEY_CONTENT" "$FIXTURE_PATH"
    "$TCFS_BIN" --config "$CONFIG_PATH" sync-status "$FIXTURE_PATH" >"$EVIDENCE_DIR/honey-sync-status-after-conflict.out" 2>&1
    grep -q "sync state: conflict" "$EVIDENCE_DIR/honey-sync-status-after-conflict.out"
    {
      echo "honey_push_conflict=detected"
      echo "honey_local_content=preserved"
      echo "honey_sync_state=conflict"
    } >"$EVIDENCE_DIR/honey-conflict-result.env"
    echo "honey conflict push ok: $FIXTURE_FILE"
    echo "honey_push_conflict=detected"
    echo "honey_local_content=preserved"
    echo "honey_sync_state=conflict"
    ;;
  recover-keep-both)
    [[ -f "$NEO_CONTENT" ]] || { echo "missing neo content fixture: $NEO_CONTENT" >&2; exit 2; }
    "$TCFS_BIN" --config "$CONFIG_PATH" sync-status "$FIXTURE_PATH" >"$EVIDENCE_DIR/honey-sync-status-before-recovery.out" 2>&1
    grep -q "sync state: conflict" "$EVIDENCE_DIR/honey-sync-status-before-recovery.out"
    mkdir -p "$(dirname "$CONFLICT_COPY_PATH")"
    cp "$FIXTURE_PATH" "$CONFLICT_COPY_PATH"
    cmp -s "$HONEY_CONTENT" "$CONFLICT_COPY_PATH"
    "$TCFS_BIN" --config "$CONFIG_PATH" pull "$FIXTURE_FILE" "$FIXTURE_PATH" --prefix "$PREFIX" --state "$STATE_JSON" >"$EVIDENCE_DIR/honey-recover-original-pull.log" 2>&1
    cmp -s "$NEO_CONTENT" "$FIXTURE_PATH"
    "$TCFS_BIN" --config "$CONFIG_PATH" sync-status "$FIXTURE_PATH" >"$EVIDENCE_DIR/honey-sync-status-after-original-recovery.out" 2>&1
    grep -q "sync state: synced" "$EVIDENCE_DIR/honey-sync-status-after-original-recovery.out"
    "$TCFS_BIN" --config "$CONFIG_PATH" push "$CONFLICT_COPY_PATH" --prefix "$PREFIX" --state "$STATE_JSON" >"$EVIDENCE_DIR/honey-recover-copy-push.log" 2>&1
    cmp -s "$HONEY_CONTENT" "$CONFLICT_COPY_PATH"
    "$TCFS_BIN" --config "$CONFIG_PATH" sync-status "$CONFLICT_COPY_PATH" >"$EVIDENCE_DIR/honey-sync-status-after-copy-push.out" 2>&1
    grep -q "sync state: synced" "$EVIDENCE_DIR/honey-sync-status-after-copy-push.out"
    {
      echo "keep_both_recovery=completed"
      echo "original_path_after_recovery=remote_neo_bytes"
      echo "conflict_copy_path=$CONFLICT_COPY_FILE"
      echo "conflict_copy_content=honey_bytes"
      echo "conflict_copy_pushed=1"
    } >"$EVIDENCE_DIR/honey-recovery-result.env"
    echo "honey keep-both recovery ok: $FIXTURE_FILE -> $CONFLICT_COPY_FILE"
    echo "keep_both_recovery=completed"
    echo "original_path_after_recovery=remote_neo_bytes"
    echo "conflict_copy_path=$CONFLICT_COPY_FILE"
    echo "conflict_copy_content=honey_bytes"
    echo "conflict_copy_pushed=1"
    ;;
  *)
    echo "unknown mode: $mode" >&2
    exit 2
    ;;
esac
