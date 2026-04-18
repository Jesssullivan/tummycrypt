#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/macos-postinstall-smoke.sh [options]

Verify the installed macOS FileProvider path after package/app install:
artifact presence, pluginkit registration, host-app launch, domain re-add,
CloudStorage appearance, enumeration, and optional hydration of a known file.

This helper assumes a real operator config and a running tcfsd. It does not
start the daemon or fabricate backend fixtures.

Options:
  --config <path>             tcfs config for `tcfs status`
                              (default: ~/.config/tcfs/config.toml)
  --expected-version <ver>    Require tcfs/tcfsd --version output to include this string
  --expected-file <relpath>   Relative path under the CloudStorage root to
                              enumerate and hydrate
  --app-path <path>           Installed TCFSProvider.app path
                              (default: auto-detect /Applications or ~/Applications)
  --cloud-root <path>         CloudStorage root path
                              (default: auto-detect ~/Library/CloudStorage/TCFS*)
  --plugin-id <id>            FileProvider extension bundle id
                              (default: io.tinyland.tcfs.fileprovider)
  --domain-id <id>            FileProvider domain id
                              (default: io.tinyland.tcfs)
  --tcfs <path-or-name>       CLI binary to use (default: tcfs)
  --tcfsd <path-or-name>      Daemon binary to use (default: tcfsd)
  --timeout <seconds>         Wait timeout for async steps (default: 45)
  --skip-status               Skip `tcfs status` checks
  -h, --help                  Show this help
EOF
}

EXPECTED_VERSION=""
CONFIG_PATH="${TCFS_CONFIG:-$HOME/.config/tcfs/config.toml}"
EXPECTED_FILE_REL=""
APP_PATH="${TCFS_APP_PATH:-}"
CLOUD_ROOT="${TCFS_CLOUD_ROOT:-}"
PLUGIN_ID="${TCFS_PLUGIN_ID:-io.tinyland.tcfs.fileprovider}"
DOMAIN_ID="${TCFS_DOMAIN_ID:-io.tinyland.tcfs}"
TCFS_BIN="${TCFS_BIN:-tcfs}"
TCFSD_BIN="${TCFSD_BIN:-tcfsd}"
TIMEOUT_SECS="${TIMEOUT_SECS:-45}"
SKIP_STATUS=0
LOG_DIR=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --config)
      CONFIG_PATH="$2"
      shift 2
      ;;
    --expected-version)
      EXPECTED_VERSION="$2"
      shift 2
      ;;
    --expected-file)
      EXPECTED_FILE_REL="$2"
      shift 2
      ;;
    --app-path)
      APP_PATH="$2"
      shift 2
      ;;
    --cloud-root)
      CLOUD_ROOT="$2"
      shift 2
      ;;
    --plugin-id)
      PLUGIN_ID="$2"
      shift 2
      ;;
    --domain-id)
      DOMAIN_ID="$2"
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
    --timeout)
      TIMEOUT_SECS="$2"
      shift 2
      ;;
    --skip-status)
      SKIP_STATUS=1
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

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "scripts/macos-postinstall-smoke.sh only runs on macOS" >&2
  exit 1
fi

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

require_file() {
  local path="$1"
  [[ -f "$path" ]] || {
    echo "required file not found: $path" >&2
    exit 1
  }
}

require_dir() {
  local path="$1"
  [[ -d "$path" ]] || {
    echo "required directory not found: $path" >&2
    exit 1
  }
}

detect_app_path() {
  if [[ -n "$APP_PATH" ]]; then
    require_dir "$APP_PATH"
    printf '%s\n' "$APP_PATH"
    return
  fi

  local candidate
  for candidate in \
    "/Applications/TCFSProvider.app" \
    "$HOME/Applications/TCFSProvider.app"
  do
    if [[ -d "$candidate" ]]; then
      printf '%s\n' "$candidate"
      return
    fi
  done

  echo "TCFSProvider.app not found in /Applications or ~/Applications" >&2
  exit 1
}

status_log() {
  local label="$1"
  printf '%s/%s-status.log\n' "$LOG_DIR" "$label"
}

run_status() {
  local label="$1"
  local log_path

  if [[ "$SKIP_STATUS" -eq 1 ]]; then
    echo "status check skipped ($label)"
    return
  fi

  require_file "$CONFIG_PATH"

  log_path="$(status_log "$label")"
  "$TCFS_PATH" --config "$CONFIG_PATH" status >"$log_path" 2>&1 || {
    echo "tcfs status failed during $label" >&2
    cat "$log_path" >&2 || true
    exit 1
  }

  grep -q "tcfsd v" "$log_path" || {
    echo "tcfs status output did not include daemon version during $label" >&2
    cat "$log_path" >&2 || true
    exit 1
  }

  grep -Eq 'storage:.*\[ok\]' "$log_path" || {
    echo "tcfs status did not report storage [ok] during $label" >&2
    cat "$log_path" >&2 || true
    exit 1
  }

  echo "tcfs status ($label):"
  cat "$log_path"
}

check_pluginkit() {
  local output
  output="$(pluginkit -m -A -D -i "$PLUGIN_ID" 2>&1)" || {
    echo "pluginkit lookup failed for $PLUGIN_ID" >&2
    echo "$output" >&2
    exit 1
  }

  echo "pluginkit registration:"
  echo "$output"

  grep -q "$PLUGIN_ID" <<<"$output" || {
    echo "pluginkit output did not include $PLUGIN_ID" >&2
    exit 1
  }
}

check_host_log() {
  local output
  output="$(log show --style compact --last 45s \
    --predicate 'subsystem == "io.tinyland.tcfs" && category == "host"' 2>/dev/null || true)"

  [[ -n "$output" ]] || return 1
  grep -q "add: OK" <<<"$output"
}

check_domain_listing() {
  command -v fileproviderctl >/dev/null 2>&1 || return 2

  local output
  output="$(fileproviderctl domain list 2>&1)" || return 2
  grep -q "$DOMAIN_ID" <<<"$output"
}

discover_cloud_root() {
  if [[ -n "$CLOUD_ROOT" ]]; then
    require_dir "$CLOUD_ROOT"
    printf '%s\n' "$CLOUD_ROOT"
    return
  fi

  local roots=()
  local candidate
  while IFS= read -r candidate; do
    roots+=("$candidate")
  done < <(find "$HOME/Library/CloudStorage" -mindepth 1 -maxdepth 1 -type d -name 'TCFS*' 2>/dev/null | sort)

  case "${#roots[@]}" in
    0)
      return 1
      ;;
    1)
      printf '%s\n' "${roots[0]}"
      ;;
    *)
      echo "multiple TCFS CloudStorage roots found; pass --cloud-root explicitly" >&2
      printf '  %s\n' "${roots[@]}" >&2
      exit 1
      ;;
  esac
}

wait_for_cloud_root() {
  local attempt=0
  local root=""
  while (( attempt < TIMEOUT_SECS )); do
    if root="$(discover_cloud_root)"; then
      printf '%s\n' "$root"
      return
    fi
    sleep 1
    attempt=$((attempt + 1))
  done

  echo "timed out waiting for CloudStorage root under $HOME/Library/CloudStorage" >&2
  exit 1
}

wait_for_expected_file() {
  local path="$1"
  local attempt=0
  while (( attempt < TIMEOUT_SECS )); do
    if [[ -e "$path" ]]; then
      return
    fi
    sleep 1
    attempt=$((attempt + 1))
  done

  echo "timed out waiting for expected file: $path" >&2
  exit 1
}

enumerate_root() {
  local root="$1"
  local listing
  local attempt=0

  while (( attempt < TIMEOUT_SECS )); do
    listing="$(find "$root" -mindepth 1 -maxdepth 4 | head -n 10 || true)"
    if [[ -n "$listing" ]]; then
      echo "enumeration sample:"
      echo "$listing"
      return
    fi
    sleep 1
    attempt=$((attempt + 1))
  done

  echo "enumeration found no entries under $root" >&2
  exit 1
}

hydrate_expected_file() {
  local path="$1"
  [[ -f "$path" ]] || {
    echo "expected path is not a regular file: $path" >&2
    exit 1
  }

  dd if="$path" of=/dev/null bs=4096 count=1 status=none || {
    echo "failed to read expected file for hydration: $path" >&2
    exit 1
  }

  echo "hydrated file: $path"
  stat -f '  size: %z bytes' "$path"
}

APP_PATH="$(detect_app_path)"
LOG_DIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-macos-postinstall.XXXXXX")"
trap 'rm -rf "$LOG_DIR"' EXIT

TCFSD_PATH="$(resolve_bin "$TCFSD_BIN")"
assert_version "tcfsd" "$TCFSD_PATH"

if [[ "$SKIP_STATUS" -eq 0 ]]; then
  TCFS_PATH="$(resolve_bin "$TCFS_BIN")"
  assert_version "tcfs" "$TCFS_PATH"
  run_status "preflight"
else
  TCFS_PATH=""
fi

require_file "$HOME/.config/tcfs/fileprovider/config.json"
check_pluginkit

echo "launching host app: $APP_PATH"
open "$APP_PATH"

HOST_LOG_WAIT=0
until check_host_log; do
  sleep 1
  HOST_LOG_WAIT=$((HOST_LOG_WAIT + 1))
  if (( HOST_LOG_WAIT >= TIMEOUT_SECS )); then
    echo "timed out waiting for host app log showing domain re-add" >&2
    log show --style compact --last 2m \
      --predicate 'subsystem == "io.tinyland.tcfs" && category == "host"' || true
    exit 1
  fi
done

echo "host app log confirmed domain re-add"
if check_domain_listing; then
  echo "fileproviderctl domain listing includes $DOMAIN_ID"
else
  echo "warning: could not confirm domain via fileproviderctl; relying on host log + CloudStorage root" >&2
fi

CLOUD_ROOT="$(wait_for_cloud_root)"
echo "CloudStorage root: $CLOUD_ROOT"

enumerate_root "$CLOUD_ROOT"

if [[ -n "$EXPECTED_FILE_REL" ]]; then
  EXPECTED_PATH="$CLOUD_ROOT/$EXPECTED_FILE_REL"
  wait_for_expected_file "$EXPECTED_PATH"
  hydrate_expected_file "$EXPECTED_PATH"
else
  echo "warning: --expected-file not provided; hydration was not exercised" >&2
fi

run_status "post-hydrate"

echo "macOS post-install FileProvider smoke passed"
