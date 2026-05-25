#!/usr/bin/env bash
# Regression tests for scripts/install-smoke.sh using fake installed binaries.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="${REPO_ROOT}/scripts/install-smoke.sh"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-install-smoke-test.XXXXXX")"
trap 'rm -rf "$TMPDIR"' EXIT

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

assert_not_contains() {
  local file="$1"
  local unexpected="$2"

  if grep -Fq -- "$unexpected" "$file"; then
    printf 'did not expect to find %s in %s\n' "$unexpected" "$file" >&2
    printf '%s\n' '--- output ---' >&2
    cat "$file" >&2
    exit 1
  fi
}

write_fake_tcfs() {
  local path="$1"
  cat >"$path" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

log="${FAKE_TCFS_LOG:?set FAKE_TCFS_LOG}"
env_log="${FAKE_TCFS_ENV_LOG:?set FAKE_TCFS_ENV_LOG}"
printf 'tcfs args:%s\n' " $*" >>"$log"

if [[ "${1:-}" == "--version" ]]; then
  echo "tcfs 0.12.13"
  exit 0
fi

base_config=""
if [[ "${1:-}" == "--config" ]]; then
  base_config="$2"
  printf 'tcfs base-config:%s\n' "$base_config" >>"$log"
  shift 2
fi

case "${1:-}" in
  init)
    shift
    if [[ "${1:-}" == "--help" ]]; then
      echo "Usage: tcfs init [OPTIONS]"
      if [[ "${FAKE_TCFS_INIT_CONFIG_OUT:-1}" == "1" ]]; then
        echo "      --config-out <CONFIG_OUT>"
        echo "      --check"
      fi
      exit 0
    fi
    config_out=""
    check=0
    while [[ $# -gt 0 ]]; do
      case "$1" in
        --config-out)
          config_out="$2"
          shift 2
          ;;
        --check)
          check=1
          shift
          ;;
        --device-name|--password)
          shift 2
          ;;
        --non-interactive)
          shift
          ;;
        *)
          shift
          ;;
      esac
    done
    : "${config_out:?missing --config-out}"
    config_dir="$(dirname "$config_out")"
    if [[ "$check" -eq 1 ]]; then
      [[ -f "$config_out" ]] || { echo "missing config" >&2; exit 1; }
      [[ -f "$config_dir/master.key" ]] || { echo "missing master key" >&2; exit 1; }
      [[ -f "$config_dir/devices.json" ]] || { echo "missing device registry" >&2; exit 1; }
      echo "tcfs init check [ok]"
      echo "  Config:     $config_out"
      exit 0
    fi
    mkdir -p "$config_dir"
    if [[ -n "$base_config" && -f "$base_config" ]]; then
      printf 'tcfs base-config-content-begin\n' >>"$log"
      cat "$base_config" >>"$log"
      printf '\ntcfs base-config-content-end\n' >>"$log"
    fi
    printf '0123456789abcdef0123456789abcdef' >"$config_dir/master.key"
    printf '{"devices":[{"name":"install-smoke"}]}\n' >"$config_dir/devices.json"
    cat >"$config_out" <<CONFIG
[daemon]
socket = "${XDG_STATE_HOME}/tcfsd/tcfsd.sock"

[crypto]
enabled = true
master_key_file = "${config_dir}/master.key"

[sync]
device_identity = "${config_dir}/devices.json"
device_name = "install-smoke"
CONFIG
    echo "tcfs initialized successfully."
    ;;
  status)
    if [[ -z "${FAKE_ALLOW_STORAGE_ENV:-}" && -n "${AWS_ACCESS_KEY_ID:-}${AWS_SECRET_ACCESS_KEY:-}${TCFS_S3_ACCESS:-}${TCFS_S3_SECRET:-}${SEAWEED_ACCESS_KEY:-}${SEAWEED_SECRET_KEY:-}" ]]; then
      printf 'ambient credentials leaked into tcfs status\n' >>"$env_log"
    fi
    echo "tcfsd v0.12.13"
    echo "storage: [ok]"
    ;;
  *)
    echo "unexpected fake tcfs command: $*" >&2
    exit 2
    ;;
esac
EOF
  chmod +x "$path"
}

write_fake_tcfsd() {
  local path="$1"
  cat >"$path" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "--version" ]]; then
  echo "tcfsd 0.12.13"
  exit 0
fi

env_log="${FAKE_TCFSD_ENV_LOG:?set FAKE_TCFSD_ENV_LOG}"
if [[ -z "${FAKE_ALLOW_STORAGE_ENV:-}" && -n "${AWS_ACCESS_KEY_ID:-}${AWS_SECRET_ACCESS_KEY:-}${TCFS_S3_ACCESS:-}${TCFS_S3_SECRET:-}${SEAWEED_ACCESS_KEY:-}${SEAWEED_SECRET_KEY:-}" ]]; then
  printf 'ambient credentials leaked into tcfsd\n' >>"$env_log"
fi

config=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --config)
      config="$2"
      shift 2
      ;;
    --log-format)
      shift 2
      ;;
    *)
      shift
      ;;
  esac
done

if [[ -z "$config" || ! -f "$config" ]]; then
  echo "tcfsd config not found: ${config:-<unset>}. Run 'tcfs init --config-out ${config:-<path>}' or pass --config <path>." >&2
  exit 1
fi

socket_path="$(sed -n 's/^socket = "\(.*\)"$/\1/p' "$config" | head -1)"
: "${socket_path:?config missing daemon socket}"
mkdir -p "$(dirname "$socket_path")"

python3 - "$socket_path" <<'PY'
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

server = socket.socket(socket.AF_UNIX)
server.bind(path)
server.listen(1)

def stop(_signum, _frame):
    server.close()
    try:
        os.unlink(path)
    except FileNotFoundError:
        pass
    sys.exit(0)

signal.signal(signal.SIGTERM, stop)
while True:
    time.sleep(1)
PY
EOF
  chmod +x "$path"
}

FAKE_TCFS="$TMPDIR/tcfs"
FAKE_TCFSD="$TMPDIR/tcfsd"
FAKE_TCFS_LOG="$TMPDIR/tcfs.log"
FAKE_TCFS_ENV_LOG="$TMPDIR/tcfs-env.log"
FAKE_TCFSD_ENV_LOG="$TMPDIR/tcfsd-env.log"
export FAKE_TCFS_LOG
export FAKE_TCFS_ENV_LOG
export FAKE_TCFSD_ENV_LOG
export AWS_ACCESS_KEY_ID="ambient-test-key"
export AWS_SECRET_ACCESS_KEY="ambient-test-secret"
export TCFS_S3_ACCESS="ambient-tcfs-key"
export TCFS_S3_SECRET="ambient-tcfs-secret"
export SEAWEED_ACCESS_KEY="ambient-seaweed-key"
export SEAWEED_SECRET_KEY="ambient-seaweed-secret"
export FAKE_TCFS_INIT_CONFIG_OUT=1
: >"$FAKE_TCFSD_ENV_LOG"
: >"$FAKE_TCFS_ENV_LOG"
write_fake_tcfs "$FAKE_TCFS"
write_fake_tcfsd "$FAKE_TCFSD"

CLI_OUT="$TMPDIR/cli.out"
bash "$SCRIPT" \
  --tcfs "$FAKE_TCFS" \
  --tcfsd "$FAKE_TCFSD" \
  --expected-version 0.12.13 \
  >"$CLI_OUT" 2>&1

assert_contains "$CLI_OUT" "initializing first-run config:"
assert_contains "$CLI_OUT" "tcfs init check [ok]"
assert_contains "$CLI_OUT" "daemon socket ready:"
assert_contains "$CLI_OUT" "tcfsd v0.12.13"
assert_contains "$CLI_OUT" "install smoke passed"
assert_contains "$FAKE_TCFS_LOG" "tcfs args: --config"
assert_contains "$FAKE_TCFS_LOG" " init --config-out "
assert_contains "$FAKE_TCFS_LOG" " init --check --config-out "
assert_not_contains "$FAKE_TCFS_ENV_LOG" "ambient credentials leaked"
assert_not_contains "$FAKE_TCFSD_ENV_LOG" "ambient credentials leaked"
assert_not_contains "$CLI_OUT" "missing-config.toml"

STORAGE_OUT="$TMPDIR/storage.out"
: >"$FAKE_TCFSD_ENV_LOG"
: >"$FAKE_TCFS_ENV_LOG"
: >"$FAKE_TCFS_LOG"
FAKE_ALLOW_STORAGE_ENV=1 \
TCFS_SMOKE_S3_ENDPOINT="https://storage.example.invalid" \
TCFS_SMOKE_S3_BUCKET="tcfs-smoke" \
TCFS_SMOKE_S3_REGION="us-west-2" \
TCFS_SMOKE_REMOTE_PREFIX="gha/install-smoke/test" \
TCFS_SMOKE_S3_CA_CERT_PEM="-----BEGIN CERTIFICATE-----
fake-ca
-----END CERTIFICATE-----" \
bash "$SCRIPT" \
  --tcfs "$FAKE_TCFS" \
  --tcfsd "$FAKE_TCFSD" \
  --expected-version 0.12.13 \
  --require-storage-ok \
  >"$STORAGE_OUT" 2>&1

assert_contains "$STORAGE_OUT" "writing storage-backed init base config:"
assert_contains "$STORAGE_OUT" "initializing first-run config:"
assert_contains "$STORAGE_OUT" "storage: [ok]"
assert_contains "$FAKE_TCFS_LOG" "tcfs base-config-content-begin"
assert_contains "$FAKE_TCFS_LOG" 'endpoint = "https://storage.example.invalid"'
assert_contains "$FAKE_TCFS_LOG" 'bucket = "tcfs-smoke"'
assert_contains "$FAKE_TCFS_LOG" 'remote_prefix = "gha/install-smoke/test"'
assert_contains "$FAKE_TCFS_LOG" "enforce_tls = true"
assert_contains "$FAKE_TCFS_LOG" "ca_cert_path = "
assert_not_contains "$FAKE_TCFS_ENV_LOG" "ambient credentials leaked"
assert_not_contains "$FAKE_TCFSD_ENV_LOG" "ambient credentials leaked"

LEGACY_OUT="$TMPDIR/legacy.out"
FAKE_TCFS_INIT_CONFIG_OUT=0 bash "$SCRIPT" \
  --tcfs "$FAKE_TCFS" \
  --tcfsd "$FAKE_TCFSD" \
  --expected-version 0.12.13 \
  >"$LEGACY_OUT" 2>&1

assert_contains "$LEGACY_OUT" "installed tcfs init does not support --config-out; writing explicit smoke config:"
assert_contains "$LEGACY_OUT" "daemon socket ready:"
assert_contains "$LEGACY_OUT" "tcfsd v0.12.13"
assert_contains "$LEGACY_OUT" "install smoke passed"
assert_not_contains "$LEGACY_OUT" "tcfs init failed"
assert_not_contains "$LEGACY_OUT" "missing-config.toml"

DAEMON_ONLY_OUT="$TMPDIR/daemon-only.out"
bash "$SCRIPT" \
  --tcfsd "$FAKE_TCFSD" \
  --expected-version 0.12.13 \
  --skip-cli \
  >"$DAEMON_ONLY_OUT" 2>&1

assert_contains "$DAEMON_ONLY_OUT" "writing daemon-only first-run config:"
assert_contains "$DAEMON_ONLY_OUT" "daemon socket ready:"
assert_contains "$DAEMON_ONLY_OUT" "CLI smoke skipped"
assert_not_contains "$FAKE_TCFSD_ENV_LOG" "ambient credentials leaked"
assert_not_contains "$DAEMON_ONLY_OUT" "missing-config.toml"

printf 'install-smoke tests passed\n'
