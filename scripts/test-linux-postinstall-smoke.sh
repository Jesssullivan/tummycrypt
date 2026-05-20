#!/usr/bin/env bash
#
# Regression tests for linux-postinstall-smoke.sh using fake tools.
#
# These tests do NOT mount a real FUSE filesystem. They stub `tcfs`,
# `tcfsd`, `mountpoint`, and `fusermount3` so the harness's argument
# parsing, the `tcfs index inspect` gate, and the seed/hydrate/mutation
# control flow can be exercised on any Linux runner (and even on macOS by
# bypassing the `uname` guard).
#
# TIN-1422 scaffold: only the most load-bearing behaviors are asserted
# today — the harness itself contains explicit TODOs for the rest.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="${REPO_ROOT}/scripts/linux-postinstall-smoke.sh"
TMPDIR_BASE="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-linux-postinstall-test.XXXXXX")"
trap 'rm -rf "$TMPDIR_BASE"' EXIT

assert_contains() {
  local file="$1"
  local expected="$2"

  if ! grep -Fq -- "$expected" "$file"; then
    printf 'expected to find %s in %s\n' "$expected" "$file" >&2
    printf '%s\n' '--- output ---' >&2
    cat "$file" >&2
    exit 1
  fi
}

assert_fails_contains() {
  local expected="$1"
  shift

  local out="${TMPDIR_BASE}/failure.out"
  local err="${TMPDIR_BASE}/failure.err"

  if "$@" >"$out" 2>"$err"; then
    printf 'expected command to fail: %s\n' "$*" >&2
    exit 1
  fi

  cat "$out" "$err" >"${TMPDIR_BASE}/failure.combined"
  assert_contains "${TMPDIR_BASE}/failure.combined" "$expected"
}

FAKE_BIN="${TMPDIR_BASE}/fake-bin"
HOME_DIR="${TMPDIR_BASE}/home"
LOG_DIR="${TMPDIR_BASE}/logs"
MOUNT_POINT="${TMPDIR_BASE}/mount"
CONFIG_PATH="${HOME_DIR}/.config/tcfs/config.toml"
EXPECTED_REL="canary/fixture.txt"
MUTATION_REL="canary/mutation.txt"
EXPECTED_CONTENT_FILE="${TMPDIR_BASE}/expected.txt"
MUTATION_CONTENT_FILE="${TMPDIR_BASE}/mutation.txt"
REMOTE_ROOT="${TMPDIR_BASE}/remote"
DAEMON_SOCKET="${TMPDIR_BASE}/tcfsd.sock"

mkdir -p \
  "$FAKE_BIN" \
  "$(dirname "$CONFIG_PATH")" \
  "$MOUNT_POINT/$(dirname "$EXPECTED_REL")" \
  "$REMOTE_ROOT/$(dirname "$EXPECTED_REL")" \
  "$LOG_DIR"

cat >"$CONFIG_PATH" <<EOF
[daemon]
socket = "${DAEMON_SOCKET}"

[storage]
endpoint = "https://example.invalid:8333"
bucket = "tcfs"

[sync]
state_db = "${TMPDIR_BASE}/tcfs-state.db"
sync_root = "${TMPDIR_BASE}/sync-root"
EOF

printf 'TCFS Linux hydration fixture\n' >"$EXPECTED_CONTENT_FILE"
printf 'TCFS Linux mutation fixture\n' >"$MUTATION_CONTENT_FILE"
cp "$EXPECTED_CONTENT_FILE" "$MOUNT_POINT/$EXPECTED_REL"
cp "$EXPECTED_CONTENT_FILE" "$REMOTE_ROOT/$EXPECTED_REL"

# ── Fake uname so the script runs on the test host too ──────────────────────
cat >"$FAKE_BIN/uname" <<'EOF'
#!/usr/bin/env bash
if [[ "${1:-}" == "-s" ]]; then
  printf 'Linux\n'
else
  printf 'Linux\n'
fi
EOF

# ── Fake tcfs CLI that supports status, index inspect, push, pull, mount,
#    and cache evict ─────────────────────────────────────────────────────────
cat >"$FAKE_BIN/tcfs" <<'EOF'
#!/usr/bin/env bash
case "${1:-}" in
  --version)
    printf 'tcfs 0.13.0\n'
    ;;
  --config)
    config="$2"
    shift 2
    case "${1:-status}" in
      status)
        printf 'tcfsd v0.13.0\nstorage: [ok]\n'
        ;;
      index)
        [[ "${2:-}" == "inspect" ]] || {
          printf 'expected fake index inspect\n' >&2
          exit 1
        }
        rel="${3:-}"
        status="${TCFS_FAKE_INDEX_STATUS:-visible}"
        cat <<JSON
{
  "rel_path": "$rel",
  "remote_prefix": "data",
  "index_key": "data/index/$rel",
  "index_exists": true,
  "status": "$status",
  "parse_error": null,
  "entry_state": "committed",
  "visible_entry": {
    "manifest_hash": "fakehash",
    "manifest_key": "data/manifests/fakehash",
    "manifest_exists": true,
    "size": 27,
    "chunks": 1,
    "kind": "regular_file",
    "symlink_target": null
  },
  "pending_entry": null
}
JSON
        ;;
      push)
        root="$2"
        [[ -n "${TCFS_FAKE_REMOTE_ROOT:-}" ]] || {
          printf 'TCFS_FAKE_REMOTE_ROOT missing for fake push\n' >&2
          exit 1
        }
        mkdir -p "$TCFS_FAKE_REMOTE_ROOT"
        cp -R "$root"/. "$TCFS_FAKE_REMOTE_ROOT"/
        printf 'Push complete:\n  uploaded: 1 files\n'
        ;;
      pull)
        rel="$2"
        dest="$3"
        src="${TCFS_FAKE_REMOTE_ROOT:?}/$rel"
        if [[ ! -f "$src" && -n "${TCFS_FAKE_MOUNT_ROOT:-}" && -f "$TCFS_FAKE_MOUNT_ROOT/$rel" ]]; then
          mkdir -p "$(dirname "$src")"
          cp "$TCFS_FAKE_MOUNT_ROOT/$rel" "$src"
        fi
        if [[ ! -f "$src" ]]; then
          printf 'fake pull: missing %s\n' "$src" >&2
          exit 1
        fi
        /bin/cat "$src" >"$dest"
        printf 'Pulling %s -> %s using %s\n' "$rel" "$dest" "$config"
        ;;
      mount)
        # tcfs mount is normally long-running; for the test we sleep until killed.
        spec="$2"
        point="$3"
        printf 'fake mount: %s -> %s\n' "$spec" "$point"
        # Touch a marker so the harness can use mountpoint(1) to detect.
        : >"${TCFS_FAKE_MOUNT_MARKER:-/dev/null}"
        exec sleep 60
        ;;
      cache)
        [[ "${2:-}" == "evict" ]] || {
          printf 'expected fake cache evict\n' >&2
          exit 1
        }
        rel="${3:-}"
        printf 'Evicted cache entry: %s\n' "$rel"
        printf '  remote prefix: fake/prefix\n'
        printf '  manifest:      fakehash\n'
        printf '  freed:         27 B\n'
        printf '  result:        evicted\n'
        ;;
      unsync)
        printf 'fake tcfs unsync should not be used for mounted cache eviction\n' >&2
        exit 64
        ;;
      *)
        printf 'unexpected tcfs --config invocation:'
        printf ' %q' "$@"
        printf '\n'
        exit 1
        ;;
    esac
    ;;
  *)
    printf 'unexpected tcfs invocation:'
    printf ' %q' "$@"
    printf '\n'
    exit 1
    ;;
esac
EOF

cat >"$FAKE_BIN/tcfsd" <<'EOF'
#!/usr/bin/env bash
if [[ "${1:-}" == "--version" ]]; then
  printf 'tcfsd 0.13.0\n'
else
  config=""
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --config)
        config="$2"
        shift 2
        ;;
      *)
        shift
        ;;
    esac
  done

  socket_path=""
  if [[ -n "$config" ]]; then
    socket_path="$(python3 - "$config" <<'PY' 2>/dev/null || true
import sys
try:
    import tomllib
except ModuleNotFoundError:
    import tomli as tomllib
with open(sys.argv[1], "rb") as fh:
    cfg = tomllib.load(fh)
print(cfg.get("daemon", {}).get("socket", ""))
PY
)"
  fi

  if [[ -z "$socket_path" ]]; then
    exec sleep 60
  fi

  mkdir -p "$(dirname "$socket_path")"
  exec python3 - "$socket_path" <<'PY'
import os
import signal
import socket
import sys
import time

path = sys.argv[1]
try:
    os.unlink(path)
except FileNotFoundError:
    pass

server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
server.bind(path)
server.listen(1)

def stop(_signum, _frame):
    server.close()
    try:
        os.unlink(path)
    except FileNotFoundError:
        pass
    raise SystemExit(0)

signal.signal(signal.SIGTERM, stop)
while True:
    time.sleep(60)
PY
fi
EOF

cat >"$FAKE_BIN/systemctl" <<'EOF'
#!/usr/bin/env bash
# Always report "no systemd" so the harness falls back to foreground.
exit 1
EOF

cat >"$FAKE_BIN/sudo" <<'EOF'
#!/usr/bin/env bash
if [[ "${1:-}" == "-n" ]]; then
  shift
fi
exec "$@"
EOF

cat >"$FAKE_BIN/apt-get" <<'EOF'
#!/usr/bin/env bash
if [[ "${1:-}" == "install" && "${*: -1}" == *.deb ]]; then
  printf 'fake apt direct install failure\n' >&2
  exit 1
fi

if [[ "${1:-}" == "install" && "${*: -1}" == "-f" ]]; then
  if [[ "${TCFS_FAKE_APT_FIX_FAIL:-0}" == "1" ]]; then
    printf 'fake apt dependency repair failure\n' >&2
    exit 23
  fi
  printf 'fake apt dependency repair success\n'
  exit 0
fi

printf 'unexpected fake apt-get invocation:'
printf ' %q' "$@"
printf '\n' >&2
exit 1
EOF

cat >"$FAKE_BIN/dpkg" <<'EOF'
#!/usr/bin/env bash
printf 'fake dpkg install success\n'
exit 0
EOF

cat >"$FAKE_BIN/mountpoint" <<'EOF'
#!/usr/bin/env bash
# Treat the marker file as proof of mount.
if [[ "${1:-}" == "-q" && -n "${2:-}" ]]; then
  if [[ -f "${TCFS_FAKE_MOUNT_MARKER:-}" ]]; then
    exit 0
  fi
  exit 1
fi
exit 1
EOF

cat >"$FAKE_BIN/fusermount3" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF

chmod +x "$FAKE_BIN"/*

POSITIVE_OUT="${TMPDIR_BASE}/positive.out"
MOUNT_MARKER="${TMPDIR_BASE}/mount-marker"
FAKE_CLI_DEB="${TMPDIR_BASE}/tcfs-0.13.0-amd64.deb"
FAKE_DAEMON_DEB="${TMPDIR_BASE}/tcfsd-0.13.0-amd64.deb"
: >"$FAKE_CLI_DEB"
: >"$FAKE_DAEMON_DEB"

env PATH="$FAKE_BIN:$PATH" \
  HOME="$HOME_DIR" \
  TCFS_FAKE_REMOTE_ROOT="$REMOTE_ROOT" \
  TCFS_FAKE_MOUNT_ROOT="$MOUNT_POINT" \
  TCFS_FAKE_MOUNT_MARKER="$MOUNT_MARKER" \
  bash "$SCRIPT" \
    --package-path "$FAKE_CLI_DEB" \
    --package-path "$FAKE_DAEMON_DEB" \
    --expected-version 0.13.0 \
    --config "$CONFIG_PATH" \
    --expected-file "$EXPECTED_REL" \
    --expected-content-file "$EXPECTED_CONTENT_FILE" \
    --remote-prefix "fake/prefix" \
    --mount-point "$MOUNT_POINT" \
    --exercise-evict-rehydrate \
    --exercise-mutation \
    --mutation-file "$MUTATION_REL" \
    --mutation-content-file "$MUTATION_CONTENT_FILE" \
    --no-systemd \
    --timeout 10 \
    --log-dir "$LOG_DIR" \
    >"$POSITIVE_OUT" 2>&1 || {
  echo "positive run failed; output:" >&2
  cat "$POSITIVE_OUT" >&2 || true
  exit 1
}

assert_contains "$POSITIVE_OUT" "installing .deb: $FAKE_CLI_DEB"
assert_contains "$POSITIVE_OUT" "installing .deb: $FAKE_DAEMON_DEB"
assert_contains "$POSITIVE_OUT" "package-install-1.log"
assert_contains "$POSITIVE_OUT" "package-install-2.log"
assert_contains "$POSITIVE_OUT" "tcfsd version: tcfsd 0.13.0"
assert_contains "$POSITIVE_OUT" "tcfs version: tcfs 0.13.0"
assert_contains "$POSITIVE_OUT" "daemon socket ready: $DAEMON_SOCKET"
assert_contains "$POSITIVE_OUT" "remote index status for expected file: visible"
assert_contains "$POSITIVE_OUT" "derived --remote-spec: seaweedfs+https://example.invalid:8333/tcfs/fake/prefix"
assert_contains "$POSITIVE_OUT" "FUSE mount ready"
assert_contains "$POSITIVE_OUT" "hydrated file content matched expected content file"
assert_contains "$POSITIVE_OUT" "evicting hydrated cache entry: $EXPECTED_REL"
assert_contains "$POSITIVE_OUT" "Evicted cache entry: $EXPECTED_REL"
assert_contains "$POSITIVE_OUT" "Linux evict/rehydrate cycle passed"
assert_contains "$POSITIVE_OUT" "Linux mutation local content matched"
assert_contains "$POSITIVE_OUT" "remote mutation pull matched expected content"
assert_contains "$POSITIVE_OUT" "tcfs status (post-mutation):"
assert_contains "$POSITIVE_OUT" "Linux post-install smoke passed"
assert_contains "$LOG_DIR/expected-file-index.json" '"status": "visible"'

# ── Negative: index inspect status != visible blocks hydration ──────────────
NEG_OUT="${TMPDIR_BASE}/negative.out"
NEG_MOUNT_MARKER="${TMPDIR_BASE}/neg-mount-marker"
NEG_MOUNT_POINT="${TMPDIR_BASE}/neg-mount"
mkdir -p "$NEG_MOUNT_POINT/$(dirname "$EXPECTED_REL")"
cp "$EXPECTED_CONTENT_FILE" "$NEG_MOUNT_POINT/$EXPECTED_REL"

env PATH="$FAKE_BIN:$PATH" \
  HOME="$HOME_DIR" \
  TCFS_FAKE_REMOTE_ROOT="$REMOTE_ROOT" \
  TCFS_FAKE_MOUNT_MARKER="$NEG_MOUNT_MARKER" \
  TCFS_FAKE_INDEX_STATUS="missing" \
  bash "$SCRIPT" \
    --expected-version 0.13.0 \
    --config "$CONFIG_PATH" \
    --expected-file "$EXPECTED_REL" \
    --expected-content-file "$EXPECTED_CONTENT_FILE" \
    --remote-prefix "fake/prefix" \
    --remote-spec "seaweedfs://example.invalid:8333/tcfs/fake/prefix" \
    --mount-point "$NEG_MOUNT_POINT" \
    --skip-package-install \
    --no-systemd \
    --timeout 10 \
    --log-dir "${TMPDIR_BASE}/neg-logs" \
    >"$NEG_OUT" 2>&1 && {
  echo "expected negative run to fail when index status != visible" >&2
  cat "$NEG_OUT" >&2 || true
  exit 1
}
assert_contains "$NEG_OUT" "expected file is not backed by a visible remote index entry"

# Helpers for negative arg-parsing checks. These need the fake `uname` on PATH
# so the harness's Linux guard does not preempt arg validation.
run_with_fakes() {
  env PATH="$FAKE_BIN:$PATH" bash "$SCRIPT" "$@"
}

# ── Negative: --exercise-mutation requires --remote-prefix ──────────────────
assert_fails_contains "--exercise-mutation requires --remote-prefix" \
  run_with_fakes --exercise-mutation

# ── Negative: --expected-content and --expected-content-file are mutually exclusive
assert_fails_contains "mutually exclusive" \
  run_with_fakes --expected-content foo --expected-content-file /etc/hostname

# ── Negative: --exercise-evict-rehydrate without expected file ──────────────
assert_fails_contains "--exercise-evict-rehydrate requires" \
  run_with_fakes --exercise-evict-rehydrate

# ── Negative: .deb fallback must not mask apt dependency repair failure ─────
FAKE_DEB="${TMPDIR_BASE}/fake-tcfsd.deb"
: >"$FAKE_DEB"
assert_fails_contains "apt dependency repair failed after dpkg install" \
  env PATH="$FAKE_BIN:$PATH" \
    HOME="$HOME_DIR" \
    TCFS_FAKE_APT_FIX_FAIL=1 \
    bash "$SCRIPT" \
      --package-path "$FAKE_DEB" \
      --config "$CONFIG_PATH" \
      --no-systemd \
      --timeout 1 \
      --log-dir "${TMPDIR_BASE}/deb-failure-logs"

echo "linux-postinstall-smoke tests passed"
