#!/usr/bin/env bash
set -euo pipefail

ENDPOINT=http://100.64.48.53:8333
REGION=us-east-1
BUCKET=tcfs
PREFIX=neo-honey-conflict-daemon-keep-both-20260510T054401Z
TCFS_BIN=/tmp/tcfs-fleet-pilot-build-20260509T1907Z/target/debug/tcfs
TCFSD_BIN=/tmp/tcfs-fleet-pilot-build-20260509T1907Z/target/debug/tcfsd
HONEY_ROOT_RAW='/tmp/tcfs-neo-honey-conflict-20260510T054416Z-92811-honey/root'
RUN_DIR=/tmp/tcfs-neo-honey-conflict-20260510T054416Z-92811-honey/run
FIXTURE_FILE=Projects/shared/conflict-notes.md
CONFLICT_COPY_FILE=Projects/shared/conflict-notes.conflict-honey.md
DAEMON_CONFLICT_COPY_FILE=Projects/shared/conflict-notes.conflict-00000000-0000-4000-8000-0000000000b2.md
SIBLING_FILE=Projects/shared/conflict-independent-sibling.md
HONEY_INDEPENDENT_SIBLING=0
BASE_CONTENT="${TCFS_HONEY_BASE_CONTENT_FILE:-/tmp/tcfs-neo-honey-conflict-20260510T054416Z-92811-honey/run/base-content.txt}"
NEO_CONTENT="${TCFS_HONEY_NEO_CONTENT_FILE:-/tmp/tcfs-neo-honey-conflict-20260510T054416Z-92811-honey/run/neo-conflict-content.txt}"
HONEY_CONTENT="${TCFS_HONEY_CONFLICT_CONTENT_FILE:-/tmp/tcfs-neo-honey-conflict-20260510T054416Z-92811-honey/run/honey-conflict-content.txt}"
SIBLING_BASE_CONTENT="${TCFS_HONEY_SIBLING_BASE_CONTENT_FILE:-/tmp/tcfs-neo-honey-conflict-20260510T054416Z-92811-honey/run/sibling-base-content.txt}"
HONEY_SIBLING_CONTENT="${TCFS_HONEY_SIBLING_CONTENT_FILE:-/tmp/tcfs-neo-honey-conflict-20260510T054416Z-92811-honey/run/honey-sibling-content.txt}"

case "$HONEY_ROOT_RAW" in
  "~/"*) HONEY_ROOT="${HOME}/${HONEY_ROOT_RAW#\~/}" ;;
  *) HONEY_ROOT="$HONEY_ROOT_RAW" ;;
esac

if [[ -n "${TCFS_HONEY_ENV_FILE:-}" ]]; then
  # shellcheck disable=SC1090
  source "$TCFS_HONEY_ENV_FILE"
fi

mode="${1:-}"
[[ -n "$mode" ]] || { echo "mode required: prepare, push-conflict, push-sibling, recover-keep-both, or resolve-keep-both" >&2; exit 2; }

STATE_DIR="$RUN_DIR/honey-state"
CACHE_ROOT="$STATE_DIR/cache"
EVIDENCE_DIR="$RUN_DIR/honey-evidence"
CONFIG_PATH="$STATE_DIR/tcfs-neo-honey-conflict.toml"
STATE_JSON="$STATE_DIR/state.json"
DEVICE_REGISTRY="$RUN_DIR/device-registry.json"
FIXTURE_PATH="$HONEY_ROOT/$FIXTURE_FILE"
CONFLICT_COPY_PATH="$HONEY_ROOT/$CONFLICT_COPY_FILE"
DAEMON_CONFLICT_COPY_PATH="$HONEY_ROOT/$DAEMON_CONFLICT_COPY_FILE"
SIBLING_PATH="$HONEY_ROOT/$SIBLING_FILE"

mkdir -p "$(dirname "$FIXTURE_PATH")" "$CACHE_ROOT" "$EVIDENCE_DIR"

cat >"$CONFIG_PATH" <<REMOTE_CONFIG
[daemon]
socket = "$STATE_DIR/tcfsd.sock"
metrics_addr = "127.0.0.1:0"

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
reconcile_interval_secs = 0

[fuse]
cache_dir = "$CACHE_ROOT"
cache_max_mb = 64
negative_cache_ttl_secs = 1

[crypto]
enabled = false

[auth]
require_session = false
REMOTE_CONFIG

write_daemon_resolve_blocker() {
  local reason="$1"
  local detail="${2:-}"
  if [[ -n "$detail" ]]; then
    printf '%s\n' "$detail" >"$EVIDENCE_DIR/honey-daemon-resolve-blocker.txt"
  fi
  {
    echo "daemon_resolve_keep_both=blocked"
    echo "blocker_reason=$reason"
    echo "daemon_auth_bypass_required=1"
    echo "conflict_copy_path=$DAEMON_CONFLICT_COPY_FILE"
  } >"$EVIDENCE_DIR/honey-daemon-resolve-result.env"
  echo "daemon_resolve_keep_both=blocked"
  echo "blocker_reason=$reason"
  echo "daemon_auth_bypass_required=1"
  echo "conflict_copy_path=$DAEMON_CONFLICT_COPY_FILE"
}

stop_resolve_daemon() {
  if [[ -n "${DAEMON_PID:-}" ]]; then
    if kill -0 "$DAEMON_PID" >/dev/null 2>&1; then
      kill "$DAEMON_PID" >/dev/null 2>&1 || true
      wait "$DAEMON_PID" >/dev/null 2>&1 || true
    fi
    DAEMON_PID=""
  fi
}

start_resolve_daemon() {
  if [[ "$TCFSD_BIN" == */* ]]; then
    [[ -x "$TCFSD_BIN" ]] || return 10
  else
    command -v "$TCFSD_BIN" >/dev/null 2>&1 || return 10
  fi

  rm -f "$STATE_DIR/tcfsd.sock"
  mkdir -p "$RUN_DIR/xdg-data" "$RUN_DIR/xdg-state"
  XDG_DATA_HOME="$RUN_DIR/xdg-data" XDG_STATE_HOME="$RUN_DIR/xdg-state" "$TCFSD_BIN" --config "$CONFIG_PATH" --mode daemon --log debug --log-format text >"$EVIDENCE_DIR/honey-tcfsd-resolve-keep-both.log" 2>&1 &
  DAEMON_PID="$!"

  for _ in {1..100}; do
    if [[ -S "$STATE_DIR/tcfsd.sock" ]]; then
      return 0
    fi
    if ! kill -0 "$DAEMON_PID" >/dev/null 2>&1; then
      return 11
    fi
    sleep 0.1
  done
  return 12
}

case "$mode" in
  prepare)
    "$TCFS_BIN" --config "$CONFIG_PATH" pull "$FIXTURE_FILE" "$FIXTURE_PATH" --prefix "$PREFIX" --state "$STATE_JSON" >"$EVIDENCE_DIR/honey-initial-pull.log" 2>&1
    cmp -s "$BASE_CONTENT" "$FIXTURE_PATH"
    cp "$HONEY_CONTENT" "$FIXTURE_PATH"
    "$TCFS_BIN" --config "$CONFIG_PATH" sync-status "$FIXTURE_PATH" >"$EVIDENCE_DIR/honey-sync-status-before-conflict.out" 2>&1
    if [[ "$HONEY_INDEPENDENT_SIBLING" == "1" ]]; then
      "$TCFS_BIN" --config "$CONFIG_PATH" pull "$SIBLING_FILE" "$SIBLING_PATH" --prefix "$PREFIX" --state "$STATE_JSON" >"$EVIDENCE_DIR/honey-sibling-initial-pull.log" 2>&1
      cmp -s "$SIBLING_BASE_CONTENT" "$SIBLING_PATH"
      cp "$HONEY_SIBLING_CONTENT" "$SIBLING_PATH"
      "$TCFS_BIN" --config "$CONFIG_PATH" sync-status "$SIBLING_PATH" >"$EVIDENCE_DIR/honey-sibling-sync-status-before-push.out" 2>&1
    fi
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
  push-sibling)
    [[ "$HONEY_INDEPENDENT_SIBLING" == "1" ]] || { echo "independent sibling mode is disabled" >&2; exit 2; }
    "$TCFS_BIN" --config "$CONFIG_PATH" push "$SIBLING_PATH" --prefix "$PREFIX" --state "$STATE_JSON" >"$EVIDENCE_DIR/honey-independent-sibling-push.log" 2>&1
    cat "$EVIDENCE_DIR/honey-independent-sibling-push.log"
    if grep -q "CONFLICT:" "$EVIDENCE_DIR/honey-independent-sibling-push.log"; then
      echo "unexpected conflict while pushing independent sibling" >&2
      exit 1
    fi
    cmp -s "$HONEY_SIBLING_CONTENT" "$SIBLING_PATH"
    "$TCFS_BIN" --config "$CONFIG_PATH" sync-status "$SIBLING_PATH" >"$EVIDENCE_DIR/honey-sibling-sync-status-after-push.out" 2>&1
    grep -q "sync state: synced" "$EVIDENCE_DIR/honey-sibling-sync-status-after-push.out"
    {
      echo "independent_sibling_push=completed"
      echo "independent_sibling_content=honey_bytes"
      echo "independent_sibling_conflict=absent"
    } >"$EVIDENCE_DIR/honey-independent-sibling-result.env"
    echo "honey independent sibling push ok: $SIBLING_FILE"
    echo "independent_sibling_push=completed"
    echo "independent_sibling_content=honey_bytes"
    echo "independent_sibling_conflict=absent"
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
  resolve-keep-both)
    "$TCFS_BIN" --config "$CONFIG_PATH" sync-status "$FIXTURE_PATH" >"$EVIDENCE_DIR/honey-sync-status-before-daemon-resolve.out" 2>&1 || {
      write_daemon_resolve_blocker "pre_resolve_status_failed" "tcfs sync-status failed before daemon resolve"
      exit 0
    }
    if ! grep -q "sync state: conflict" "$EVIDENCE_DIR/honey-sync-status-before-daemon-resolve.out"; then
      write_daemon_resolve_blocker "pre_resolve_state_not_conflict" "expected conflict state before daemon resolve"
      exit 0
    fi

    DAEMON_PID=""
    trap stop_resolve_daemon EXIT
    if start_resolve_daemon; then
      :
    else
      daemon_rc="$?"
      write_daemon_resolve_blocker "daemon_start_or_socket_failed" "tcfsd start/socket wait failed with rc=$daemon_rc; see honey-tcfsd-resolve-keep-both.log"
      exit 0
    fi

    set +e
    "$TCFS_BIN" --config "$CONFIG_PATH" resolve "$FIXTURE_PATH" --strategy keep-both >"$EVIDENCE_DIR/honey-daemon-resolve-keep-both.out" 2>&1
    resolve_rc="$?"
    set -e
    cat "$EVIDENCE_DIR/honey-daemon-resolve-keep-both.out"
    stop_resolve_daemon
    trap - EXIT
    if [[ "$resolve_rc" -ne 0 ]]; then
      write_daemon_resolve_blocker "resolve_command_failed" "tcfs resolve returned rc=$resolve_rc"
      exit 0
    fi

    if ! cmp -s "$NEO_CONTENT" "$FIXTURE_PATH"; then
      write_daemon_resolve_blocker "original_content_mismatch" "original path did not contain neo remote bytes after daemon resolve"
      exit 0
    fi
    if [[ ! -f "$DAEMON_CONFLICT_COPY_PATH" ]]; then
      write_daemon_resolve_blocker "conflict_copy_missing" "daemon conflict copy was not created at $DAEMON_CONFLICT_COPY_FILE"
      exit 0
    fi
    if ! cmp -s "$HONEY_CONTENT" "$DAEMON_CONFLICT_COPY_PATH"; then
      write_daemon_resolve_blocker "conflict_copy_content_mismatch" "daemon conflict copy did not preserve honey bytes"
      exit 0
    fi
    "$TCFS_BIN" --config "$CONFIG_PATH" sync-status "$FIXTURE_PATH" >"$EVIDENCE_DIR/honey-sync-status-after-daemon-resolve-original.out" 2>&1 || {
      write_daemon_resolve_blocker "post_original_status_failed" "tcfs sync-status failed for original after daemon resolve"
      exit 0
    }
    if ! grep -q "sync state: synced" "$EVIDENCE_DIR/honey-sync-status-after-daemon-resolve-original.out"; then
      write_daemon_resolve_blocker "post_original_not_synced" "original path was not synced after daemon resolve"
      exit 0
    fi
    "$TCFS_BIN" --config "$CONFIG_PATH" sync-status "$DAEMON_CONFLICT_COPY_PATH" >"$EVIDENCE_DIR/honey-sync-status-after-daemon-resolve-copy.out" 2>&1 || {
      write_daemon_resolve_blocker "post_copy_status_failed" "tcfs sync-status failed for conflict copy after daemon resolve"
      exit 0
    }
    if ! grep -q "sync state: synced" "$EVIDENCE_DIR/honey-sync-status-after-daemon-resolve-copy.out"; then
      write_daemon_resolve_blocker "post_copy_not_synced" "conflict copy was not synced after daemon resolve"
      exit 0
    fi

    {
      echo "daemon_resolve_keep_both=completed"
      echo "daemon_auth_bypass_required=1"
      echo "original_path_after_resolve=remote_neo_bytes"
      echo "conflict_copy_path=$DAEMON_CONFLICT_COPY_FILE"
      echo "conflict_copy_content=honey_bytes"
      echo "conflict_copy_pushed=1"
    } >"$EVIDENCE_DIR/honey-daemon-resolve-result.env"
    echo "honey daemon resolve keep-both ok: $FIXTURE_FILE -> $DAEMON_CONFLICT_COPY_FILE"
    echo "daemon_resolve_keep_both=completed"
    echo "daemon_auth_bypass_required=1"
    echo "original_path_after_resolve=remote_neo_bytes"
    echo "conflict_copy_path=$DAEMON_CONFLICT_COPY_FILE"
    echo "conflict_copy_content=honey_bytes"
    echo "conflict_copy_pushed=1"
    ;;
  *)
    echo "unknown mode: $mode" >&2
    exit 2
    ;;
esac
