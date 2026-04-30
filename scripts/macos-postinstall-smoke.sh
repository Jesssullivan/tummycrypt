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
  --expected-content <text>   Exact content expected from --expected-file
  --expected-content-file <path>
                              File containing exact expected content
  --app-path <path>           Installed TCFSProvider.app path
                              (default: auto-detect /Applications or ~/Applications)
  --cloud-root <path>         CloudStorage root path
                              (default: auto-detect ~/Library/CloudStorage/TCFS*)
  --plugin-id <id>            FileProvider extension bundle id
                              (default: io.tinyland.tcfs.fileprovider)
  --domain-id <id>            FileProvider domain id
                              (default: io.tinyland.tcfs)
  --allow-multiple-plugin-registrations
                              Warn instead of failing if pluginkit shows more
                              than one registration for --plugin-id
  --require-keychain-config   Require extension logs to prove config loaded
                              from shared Keychain, and fail if embedded config
                              was used
  --tcfs <path-or-name>       CLI binary to use (default: tcfs)
  --tcfsd <path-or-name>      Daemon binary to use (default: tcfsd)
  --timeout <seconds>         Wait timeout for async steps (default: 45)
  --log-dir <path>            Persist status logs instead of using a temp dir
  --skip-status               Skip `tcfs status` checks
  -h, --help                  Show this help
EOF
}

EXPECTED_VERSION=""
CONFIG_PATH="${TCFS_CONFIG:-$HOME/.config/tcfs/config.toml}"
EXPECTED_FILE_REL=""
EXPECTED_CONTENT=""
EXPECTED_CONTENT_FILE=""
APP_PATH="${TCFS_APP_PATH:-}"
CLOUD_ROOT="${TCFS_CLOUD_ROOT:-}"
PLUGIN_ID="${TCFS_PLUGIN_ID:-io.tinyland.tcfs.fileprovider}"
DOMAIN_ID="${TCFS_DOMAIN_ID:-io.tinyland.tcfs}"
ALLOW_MULTIPLE_PLUGIN_REGISTRATIONS="${TCFS_ALLOW_MULTIPLE_PLUGIN_REGISTRATIONS:-0}"
REQUIRE_KEYCHAIN_CONFIG="${TCFS_REQUIRE_KEYCHAIN_CONFIG:-0}"
TCFS_BIN="${TCFS_BIN:-tcfs}"
TCFSD_BIN="${TCFSD_BIN:-tcfsd}"
TIMEOUT_SECS="${TIMEOUT_SECS:-45}"
LOG_SHOW_TIMEOUT_SECS="${LOG_SHOW_TIMEOUT_SECS:-5}"
LOG_DIR_OVERRIDE=""
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
    --expected-content)
      EXPECTED_CONTENT="$2"
      shift 2
      ;;
    --expected-content-file)
      EXPECTED_CONTENT_FILE="$2"
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
    --allow-multiple-plugin-registrations)
      ALLOW_MULTIPLE_PLUGIN_REGISTRATIONS=1
      shift
      ;;
    --require-keychain-config)
      REQUIRE_KEYCHAIN_CONFIG=1
      shift
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
    --log-dir)
      LOG_DIR_OVERRIDE="$2"
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

if [[ -n "$EXPECTED_CONTENT" && -n "$EXPECTED_CONTENT_FILE" ]]; then
  echo "--expected-content and --expected-content-file are mutually exclusive" >&2
  exit 2
fi

if [[ "$REQUIRE_KEYCHAIN_CONFIG" == "1" && -z "$EXPECTED_FILE_REL" ]]; then
  echo "--require-keychain-config requires --expected-file so the extension config path is exercised" >&2
  exit 2
fi

short_pause() {
  if command -v perl >/dev/null 2>&1; then
    perl -e 'select undef, undef, undef, 1.0'
  else
    python3 -c 'import select; select.select([], [], [], 1.0)'
  fi
}

run_log_show() {
  local out
  local err
  local pid
  local waited=0
  local status=0

  out="$(mktemp "$LOG_DIR/log-show.XXXXXX")"
  err="${out}.err"

  log show "$@" >"$out" 2>"$err" &
  pid="$!"

  while kill -0 "$pid" 2>/dev/null; do
    if (( waited >= LOG_SHOW_TIMEOUT_SECS )); then
      kill "$pid" 2>/dev/null || true
      short_pause
      kill -KILL "$pid" 2>/dev/null || true
      wait "$pid" 2>/dev/null || true
      cat "$out" 2>/dev/null || true
      rm -f "$out" "$err"
      return 124
    fi

    short_pause
    waited=$((waited + 1))
  done

  wait "$pid" || status="$?"
  cat "$out" 2>/dev/null || true
  rm -f "$out" "$err"
  return "$status"
}

run_bounded_to_log() {
  local label="$1"
  local timeout_secs="$2"
  shift 2

  local out
  local pid
  local waited=0
  local status=0

  out="$LOG_DIR/${label}.log"

  "$@" >"$out" 2>&1 &
  pid="$!"

  while kill -0 "$pid" 2>/dev/null; do
    if (( waited >= timeout_secs )); then
      kill "$pid" 2>/dev/null || true
      short_pause
      kill -KILL "$pid" 2>/dev/null || true
      wait "$pid" 2>/dev/null || true
      return 124
    fi

    short_pause
    waited=$((waited + 1))
  done

  wait "$pid" || status="$?"
  return "$status"
}

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
  echo "$label binary: $bin"
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

if [[ -n "$EXPECTED_CONTENT_FILE" ]]; then
  require_file "$EXPECTED_CONTENT_FILE"
fi

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
  local record_count
  local path_count
  local plugin_paths
  output="$(pluginkit -m -A -D -vvv -i "$PLUGIN_ID" 2>&1)" || {
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

  record_count="$(grep -c "$PLUGIN_ID" <<<"$output")"
  plugin_paths="$(
    awk '
      /^[[:space:]]*Path = / {
        path = $0
        sub(/^[[:space:]]*Path = /, "", path)
        print path
      }
    ' <<<"$output" | sort -u
  )"
  path_count="$(grep -c . <<<"$plugin_paths" || true)"

  if (( record_count > 1 && path_count == 1 )); then
    echo "warning: pluginkit shows $record_count records for one FileProvider path" >&2
  fi

  if (( path_count > 1 )); then
    if [[ "$ALLOW_MULTIPLE_PLUGIN_REGISTRATIONS" == "1" ]]; then
      echo "warning: pluginkit shows $path_count FileProvider extension paths for $PLUGIN_ID" >&2
    else
      echo "multiple FileProvider extension paths found for $PLUGIN_ID; remove stale app/extension copies or pass --allow-multiple-plugin-registrations for diagnostic runs" >&2
      print_pluginkit_duplicate_hint "$output"
      exit 1
    fi
  fi
}

print_pluginkit_duplicate_hint() {
  local output="$1"

  echo "registered FileProvider extension paths:" >&2
  awk '
    /^[[:space:]]*Path = / {
      path = $0
      sub(/^[[:space:]]*Path = /, "", path)
      print "  extension: " path
    }
    /^[[:space:]]*Parent Bundle = / {
      parent = $0
      sub(/^[[:space:]]*Parent Bundle = /, "", parent)
      print "  parent app: " parent
    }
  ' <<<"$output" >&2
  echo "cleanup is not performed automatically; remove stale app/extension copies or run pluginkit -r intentionally, then rerun preflight" >&2
}

check_host_log() {
  local output
  output="$(run_log_show --style compact --last 45s \
    --predicate 'subsystem == "io.tinyland.tcfs" && category == "host"' || true)"

  [[ -n "$output" ]] || return 1
  grep -q "add: OK" <<<"$output"
}

extension_log_path() {
  printf '%s/extension-config.log\n' "$LOG_DIR"
}

collect_extension_config_log() {
  run_log_show --style compact --last 2m \
    --predicate 'subsystem == "io.tinyland.tcfs.fileprovider" && category == "extension" && eventMessage CONTAINS "loadConfig:"' \
    || true
}

check_keychain_config_log() {
  local output

  output="$(collect_extension_config_log)"
  printf '%s\n' "$output" >"$(extension_log_path)"

  if grep -q "loadConfig: loaded from build-time embedded config" <<<"$output"; then
    echo "FileProvider extension used build-time embedded config; production Keychain proof failed" >&2
    cat "$(extension_log_path)" >&2
    exit 1
  fi

  if grep -q "loadConfig: loaded from shared Keychain" <<<"$output"; then
    echo "FileProvider extension config source: shared Keychain"
    return
  fi

  echo "FileProvider extension did not log shared Keychain config load" >&2
  cat "$(extension_log_path)" >&2
  exit 1
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
    short_pause
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
    short_pause
    attempt=$((attempt + 1))
  done

  echo "timed out waiting for expected file: $path" >&2
  exit 1
}

nudge_cloud_root_enumeration() {
  local root="$1"

  echo "nudging CloudStorage enumeration"

  ls -la "$root" >"$LOG_DIR/cloud-root-ls.log" 2>&1 || true

  # A headed workstation usually triggers FileProvider enumeration through
  # Finder naturally. GitHub-hosted macOS runners can create the CloudStorage
  # root without launching the extension until something explicitly opens or
  # materializes it, so use bounded best-effort nudges before the hard wait.
  open "$root" >/dev/null 2>&1 || true

  if command -v fileproviderctl >/dev/null 2>&1; then
    run_bounded_to_log \
      "fileproviderctl-materialize-root" \
      "$LOG_SHOW_TIMEOUT_SECS" \
      fileproviderctl materialize "$root" || true
  fi
}

enumerate_root() {
  local root="$1"
  local listing
  local attempt=0

  while (( attempt < TIMEOUT_SECS )); do
    if (( attempt % 5 == 0 )); then
      ls -la "$root" >>"$LOG_DIR/cloud-root-ls.log" 2>&1 || true
    fi
    listing="$(find "$root" -mindepth 1 -maxdepth 4 | head -n 10 || true)"
    if [[ -n "$listing" ]]; then
      echo "enumeration sample:"
      echo "$listing"
      return
    fi
    short_pause
    attempt=$((attempt + 1))
  done

  echo "enumeration found no entries under $root" >&2
  exit 1
}

hydrate_expected_file() {
  local path="$1"
  local hydrated_copy="$LOG_DIR/hydrated-expected-file"
  local expected_copy="$LOG_DIR/expected-content"
  local cat_error="$LOG_DIR/hydrate-cat-error.log"
  local hydrated_bytes
  local attempt=0

  [[ -f "$path" ]] || {
    echo "expected path is not a regular file: $path" >&2
    exit 1
  }

  while (( attempt < TIMEOUT_SECS )); do
    if cat "$path" >"$hydrated_copy" 2>"$cat_error"; then
      break
    fi
    rm -f "$hydrated_copy"
    short_pause
    attempt=$((attempt + 1))
  done

  if (( attempt >= TIMEOUT_SECS )); then
    cat "$cat_error" >&2 || true
    echo "failed to read expected file for hydration: $path" >&2
    exit 1
  fi

  echo "hydrated file: $path"
  hydrated_bytes="$(wc -c <"$hydrated_copy" | tr -d '[:space:]')"
  echo "  size: $hydrated_bytes bytes"

  if [[ -n "$EXPECTED_CONTENT_FILE" ]]; then
    if ! cmp -s "$EXPECTED_CONTENT_FILE" "$hydrated_copy"; then
      echo "hydrated file content mismatch against $EXPECTED_CONTENT_FILE" >&2
      exit 1
    fi
    echo "hydrated file content matched expected content file"
  elif [[ -n "$EXPECTED_CONTENT" ]]; then
    printf '%s' "$EXPECTED_CONTENT" >"$expected_copy"
    if ! cmp -s "$expected_copy" "$hydrated_copy"; then
      echo "hydrated file content mismatch against --expected-content" >&2
      exit 1
    fi
    echo "hydrated file content matched expected content"
  fi
}

APP_PATH="$(detect_app_path)"
if [[ -n "$LOG_DIR_OVERRIDE" ]]; then
  LOG_DIR="$LOG_DIR_OVERRIDE"
  mkdir -p "$LOG_DIR"
else
  LOG_DIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-macos-postinstall.XXXXXX")"
  trap 'rm -rf "$LOG_DIR"' EXIT
fi

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
echo "status logs: $LOG_DIR"
check_pluginkit

echo "launching host app: $APP_PATH"
open "$APP_PATH"

HOST_LOG_WAIT=0
until check_host_log; do
  short_pause
  HOST_LOG_WAIT=$((HOST_LOG_WAIT + 1))
  if (( HOST_LOG_WAIT >= TIMEOUT_SECS )); then
    echo "timed out waiting for host app log showing domain re-add" >&2
    run_log_show --style compact --last 2m \
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

nudge_cloud_root_enumeration "$CLOUD_ROOT"
enumerate_root "$CLOUD_ROOT"

if [[ -n "$EXPECTED_FILE_REL" ]]; then
  EXPECTED_PATH="$CLOUD_ROOT/$EXPECTED_FILE_REL"
  wait_for_expected_file "$EXPECTED_PATH"
  hydrate_expected_file "$EXPECTED_PATH"
  if [[ "$REQUIRE_KEYCHAIN_CONFIG" == "1" ]]; then
    check_keychain_config_log
  fi
else
  echo "warning: --expected-file not provided; hydration was not exercised" >&2
fi

run_status "post-hydrate"

echo "macOS post-install FileProvider smoke passed"
