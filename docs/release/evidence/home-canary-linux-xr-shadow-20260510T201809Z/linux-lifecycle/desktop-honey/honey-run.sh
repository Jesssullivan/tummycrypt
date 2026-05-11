#!/usr/bin/env bash
set -euo pipefail

REMOTE=seaweedfs://100.64.48.53:8333/tcfs/home-canary-linux-xr-shadow-20260510T201807Z/linux-lifecycle
TCFS_BIN=tcfs
MOUNT_ROOT_RAW='~/tcfs-pilot/fleet-parity-20260511T005139Z-3314'
case "$MOUNT_ROOT_RAW" in
  "~/"*) MOUNT_ROOT="${HOME}/${MOUNT_ROOT_RAW#\~/}" ;;
  *) MOUNT_ROOT="$MOUNT_ROOT_RAW" ;;
esac
EXPECTED_FILE=Projects/tcfs-odrive-parity/honey-readme.txt
EXPECTED_CONTENT_FILE="${TCFS_HONEY_EXPECTED_CONTENT_FILE:-/tmp/tcfs-desktop-honey-expected.txt}"
SMOKE_SCRIPT="${TCFS_HONEY_SMOKE_SCRIPT:-/tmp/lazy-hydration-mounted-smoke.sh}"
MOUNT_LOG="${TCFS_HONEY_MOUNT_LOG:-/tmp/tcfs-desktop-honey-mount.log}"

if [[ -n "${TCFS_HONEY_ENV_FILE:-}" ]]; then
  # shellcheck disable=SC1090
  source "$TCFS_HONEY_ENV_FILE"
fi

mkdir -p "$MOUNT_ROOT" "$(dirname "$EXPECTED_CONTENT_FILE")"
cat >"$EXPECTED_CONTENT_FILE" <<'EXPECTED_CONTENT_EOF'
TCFS Desktop honey fixture
This file starts in an isolated Desktop demo folder and should hydrate lazily on honey.
EXPECTED_CONTENT_EOF

if [[ "${TCFS_HONEY_START_MOUNT:-0}" == "1" ]]; then
  nohup "$TCFS_BIN" mount "$REMOTE" "$MOUNT_ROOT" >"$MOUNT_LOG" 2>&1 &
  mount_pid="$!"
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

if [[ ! -x "$SMOKE_SCRIPT" && ! -f "$SMOKE_SCRIPT" ]]; then
  echo "missing mounted smoke helper: $SMOKE_SCRIPT" >&2
  echo "copy scripts/lazy-hydration-mounted-smoke.sh there or set TCFS_HONEY_SMOKE_SCRIPT" >&2
  exit 1
fi

bash "$SMOKE_SCRIPT" \
  --mount-root "$MOUNT_ROOT" \
  --expected-file "$EXPECTED_FILE" \
  --expected-content-file "$EXPECTED_CONTENT_FILE" \
  --expect-entry Projects \
  --expect-entry Projects/tcfs-odrive-parity \
  --max-depth 6
