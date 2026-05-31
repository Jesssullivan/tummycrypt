#!/usr/bin/env bash
set -euo pipefail

REMOTE=seaweedfs://localhost:8333/tcfs/large-workdir-20260526T000907Z
TCFS_BIN=tcfs
MOUNT_ROOT_RAW='/tmp/tcfs-home-canary-linux-xr-shadow-20260526T000908Z/mount'
case "$MOUNT_ROOT_RAW" in
  "~/"*) MOUNT_ROOT="${HOME}/${MOUNT_ROOT_RAW#\~/}" ;;
  *) MOUNT_ROOT="$MOUNT_ROOT_RAW" ;;
esac
SMOKE_SCRIPT="${TCFS_HONEY_SMOKE_SCRIPT:-/tmp/tcfs-home-canary-linux-xr-shadow-20260526T000908Z/lazy-hydration-mounted-smoke.sh}"
EXPECTED_CONTENT_FILE="${TCFS_HONEY_EXPECTED_CONTENT_FILE:-/tmp/tcfs-home-canary-linux-xr-shadow-20260526T000908Z/selected-hydration-file.content}"
SYMLINK_TARGETS_FILE="${TCFS_HONEY_SYMLINK_TARGETS_FILE:-/tmp/tcfs-home-canary-linux-xr-shadow-20260526T000908Z/symlink-targets.tsv}"
MOUNT_LOG="${TCFS_HONEY_MOUNT_LOG:-/tmp/tcfs-home-canary-linux-xr-shadow-20260526T000908Z/mount.log}"
SMOKE_MAX_DEPTH="${TCFS_HONEY_SMOKE_MAX_DEPTH:-8}"
SMOKE_TIMEOUT_SECS="${TCFS_HONEY_SMOKE_TIMEOUT_SECS:-900}"
EXPECTED_FILE=.clang-format

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
if [[ -n "${TCFS_HONEY_EXPECTED_VERSION_CONTAINS:-}" && "$tcfs_version" != *"$TCFS_HONEY_EXPECTED_VERSION_CONTAINS"* ]]; then
  printf 'tcfs version mismatch: expected output containing %s\n' "$TCFS_HONEY_EXPECTED_VERSION_CONTAINS" >&2
  exit 1
fi
tcfs_sha256=""
if command -v sha256sum >/dev/null 2>&1; then
  tcfs_sha256="$(sha256sum "$tcfs_resolved" | awk '{print $1}')"
elif command -v shasum >/dev/null 2>&1; then
  tcfs_sha256="$(shasum -a 256 "$tcfs_resolved" | awk '{print $1}')"
fi
if [[ -n "$tcfs_sha256" ]]; then
  echo "tcfs sha256: $tcfs_sha256"
fi
if [[ -n "${TCFS_HONEY_EXPECTED_SHA256:-}" ]]; then
  if [[ -z "$tcfs_sha256" ]]; then
    printf 'tcfs sha256 check requested but no sha256 tool is available\n' >&2
    exit 1
  fi
  if [[ "$tcfs_sha256" != "$TCFS_HONEY_EXPECTED_SHA256" ]]; then
    printf 'tcfs sha256 mismatch: expected %s got %s\n' "$TCFS_HONEY_EXPECTED_SHA256" "$tcfs_sha256" >&2
    exit 1
  fi
fi

mkdir -p "$MOUNT_ROOT"
mount_started=0
cleanup_mount() {
  if [[ "$mount_started" == "1" && "${TCFS_HONEY_KEEP_MOUNT:-0}" != "1" ]]; then
    "$tcfs_resolved" unmount "$MOUNT_ROOT" >/dev/null 2>&1 || fusermount3 -u "$MOUNT_ROOT" >/dev/null 2>&1 || true
  fi
}
trap cleanup_mount EXIT

if [[ "${TCFS_HONEY_START_MOUNT:-0}" == "1" ]]; then
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
if [[ -f "$SYMLINK_TARGETS_FILE" ]]; then
  args+=(--expected-symlink-targets-file "$SYMLINK_TARGETS_FILE")
fi

if [[ "$SMOKE_TIMEOUT_SECS" != "0" && "$SMOKE_TIMEOUT_SECS" =~ ^[0-9]+$ ]] && command -v timeout >/dev/null 2>&1; then
  timeout "$SMOKE_TIMEOUT_SECS" bash "$SMOKE_SCRIPT" "${args[@]}"
else
  bash "$SMOKE_SCRIPT" "${args[@]}"
fi
