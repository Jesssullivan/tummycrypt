#!/usr/bin/env bash
set -euo pipefail

REMOTE=seaweedfs://100.64.48.53:8333/tcfs/tcfs-symlink-mounted-probe-20260515T051126Z-nix_current
TCFS_BIN=/tmp/tcfs-current-srcbin-a76d48db3e06/bin/tcfs
MOUNT_ROOT=/tmp/tcfs-symlink-mounted-probe-20260515T051126Z/mount-nix_current
SMOKE_SCRIPT=/tmp/tcfs-symlink-mounted-probe-20260515T051126Z/lazy-hydration-mounted-smoke.sh
EXPECTED_CONTENT_FILE=/tmp/tcfs-symlink-mounted-probe-20260515T051126Z/target.txt.expected
SYMLINK_TARGETS_FILE=/tmp/tcfs-symlink-mounted-probe-20260515T051126Z/symlink-targets.tsv
MOUNT_LOG=/tmp/tcfs-symlink-mounted-probe-20260515T051126Z/nix_current.mount.log
SMOKE_TIMEOUT_SECS=180

if [[ -n "${TCFS_HONEY_ENV_FILE:-}" ]]; then
  # shellcheck disable=SC1090
  source "$TCFS_HONEY_ENV_FILE"
fi

echo "tcfs binary requested: $TCFS_BIN"
tcfs_resolved="$TCFS_BIN"
if command -v "$TCFS_BIN" >/dev/null 2>&1; then
  tcfs_resolved="$(command -v "$TCFS_BIN")"
elif [[ -x "$TCFS_BIN" ]]; then
  tcfs_resolved="$TCFS_BIN"
else
  printf 'tcfs binary is not executable or on PATH: %s\n' "$TCFS_BIN" >&2
  exit 1
fi
echo "tcfs binary resolved: $tcfs_resolved"
tcfs_version="$("$tcfs_resolved" --version 2>&1)" || {
  printf 'failed to run tcfs --version through %s\n' "$tcfs_resolved" >&2
  printf '%s\n' "$tcfs_version" >&2
  exit 1
}
echo "tcfs version: $tcfs_version"
if command -v sha256sum >/dev/null 2>&1; then
  echo "tcfs sha256: $(sha256sum "$tcfs_resolved" | awk '{print $1}')"
elif command -v shasum >/dev/null 2>&1; then
  echo "tcfs sha256: $(shasum -a 256 "$tcfs_resolved" | awk '{print $1}')"
fi

mkdir -p "$MOUNT_ROOT"
mount_started=0
cleanup_mount() {
  if [[ "$mount_started" == "1" && "${TCFS_HONEY_KEEP_MOUNT:-0}" != "1" ]]; then
    "$tcfs_resolved" unmount "$MOUNT_ROOT" >/dev/null 2>&1 || fusermount3 -u "$MOUNT_ROOT" >/dev/null 2>&1 || true
  fi
}
trap cleanup_mount EXIT

nohup "$tcfs_resolved" mount "$REMOTE" "$MOUNT_ROOT" >"$MOUNT_LOG" 2>&1 &
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

if ! mount | grep -F -- "$MOUNT_ROOT" >/dev/null 2>&1; then
  tail -n 80 "$MOUNT_LOG" >&2 || true
  echo "tcfs mount did not become active" >&2
  exit 1
fi

args=(
  --mount-root "$MOUNT_ROOT"
  --expected-file target.txt
  --expect-entry link.txt
  --expected-content-file "$EXPECTED_CONTENT_FILE"
  --expected-symlink-targets-file "$SYMLINK_TARGETS_FILE"
  --max-depth 2
)

if [[ "$SMOKE_TIMEOUT_SECS" != "0" && "$SMOKE_TIMEOUT_SECS" =~ ^[0-9]+$ ]] && command -v timeout >/dev/null 2>&1; then
  timeout "$SMOKE_TIMEOUT_SECS" bash "$SMOKE_SCRIPT" "${args[@]}"
else
  bash "$SMOKE_SCRIPT" "${args[@]}"
fi
