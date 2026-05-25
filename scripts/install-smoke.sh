#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/install-smoke.sh [options]

Initializes an installed tcfs CLI surface in an isolated temp environment,
starts tcfsd against that generated config, waits for its socket, and runs
`tcfs status` when the CLI is available. Daemon-only surfaces get a minimal
intentional config instead of relying on tcfsd defaults.

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

write_daemon_only_config() {
  cat >"$CONFIG_PATH" <<EOF
[daemon]
socket = "$SOCKET_PATH"

[storage]
endpoint = "http://localhost:8333"
bucket = "tcfs"
enforce_tls = false

[sync]
state_db = "$STATE_DIR/tcfsd/state.db"

[crypto]
enabled = false
EOF
  chmod 600 "$CONFIG_PATH" 2>/dev/null || true
}

tcfs_supports_init_config_out() {
  "$TCFS_PATH" init --help 2>&1 | grep -Fq -- "--config-out"
}

start_daemon() {
  if [[ "$REQUIRE_STORAGE_OK" -eq 1 ]]; then
    "$TCFSD_PATH" --config "$CONFIG_PATH" --log-format text
    return
  fi

  env \
    -u TCFS_S3_ACCESS \
    -u TCFS_S3_SECRET \
    -u TCFS_S3_ACCESS_FILE \
    -u TCFS_S3_SECRET_FILE \
    -u TCFS_S3_ACCESS_KEY_FILE \
    -u TCFS_S3_SECRET_KEY_FILE \
    -u AWS_ACCESS_KEY_ID \
    -u AWS_SECRET_ACCESS_KEY \
    -u AWS_SESSION_TOKEN \
    -u AWS_ACCESS_KEY_ID_FILE \
    -u AWS_SECRET_ACCESS_KEY_FILE \
    -u SEAWEED_ACCESS_KEY \
    -u SEAWEED_SECRET_KEY \
    -u REMOTE_JUGGLER_IDENTITY \
    -u TCFS_KDBX_PATH \
    -u AWS_PROFILE \
    -u AWS_SHARED_CREDENTIALS_FILE \
    -u AWS_CONFIG_FILE \
    "$TCFSD_PATH" --config "$CONFIG_PATH" --log-format text
}

run_tcfs_status() {
  if [[ "$REQUIRE_STORAGE_OK" -eq 1 ]]; then
    "$TCFS_PATH" --config "$CONFIG_PATH" status
    return
  fi

  env \
    -u TCFS_S3_ACCESS \
    -u TCFS_S3_SECRET \
    -u TCFS_S3_ACCESS_FILE \
    -u TCFS_S3_SECRET_FILE \
    -u TCFS_S3_ACCESS_KEY_FILE \
    -u TCFS_S3_SECRET_KEY_FILE \
    -u AWS_ACCESS_KEY_ID \
    -u AWS_SECRET_ACCESS_KEY \
    -u AWS_SESSION_TOKEN \
    -u AWS_ACCESS_KEY_ID_FILE \
    -u AWS_SECRET_ACCESS_KEY_FILE \
    -u SEAWEED_ACCESS_KEY \
    -u SEAWEED_SECRET_KEY \
    -u REMOTE_JUGGLER_IDENTITY \
    -u TCFS_KDBX_PATH \
    -u AWS_PROFILE \
    -u AWS_SHARED_CREDENTIALS_FILE \
    -u AWS_CONFIG_FILE \
    "$TCFS_PATH" --config "$CONFIG_PATH" status
}

# Keep the default short on macOS. GitHub-hosted macOS exposes a long
# /var/folders/... TMPDIR, and daemon Unix sockets can exceed SUN_LEN there.
TMP_BASE="${TCFS_INSTALL_SMOKE_TMPDIR:-/tmp}"
TMP_DIR="$(mktemp -d "${TMP_BASE%/}/tcfs-install-smoke.XXXXXX")"
CONFIG_PATH="$TMP_DIR/config.toml"
INIT_BASE_CONFIG="$TMP_DIR/init-base-missing.toml"
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
  if tcfs_supports_init_config_out; then
    echo "initializing first-run config: $CONFIG_PATH"
    TCFS_MASTER_PASSWORD="tcfs-install-smoke-passphrase" \
      "$TCFS_PATH" --config "$INIT_BASE_CONFIG" init \
        --config-out "$CONFIG_PATH" \
        --device-name "install-smoke" \
        --non-interactive \
        >"$TMP_DIR/tcfs-init.log" 2>&1 || {
          echo "tcfs init failed" >&2
          cat "$TMP_DIR/tcfs-init.log" >&2 || true
          exit 1
        }
    "$TCFS_PATH" --config "$CONFIG_PATH" init --check --config-out "$CONFIG_PATH" \
      >"$TMP_DIR/tcfs-init-check.log" 2>&1 || {
        echo "tcfs init --check failed" >&2
        cat "$TMP_DIR/tcfs-init-check.log" >&2 || true
        exit 1
      }
    cat "$TMP_DIR/tcfs-init-check.log"
  else
    echo "installed tcfs init does not support --config-out; writing explicit smoke config: $CONFIG_PATH"
    write_daemon_only_config
  fi
else
  TCFS_PATH=""
  echo "writing daemon-only first-run config: $CONFIG_PATH"
  write_daemon_only_config
fi

echo "starting daemon smoke with temp home: $HOME_DIR"
start_daemon >"$DAEMON_LOG" 2>&1 &
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

run_tcfs_status >"$STATUS_LOG" 2>&1 || {
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
