#!/usr/bin/env bash
set -euo pipefail

REMOTE=seaweedfs://100.64.48.53:8333/tcfs/home-canary-linux-xr-shadow-20260510T023938Z
TCFS_BIN=/tmp/tcfs-fleet-pilot-build-20260509T1907Z/target/debug/tcfs
MOUNT_ROOT_RAW='/tmp/tcfs-home-canary-linux-xr-shadow-20260510T023938Z-honey/mount'
case "$MOUNT_ROOT_RAW" in
  "~/"*) MOUNT_ROOT="${HOME}/${MOUNT_ROOT_RAW#\~/}" ;;
  *) MOUNT_ROOT="$MOUNT_ROOT_RAW" ;;
esac
SMOKE_SCRIPT="${TCFS_HONEY_SMOKE_SCRIPT:-/tmp/tcfs-home-canary-linux-xr-shadow-20260510T023938Z-honey/run/lazy-hydration-mounted-smoke.sh}"
EXPECTED_CONTENT_FILE="${TCFS_HONEY_EXPECTED_CONTENT_FILE:-/tmp/tcfs-home-canary-linux-xr-shadow-20260510T023938Z-honey/run/selected-hydration-file.content}"
MOUNT_LOG="${TCFS_HONEY_MOUNT_LOG:-/tmp/tcfs-home-canary-linux-xr-shadow-20260510T023938Z-honey/run/mount.log}"
SMOKE_MAX_DEPTH="${TCFS_HONEY_SMOKE_MAX_DEPTH:-3}"
SMOKE_TIMEOUT_SECS="${TCFS_HONEY_SMOKE_TIMEOUT_SECS:-300}"
EXPECTED_FILE=.clang-format

if [[ -n "${TCFS_HONEY_ENV_FILE:-}" ]]; then
  # shellcheck disable=SC1090
  source "$TCFS_HONEY_ENV_FILE"
fi

mkdir -p "$MOUNT_ROOT"
mount_started=0
cleanup_mount() {
  if [[ "$mount_started" == "1" && "${TCFS_HONEY_KEEP_MOUNT:-0}" != "1" ]]; then
    "$TCFS_BIN" unmount "$MOUNT_ROOT" >/dev/null 2>&1 || fusermount3 -u "$MOUNT_ROOT" >/dev/null 2>&1 || true
  fi
}
trap cleanup_mount EXIT

if [[ "${TCFS_HONEY_START_MOUNT:-0}" == "1" ]]; then
  nohup "$TCFS_BIN" mount "$REMOTE" "$MOUNT_ROOT" >"$MOUNT_LOG" 2>&1 &
  mount_pid="$!"
  mount_started=1
  for _ in {1..300}; do
    if mount | grep -F -- "$MOUNT_ROOT" >/dev/null 2>&1; then
      break
    fi
    if ! kill -0 "$mount_pid" 2>/dev/null; then
      tail -n 80 "$MOUNT_LOG" >&2 || true
      echo "tcfs mount exited before mountpoint became active" >&2
      exit 1
    fi
    if command -v perl >/dev/null 2>&1; then
      perl -e 'select undef, undef, undef, 0.1'
    else
      python3 -c 'import select; select.select([], [], [], 0.1)'
    fi
  done
fi

args=(
  --mount-root "$MOUNT_ROOT"
  --expect-entry .git
  --max-depth "$SMOKE_MAX_DEPTH"
)
if [[ -n "$EXPECTED_FILE" ]]; then
  args+=(--expected-file "$EXPECTED_FILE")
fi
if [[ -f "$EXPECTED_CONTENT_FILE" ]]; then
  args+=(--expected-content-file "$EXPECTED_CONTENT_FILE")
fi

if [[ "$SMOKE_TIMEOUT_SECS" != "0" && "$SMOKE_TIMEOUT_SECS" =~ ^[0-9]+$ ]] && command -v timeout >/dev/null 2>&1; then
  timeout "$SMOKE_TIMEOUT_SECS" bash "$SMOKE_SCRIPT" "${args[@]}"
else
  bash "$SMOKE_SCRIPT" "${args[@]}"
fi
