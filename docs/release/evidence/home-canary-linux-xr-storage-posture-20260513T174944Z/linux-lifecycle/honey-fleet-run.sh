#!/usr/bin/env bash
set -euo pipefail

MOUNT_ROOT_RAW='~/tcfs-pilot/fleet-parity-20260513T180654Z-67250'
case "$MOUNT_ROOT_RAW" in
  "~/"*) MOUNT_ROOT="${HOME}/${MOUNT_ROOT_RAW#\~/}" ;;
  *) MOUNT_ROOT="$MOUNT_ROOT_RAW" ;;
esac
SMOKE_SCRIPT="${TCFS_HONEY_SMOKE_SCRIPT:-/tmp/tcfs-home-canary-linux-xr-shadow-20260513T174944Z/linux-lifecycle/lazy-hydration-mounted-smoke.sh}"
EXPECTED_CONTENT_FILE="${TCFS_HONEY_FLEET_EXPECTED_CONTENT_FILE:-/tmp/tcfs-home-canary-linux-xr-shadow-20260513T174944Z/linux-lifecycle/fleet-documents-expected.txt}"

bash "$SMOKE_SCRIPT" \
  --mount-root "$MOUNT_ROOT" \
  --expected-file Documents/fleet-readiness.md \
  --expected-content-file "$EXPECTED_CONTENT_FILE" \
  --expect-entry Documents \
  --expect-entry git \
  --expect-entry git/tcfs-pilot-repo \
  --max-depth 8
