#!/usr/bin/env bash
set -euo pipefail

MOUNT_ROOT_RAW='/tmp/tcfs-home-canary-linux-xr-shadow-20260510T002604Z/mount'
case "$MOUNT_ROOT_RAW" in
  "~/"*) MOUNT_ROOT="${HOME}/${MOUNT_ROOT_RAW#\~/}" ;;
  *) MOUNT_ROOT="$MOUNT_ROOT_RAW" ;;
esac
SMOKE_SCRIPT="${TCFS_HONEY_SMOKE_SCRIPT:-/tmp/tcfs-home-canary-linux-xr-shadow-20260510T002604Z/lazy-hydration-mounted-smoke.sh}"
EXPECTED_FILE=.clang-format

args=(
  --mount-root "$MOUNT_ROOT"
  --expect-entry .git
  --max-depth 8
)
if [[ -n "$EXPECTED_FILE" ]]; then
  args+=(--expected-file "$EXPECTED_FILE")
fi

bash "$SMOKE_SCRIPT" "${args[@]}"
