#!/usr/bin/env bash
set -euo pipefail

REMOTE=seaweedfs://100.64.48.53:8333/tcfs/honey-mounted-reverse-read-20260510T042203Z
ENDPOINT=http://100.64.48.53:8333
REGION=us-east-1
BUCKET=tcfs
PREFIX=honey-mounted-reverse-read-20260510T042203Z
TCFS_BIN=/tmp/tcfs-fleet-pilot-build-20260509T1907Z/target/debug/tcfs
HONEY_ROOT_RAW='/tmp/tcfs-reverse-unsynced-rehydrate-20260510T042204Z-92360-honey/root'
MOUNT_ROOT_RAW='/tmp/tcfs-reverse-unsynced-rehydrate-20260510T042204Z-92360-honey/mount'
RUN_DIR=/tmp/tcfs-reverse-unsynced-rehydrate-20260510T042204Z-92360-honey/run
FIXTURE_FILE=Projects/shared/reverse-notes.md
INITIAL_CONTENT="${TCFS_HONEY_INITIAL_CONTENT_FILE:-/tmp/tcfs-reverse-unsynced-rehydrate-20260510T042204Z-92360-honey/run/neo-initial-content.txt}"
MUTATED_CONTENT="${TCFS_HONEY_MUTATED_CONTENT_FILE:-/tmp/tcfs-reverse-unsynced-rehydrate-20260510T042204Z-92360-honey/run/neo-mutated-content.txt}"
SMOKE_SCRIPT="${TCFS_HONEY_SMOKE_SCRIPT:-/tmp/tcfs-reverse-unsynced-rehydrate-20260510T042204Z-92360-honey/run/lazy-hydration-mounted-smoke.sh}"
START_MOUNT="${TCFS_HONEY_START_MOUNT:-0}"
MOUNT_LOG="${TCFS_HONEY_MOUNT_LOG:-/tmp/tcfs-reverse-unsynced-rehydrate-20260510T042204Z-92360-honey/run/honey-mount.log}"

case "$HONEY_ROOT_RAW" in
  "~/"*) HONEY_ROOT="${HOME}/${HONEY_ROOT_RAW#\~/}" ;;
  *) HONEY_ROOT="$HONEY_ROOT_RAW" ;;
esac
case "$MOUNT_ROOT_RAW" in
  "~/"*) MOUNT_ROOT="${HOME}/${MOUNT_ROOT_RAW#\~/}" ;;
  *) MOUNT_ROOT="$MOUNT_ROOT_RAW" ;;
esac

if [[ -n "${TCFS_HONEY_ENV_FILE:-}" ]]; then
  # shellcheck disable=SC1090
  source "$TCFS_HONEY_ENV_FILE"
fi
if [[ -n "${TCFS_HONEY_MOUNT_ROOT:-}" ]]; then
  MOUNT_ROOT_RAW="$TCFS_HONEY_MOUNT_ROOT"
fi
case "$MOUNT_ROOT_RAW" in
  "~/"*) MOUNT_ROOT="${HOME}/${MOUNT_ROOT_RAW#\~/}" ;;
  *) MOUNT_ROOT="$MOUNT_ROOT_RAW" ;;
esac

mode="${1:-}"
[[ -n "$mode" ]] || { echo "mode required: prepare-unsync, rehydrate, or mounted-read" >&2; exit 2; }

STATE_DIR="$RUN_DIR/honey-state"
CACHE_ROOT="$STATE_DIR/cache"
EVIDENCE_DIR="$RUN_DIR/honey-evidence"
CONFIG_PATH="$STATE_DIR/tcfs-reverse-unsynced-rehydrate.toml"
STATE_JSON="$STATE_DIR/state.json"
FIXTURE_PATH="$HONEY_ROOT/$FIXTURE_FILE"
STUB_PATH="${FIXTURE_PATH}.tc"
MOUNT_STARTED=0

cleanup_mount() {
  if [[ "$MOUNT_STARTED" == "1" && "${TCFS_HONEY_KEEP_MOUNT:-0}" != "1" ]]; then
    "$TCFS_BIN" unmount "$MOUNT_ROOT" >/dev/null 2>&1 || fusermount3 -u "$MOUNT_ROOT" >/dev/null 2>&1 || true
  fi
}
trap cleanup_mount EXIT

mkdir -p "$(dirname "$FIXTURE_PATH")" "$CACHE_ROOT" "$EVIDENCE_DIR" "$MOUNT_ROOT"

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
  mounted-read)
    if [[ "$START_MOUNT" == "1" ]]; then
      nohup "$TCFS_BIN" mount "$REMOTE" "$MOUNT_ROOT" >"$MOUNT_LOG" 2>&1 &
      mount_pid="$!"
      MOUNT_STARTED=1
      for _ in {1..300}; do
        if mount | grep -F -- "$MOUNT_ROOT" >/dev/null 2>&1; then
          break
        fi
        if ! kill -0 "$mount_pid" 2>/dev/null; then
          tail -n 80 "$MOUNT_LOG" >&2 || true
          echo "honey tcfs mount exited before mountpoint became active" >&2
          exit 1
        fi
        if command -v perl >/dev/null 2>&1; then
          perl -e 'select undef, undef, undef, 0.1'
        else
          python3 -c 'import select; select.select([], [], [], 0.1)'
        fi
      done
      if ! mount | grep -F -- "$MOUNT_ROOT" >/dev/null 2>&1; then
        tail -n 80 "$MOUNT_LOG" >&2 || true
        echo "timed out waiting for honey mount: $MOUNT_ROOT" >&2
        exit 1
      fi
    fi
    bash "$SMOKE_SCRIPT"       --mount-root "$MOUNT_ROOT"       --expected-file "$FIXTURE_FILE"       --expected-content-file "$MUTATED_CONTENT"       --expect-entry Projects       --expect-entry Projects/shared       --max-depth 8 >"$EVIDENCE_DIR/honey-mounted-read.log" 2>&1
    if [[ -e "$STUB_PATH" && ! -e "$FIXTURE_PATH" ]]; then
      physical_state="stub_present"
    else
      physical_state="unexpected"
    fi
    {
      echo "honey_physical_after_mounted_read=$physical_state"
      echo "stub_path=$STUB_PATH"
      echo "hydrated_path=$FIXTURE_PATH"
    } >"$EVIDENCE_DIR/honey-physical-stub-after-mounted-read.env"
    [[ "$physical_state" == "stub_present" ]] || {
      echo "honey physical root did not remain stub-only after mounted read" >&2
      exit 1
    }
    echo "honey reverse mounted read ok: $FIXTURE_FILE"
    echo "honey_physical_after_mounted_read=stub_present"
    ;;
  *)
    echo "unknown mode: $mode" >&2
    exit 2
    ;;
esac
