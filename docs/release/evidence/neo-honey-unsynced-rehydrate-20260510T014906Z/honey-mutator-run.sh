#!/usr/bin/env bash
set -euo pipefail

REMOTE=seaweedfs://100.64.48.53:8333/tcfs/neo-honey-unsynced-rehydrate-20260510T014906Z
TCFS_BIN=/tmp/tcfs-fleet-pilot-build-20260509T1907Z/target/debug/tcfs
MOUNT_ROOT_RAW='/tmp/tcfs-neo-honey-unsynced-rehydrate-20260510T014906Z-honey/mount'
case "$MOUNT_ROOT_RAW" in
  "~/"*) MOUNT_ROOT="${HOME}/${MOUNT_ROOT_RAW#\~/}" ;;
  *) MOUNT_ROOT="$MOUNT_ROOT_RAW" ;;
esac
FIXTURE_FILE=Projects/shared/notes.md
EXPECTED_INITIAL="${TCFS_HONEY_INITIAL_CONTENT_FILE:-/tmp/tcfs-neo-honey-unsynced-rehydrate-20260510T014906Z-honey/run/neo-initial-content.txt}"
EXPECTED_MUTATED="${TCFS_HONEY_MUTATED_CONTENT_FILE:-/tmp/tcfs-neo-honey-unsynced-rehydrate-20260510T014906Z-honey/run/honey-mutated-content.txt}"
SMOKE_SCRIPT="${TCFS_HONEY_SMOKE_SCRIPT:-/tmp/tcfs-neo-honey-unsynced-rehydrate-20260510T014906Z-honey/run/lazy-hydration-mounted-smoke.sh}"
MOUNT_LOG="${TCFS_HONEY_MOUNT_LOG:-/tmp/tcfs-neo-honey-unsynced-rehydrate-20260510T014906Z-honey/run/mount.log}"

if [[ -n "${TCFS_HONEY_ENV_FILE:-}" ]]; then
  # shellcheck disable=SC1090
  source "$TCFS_HONEY_ENV_FILE"
fi

mkdir -p "$MOUNT_ROOT"
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

bash "$SMOKE_SCRIPT" \
  --mount-root "$MOUNT_ROOT" \
  --expected-file "$FIXTURE_FILE" \
  --expected-content-file "$EXPECTED_INITIAL" \
  --expect-entry Projects \
  --expect-entry Projects/shared \
  --max-depth 8

fixture_path="$MOUNT_ROOT/$FIXTURE_FILE"
if [[ ! -f "$fixture_path" ]]; then
  echo "expected mounted fixture missing before honey mutation: $fixture_path" >&2
  exit 1
fi
cp "$EXPECTED_MUTATED" "$fixture_path"
cmp -s "$EXPECTED_MUTATED" "$fixture_path"
echo "honey mounted mutation wrote exact content: $FIXTURE_FILE"
