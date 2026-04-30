#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/install-smoke.sh [options]

Starts an installed tcfsd in an isolated temp environment, waits for its socket,
and runs `tcfs status` when the CLI is available.

Options:
  --expected-version <version>  Require tcfs/tcfsd --version output to include this string
  --tcfs <path-or-name>         CLI binary to use (default: tcfs)
  --tcfsd <path-or-name>        Daemon binary to use (default: tcfsd)
  --skip-cli                    Skip tcfs status smoke; useful for daemon-only surfaces
  --require-storage-ok          Require tcfs status to report storage [ok]
  -h, --help                    Show this help
EOF
}

EXPECTED_VERSION=""
TCFS_BIN="${TCFS_BIN:-tcfs}"
TCFSD_BIN="${TCFSD_BIN:-tcfsd}"
SKIP_CLI=0
REQUIRE_STORAGE_OK=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --expected-version)
      EXPECTED_VERSION="$2"
      shift 2
      ;;
    --tcfs)
      TCFS_BIN="$2"
      shift 2
      ;;
    --tcfsd)
      TCFSD_BIN="$2"
      shift 2
      ;;
    --skip-cli)
      SKIP_CLI=1
      shift
      ;;
    --require-storage-ok)
      REQUIRE_STORAGE_OK=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

resolve_bin() {
  local candidate="$1"
  if [[ "$candidate" == */* ]]; then
    [[ -x "$candidate" ]] || {
      echo "binary is not executable: $candidate" >&2
      exit 1
    }
    printf '%s\n' "$candidate"
    return
  fi

  command -v "$candidate" >/dev/null 2>&1 || {
    echo "command not found: $candidate" >&2
    exit 1
  }
  command -v "$candidate"
}

assert_version() {
  local label="$1"
  local bin="$2"
  local output

  output="$("$bin" --version)"
  echo "$label version: $output"

  if [[ -n "$EXPECTED_VERSION" && "$output" != *"$EXPECTED_VERSION"* ]]; then
    echo "$label version mismatch: expected output containing '$EXPECTED_VERSION'" >&2
    exit 1
  fi
}

# Keep the default short on macOS. GitHub-hosted macOS exposes a long
# /var/folders/... TMPDIR, and daemon Unix sockets can exceed SUN_LEN there.
TMP_BASE="${TCFS_INSTALL_SMOKE_TMPDIR:-/tmp}"
TMP_DIR="$(mktemp -d "${TMP_BASE%/}/tcfs-install-smoke.XXXXXX")"
CONFIG_PATH="$TMP_DIR/missing-config.toml"
HOME_DIR="$TMP_DIR/home"
STATE_DIR="$HOME_DIR/.local/state"
CONFIG_DIR="$HOME_DIR/.config"
SOCKET_PATH="$STATE_DIR/tcfsd/tcfsd.sock"
DAEMON_LOG="$TMP_DIR/tcfsd.log"
STATUS_LOG="$TMP_DIR/tcfs-status.log"
daemon_pid=""

cleanup() {
  if [[ -n "$daemon_pid" ]] && kill -0 "$daemon_pid" 2>/dev/null; then
    kill "$daemon_pid" 2>/dev/null || true
    wait "$daemon_pid" 2>/dev/null || true
  fi
  rm -rf "$TMP_DIR"
}

trap cleanup EXIT

mkdir -p "$STATE_DIR" "$CONFIG_DIR"
export HOME="$HOME_DIR"
export XDG_STATE_HOME="$STATE_DIR"
export XDG_CONFIG_HOME="$CONFIG_DIR"

TCFSD_PATH="$(resolve_bin "$TCFSD_BIN")"
assert_version "tcfsd" "$TCFSD_PATH"

if [[ "$SKIP_CLI" -eq 0 ]]; then
  TCFS_PATH="$(resolve_bin "$TCFS_BIN")"
  assert_version "tcfs" "$TCFS_PATH"
else
  TCFS_PATH=""
fi

echo "starting daemon smoke with temp home: $HOME_DIR"
"$TCFSD_PATH" --config "$CONFIG_PATH" --log-format text >"$DAEMON_LOG" 2>&1 &
daemon_pid="$!"

for _ in $(seq 1 30); do
  if [[ -S "$SOCKET_PATH" ]]; then
    break
  fi
  if ! kill -0 "$daemon_pid" 2>/dev/null; then
    echo "tcfsd exited before creating socket" >&2
    cat "$DAEMON_LOG" >&2 || true
    exit 1
  fi
  sleep 1
done

if [[ ! -S "$SOCKET_PATH" ]]; then
  echo "tcfsd did not create socket at $SOCKET_PATH" >&2
  cat "$DAEMON_LOG" >&2 || true
  exit 1
fi

echo "daemon socket ready: $SOCKET_PATH"

if [[ "$SKIP_CLI" -eq 1 ]]; then
  echo "CLI smoke skipped"
  exit 0
fi

"$TCFS_PATH" --config "$CONFIG_PATH" status >"$STATUS_LOG" 2>&1 || {
  echo "tcfs status failed" >&2
  cat "$STATUS_LOG" >&2 || true
  echo "--- daemon log ---" >&2
  cat "$DAEMON_LOG" >&2 || true
  exit 1
}

grep -q "tcfsd v" "$STATUS_LOG" || {
  echo "tcfs status output did not include daemon version" >&2
  cat "$STATUS_LOG" >&2 || true
  exit 1
}

if [[ "$REQUIRE_STORAGE_OK" -eq 1 ]]; then
  grep -Eq 'storage:.*\[ok\]' "$STATUS_LOG" || {
    echo "tcfs status did not report storage [ok]" >&2
    cat "$STATUS_LOG" >&2 || true
    exit 1
  }
fi

cat "$STATUS_LOG"
echo "install smoke passed"
