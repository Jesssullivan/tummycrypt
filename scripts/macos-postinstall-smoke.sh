#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
COORDINATED_READ="${TCFS_COORDINATED_READ:-1}"

usage() {
  cat <<'EOF'
Usage: scripts/macos-postinstall-smoke.sh [options]

Verify the installed macOS FileProvider path after package/app install:
artifact presence, pluginkit registration, host-app launch, domain add,
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
  --exercise-evict-rehydrate  After initial exact hydration, request provider
                              eviction and verify a second exact rehydrate
  --exercise-mutation         Write a new file through the CloudStorage root
                              and verify the upload by pulling it from remote
  --mutation-file <relpath>   Relative mutation file path under CloudStorage
                              (default: generated root-level file name)
  --mutation-content <text>   Exact content to write for --exercise-mutation
  --mutation-content-file <path>
                              File containing exact mutation content
  --exercise-conflict-status  Verify a pre-seeded conflict/status fixture
                              through CLI state, FileProvider enumeration,
                              requestDownload, and exact-content hydration
  --conflict-file <relpath>   Relative conflict fixture path under CloudStorage
  --conflict-content <text>   Exact content expected for --conflict-file
  --conflict-content-file <path>
                              File containing exact conflict fixture content
  --state <path>              State cache JSON path for conflict/status checks
  --sync-root <path>          Sync root containing the conflict fixture
  --remote-prefix <prefix>    Remote prefix used to verify mutation by pull
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
  --elect-plugin-use          Set PlugInKit user election to "use" for
                              --plugin-id before launching the host app. This
                              is intended for hosted/diagnostic runners where
                              System Settings cannot be clicked.
  --fileprovider-testing-mode Request NSFileProvider testing mode
                              alwaysEnabled when launching the host app. The
                              installed host app must carry Apple's
                              com.apple.developer.fileprovider.testing-mode
                              entitlement.
  --direct-host-launch         Launch the host app executable directly for
                              domain-add logging instead of relying on
                              LaunchServices + unified log polling.
  --rebuild-domain            Ask the host app to remove and re-add the
                              FileProvider domain before smoke. Requires a
                              direct host-app launch and is intended only for
                              archived stale-domain diagnostics.
  --seed-expected-file         Create a timestamped fixture, push it to remote
                              storage through tcfs, and use it as the expected
                              FileProvider hydration target. If
                              --expected-file is omitted, a
                              finder-smoke-<UTC>/fixture.txt path is generated.
  --host-root-probe            Always launch the signed HostApp root/user-visible
                              URL probe and require an entry sample, even when
                              shell/coordinated CloudStorage enumeration works.
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
EXERCISE_EVICT_REHYDRATE="${TCFS_EXERCISE_EVICT_REHYDRATE:-0}"
EXERCISE_MUTATION="${TCFS_EXERCISE_MUTATION:-0}"
MUTATION_FILE_REL="${TCFS_FILEPROVIDER_MUTATION_FILE_REL:-}"
MUTATION_CONTENT="${TCFS_FILEPROVIDER_MUTATION_CONTENT:-}"
MUTATION_CONTENT_FILE="${TCFS_FILEPROVIDER_MUTATION_CONTENT_FILE:-}"
EXERCISE_CONFLICT_STATUS="${TCFS_EXERCISE_CONFLICT_STATUS:-0}"
CONFLICT_FILE_REL="${TCFS_FILEPROVIDER_CONFLICT_FILE_REL:-}"
CONFLICT_CONTENT="${TCFS_FILEPROVIDER_CONFLICT_CONTENT:-}"
CONFLICT_CONTENT_FILE="${TCFS_FILEPROVIDER_CONFLICT_CONTENT_FILE:-}"
CONFLICT_STATE_PATH="${TCFS_STATE_PATH:-}"
SYNC_ROOT_OVERRIDE="${TCFS_SYNC_ROOT:-}"
REMOTE_PREFIX="${TCFS_REMOTE_PREFIX:-}"
APP_PATH="${TCFS_APP_PATH:-}"
CLOUD_ROOT="${TCFS_CLOUD_ROOT:-}"
PLUGIN_ID="${TCFS_PLUGIN_ID:-io.tinyland.tcfs.fileprovider}"
DOMAIN_ID="${TCFS_DOMAIN_ID:-io.tinyland.tcfs}"
ALLOW_MULTIPLE_PLUGIN_REGISTRATIONS="${TCFS_ALLOW_MULTIPLE_PLUGIN_REGISTRATIONS:-0}"
ELECT_PLUGIN_USE="${TCFS_ELECT_PLUGIN_USE:-0}"
FILEPROVIDER_TESTING_MODE="${TCFS_FILEPROVIDER_TESTING_MODE:-0}"
DIRECT_HOST_LAUNCH="${TCFS_FILEPROVIDER_DIRECT_HOST_LAUNCH:-0}"
REBUILD_DOMAIN="${TCFS_FILEPROVIDER_REBUILD_DOMAIN:-0}"
SEED_EXPECTED_FILE="${TCFS_FILEPROVIDER_SEED_EXPECTED_FILE:-0}"
HOST_ROOT_PROBE="${TCFS_FILEPROVIDER_HOST_ROOT_PROBE:-0}"
REQUIRE_KEYCHAIN_CONFIG="${TCFS_REQUIRE_KEYCHAIN_CONFIG:-0}"
TCFS_BIN="${TCFS_BIN:-tcfs}"
TCFSD_BIN="${TCFSD_BIN:-tcfsd}"
TIMEOUT_SECS="${TIMEOUT_SECS:-45}"
LOG_SHOW_TIMEOUT_SECS="${LOG_SHOW_TIMEOUT_SECS:-5}"
LOG_DIR_OVERRIDE=""
SKIP_STATUS=0
LOG_DIR=""
HOST_APP_ROOT_ENUMERATION=0

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
    --exercise-evict-rehydrate)
      EXERCISE_EVICT_REHYDRATE=1
      shift
      ;;
    --exercise-mutation)
      EXERCISE_MUTATION=1
      shift
      ;;
    --mutation-file)
      MUTATION_FILE_REL="$2"
      shift 2
      ;;
    --mutation-content)
      MUTATION_CONTENT="$2"
      shift 2
      ;;
    --mutation-content-file)
      MUTATION_CONTENT_FILE="$2"
      shift 2
      ;;
    --exercise-conflict-status)
      EXERCISE_CONFLICT_STATUS=1
      shift
      ;;
    --conflict-file)
      CONFLICT_FILE_REL="$2"
      shift 2
      ;;
    --conflict-content)
      CONFLICT_CONTENT="$2"
      shift 2
      ;;
    --conflict-content-file)
      CONFLICT_CONTENT_FILE="$2"
      shift 2
      ;;
    --state)
      CONFLICT_STATE_PATH="$2"
      shift 2
      ;;
    --sync-root)
      SYNC_ROOT_OVERRIDE="$2"
      shift 2
      ;;
    --remote-prefix)
      REMOTE_PREFIX="$2"
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
    --elect-plugin-use)
      ELECT_PLUGIN_USE=1
      shift
      ;;
    --fileprovider-testing-mode)
      FILEPROVIDER_TESTING_MODE=1
      shift
      ;;
    --direct-host-launch)
      DIRECT_HOST_LAUNCH=1
      shift
      ;;
    --rebuild-domain)
      REBUILD_DOMAIN=1
      DIRECT_HOST_LAUNCH=1
      shift
      ;;
    --seed-expected-file)
      SEED_EXPECTED_FILE=1
      shift
      ;;
    --host-root-probe)
      HOST_ROOT_PROBE=1
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

READ_TIMEOUT_SECS="${TCFS_FILEPROVIDER_READ_TIMEOUT_SECS:-$TIMEOUT_SECS}"

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "scripts/macos-postinstall-smoke.sh only runs on macOS" >&2
  exit 1
fi

if [[ -n "$EXPECTED_CONTENT" && -n "$EXPECTED_CONTENT_FILE" ]]; then
  echo "--expected-content and --expected-content-file are mutually exclusive" >&2
  exit 2
fi

if [[ -n "$MUTATION_CONTENT" && -n "$MUTATION_CONTENT_FILE" ]]; then
  echo "--mutation-content and --mutation-content-file are mutually exclusive" >&2
  exit 2
fi

if [[ -n "$CONFLICT_CONTENT" && -n "$CONFLICT_CONTENT_FILE" ]]; then
  echo "--conflict-content and --conflict-content-file are mutually exclusive" >&2
  exit 2
fi

if [[ "$REQUIRE_KEYCHAIN_CONFIG" == "1" && -z "$EXPECTED_FILE_REL" && "$SEED_EXPECTED_FILE" != "1" ]]; then
  echo "--require-keychain-config requires --expected-file or --seed-expected-file so the extension config path is exercised" >&2
  exit 2
fi

if [[ "$EXERCISE_EVICT_REHYDRATE" == "1" && -z "$EXPECTED_FILE_REL" && "$SEED_EXPECTED_FILE" != "1" ]]; then
  echo "--exercise-evict-rehydrate requires --expected-file or --seed-expected-file" >&2
  exit 2
fi

if [[ "$EXERCISE_EVICT_REHYDRATE" == "1" && "$SEED_EXPECTED_FILE" != "1" && -z "$EXPECTED_CONTENT" && -z "$EXPECTED_CONTENT_FILE" ]]; then
  echo "--exercise-evict-rehydrate requires --expected-content, --expected-content-file, or --seed-expected-file" >&2
  exit 2
fi

if [[ "$EXERCISE_MUTATION" == "1" && -z "$REMOTE_PREFIX" ]]; then
  echo "--exercise-mutation requires --remote-prefix" >&2
  exit 2
fi

if [[ "$EXERCISE_CONFLICT_STATUS" == "1" && -z "$CONFLICT_FILE_REL" ]]; then
  echo "--exercise-conflict-status requires --conflict-file" >&2
  exit 2
fi

if [[ "$EXERCISE_CONFLICT_STATUS" == "1" && -z "$CONFLICT_CONTENT" && -z "$CONFLICT_CONTENT_FILE" ]]; then
  echo "--exercise-conflict-status requires --conflict-content or --conflict-content-file" >&2
  exit 2
fi

if [[ "$EXERCISE_CONFLICT_STATUS" == "1" && "$SKIP_STATUS" -eq 1 ]]; then
  echo "--exercise-conflict-status requires tcfs CLI status checks; remove --skip-status" >&2
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
  local status=0

  out="$(mktemp "$LOG_DIR/log-show.XXXXXX")"
  err="${out}.err"

  log show "$@" >"$out" 2>"$err" &
  pid="$!"

  wait_for_pid_with_timeout "$pid" "$LOG_SHOW_TIMEOUT_SECS" "log show" || status="$?"
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
  local status=0

  out="$LOG_DIR/${label}.log"

  "$@" >"$out" 2>&1 &
  pid="$!"

  wait_for_pid_with_timeout "$pid" "$timeout_secs" "$label" || status="$?"
  return "$status"
}

run_bounded_append_to_log() {
  local label="$1"
  local timeout_secs="$2"
  shift 2

  local out
  local tmp
  local pid
  local status=0

  out="$LOG_DIR/${label}.log"
  tmp="$(mktemp "$LOG_DIR/${label}.XXXXXX")"

  "$@" >"$tmp" 2>&1 &
  pid="$!"

  wait_for_pid_with_timeout "$pid" "$timeout_secs" "$label" || status="$?"
  cat "$tmp" >>"$out" 2>/dev/null || true
  rm -f "$tmp"
  return "$status"
}

wait_for_pid_with_timeout() {
  local pid="$1"
  local timeout_secs="$2"
  local label="$3"
  local timeout_marker="${LOG_DIR:-${TMPDIR:-/tmp}}/tcfs-read-timeout.$$.$pid"
  local watchdog_pid
  local waited=0
  local status=0

  (
    while (( waited < timeout_secs )); do
      short_pause
      waited=$((waited + 1))
    done
    : >"$timeout_marker"
    if kill -0 "$pid" 2>/dev/null; then
      pkill -TERM -P "$pid" 2>/dev/null || true
      kill "$pid" 2>/dev/null || true
      short_pause
      pkill -KILL -P "$pid" 2>/dev/null || true
      kill -KILL "$pid" 2>/dev/null || true
    fi
  ) &
  watchdog_pid="$!"

  wait "$pid" || status="$?"
  kill "$watchdog_pid" 2>/dev/null || true
  wait "$watchdog_pid" 2>/dev/null || true

  if [[ -e "$timeout_marker" ]]; then
    rm -f "$timeout_marker"
    echo "$label timed out after ${timeout_secs}s" >&2
    return 124
  fi

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

if [[ -n "$MUTATION_CONTENT_FILE" ]]; then
  require_file "$MUTATION_CONTENT_FILE"
fi

if [[ -n "$CONFLICT_CONTENT_FILE" ]]; then
  require_file "$CONFLICT_CONTENT_FILE"
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

run_tcfs_index_inspect() {
  local rel="$1"
  local out="$2"
  local err="${out}.err"
  local -a args

  [[ -n "$TCFS_PATH" ]] || return 2

  args=(--config "$CONFIG_PATH" index inspect "$rel" --json)
  if [[ -n "$REMOTE_PREFIX" ]]; then
    args+=(--prefix "$REMOTE_PREFIX")
  fi

  "$TCFS_PATH" "${args[@]}" >"$out" 2>"$err"
}

extract_index_status() {
  local report="$1"

  python3 - "$report" <<'PY'
import json
import sys

with open(sys.argv[1], "r", encoding="utf-8") as fh:
    report = json.load(fh)
print(report.get("status", "unknown"))
PY
}

archive_expected_remote_index() {
  local rel="$1"
  local report="$LOG_DIR/expected-file-index.json"

  if [[ -z "$TCFS_PATH" ]]; then
    printf 'tcfs CLI unavailable; index inspection skipped\n' >"$report"
    return
  fi

  run_tcfs_index_inspect "$rel" "$report" || {
    echo "tcfs index inspect failed for expected file: $rel" >"$report"
    cat "${report}.err" >>"$report" 2>/dev/null || true
  }
}

require_expected_remote_index() {
  local rel="$1"
  local report="$LOG_DIR/expected-file-index.json"
  local status

  run_tcfs_index_inspect "$rel" "$report" || {
    echo "tcfs index inspect failed for expected file: $rel" >&2
    cat "${report}.err" >&2 2>/dev/null || true
    exit 1
  }

  status="$(extract_index_status "$report")"
  echo "remote index status for expected file: $status"
  if [[ "$status" != "visible" ]]; then
    echo "expected file is not backed by a visible remote index entry: $rel" >&2
    cat "$report" >&2 || true
    exit 1
  fi
}

prepare_seed_content_file() {
  local target="$1"

  if [[ -n "$EXPECTED_CONTENT_FILE" ]]; then
    cp "$EXPECTED_CONTENT_FILE" "$target"
  elif [[ -n "$EXPECTED_CONTENT" ]]; then
    printf '%s' "$EXPECTED_CONTENT" >"$target"
  else
    printf 'tcfs FileProvider seeded fixture %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" >"$target"
  fi
}

seed_expected_file_if_requested() {
  [[ "$SEED_EXPECTED_FILE" == "1" ]] || return 0

  local seed_stamp
  local seed_root="$LOG_DIR/seed-expected-root"
  local seed_content="$LOG_DIR/seed-expected-content"
  local push_log="$LOG_DIR/seed-expected-file-push.log"
  local push_state="$LOG_DIR/seed-expected-state.json"
  local -a args

  [[ -n "$TCFS_PATH" ]] || {
    echo "--seed-expected-file requires tcfs CLI status checks; remove --skip-status" >&2
    exit 2
  }

  if [[ -z "$EXPECTED_FILE_REL" ]]; then
    seed_stamp="$(date -u '+%Y%m%dT%H%M%SZ')"
    EXPECTED_FILE_REL="finder-smoke-${seed_stamp}/fixture.txt"
  fi
  validate_relative_file_path "$EXPECTED_FILE_REL"

  mkdir -p "$seed_root/$(dirname "$EXPECTED_FILE_REL")"
  prepare_seed_content_file "$seed_content"
  cp "$seed_content" "$seed_root/$EXPECTED_FILE_REL"
  EXPECTED_CONTENT_FILE="$seed_content"

  echo "seeding expected FileProvider fixture: $EXPECTED_FILE_REL"
  args=(--config "$CONFIG_PATH" push "$seed_root" --state "$push_state")
  if [[ -n "$REMOTE_PREFIX" ]]; then
    args+=(--prefix "$REMOTE_PREFIX")
  fi

  "$TCFS_PATH" "${args[@]}" >"$push_log" 2>&1 || {
    echo "failed to seed expected FileProvider fixture" >&2
    cat "$push_log" >&2 || true
    exit 1
  }
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
    if [[ "$ALLOW_MULTIPLE_PLUGIN_REGISTRATIONS" == "1" ]]; then
      echo "warning: pluginkit shows $record_count records for one FileProvider path" >&2
    else
      echo "multiple FileProvider registrations found for one path; remove duplicate PlugInKit records or pass --allow-multiple-plugin-registrations for diagnostic runs" >&2
      print_pluginkit_duplicate_hint "$output"
      exit 1
    fi
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

elect_plugin_use() {
  [[ "$ELECT_PLUGIN_USE" == "1" ]] || return 0

  echo "electing FileProvider plug-in for current user: $PLUGIN_ID"
  pluginkit -e use -i "$PLUGIN_ID" 2>&1 || {
    echo "pluginkit user election failed for $PLUGIN_ID" >&2
    exit 1
  }
}

require_fileprovider_testing_mode_entitlement() {
  local entitlements_file="$LOG_DIR/host-entitlements.plist"
  local err_file="$LOG_DIR/host-entitlements.err"
  local plutil_bin=""

  codesign -d --entitlements :- "$APP_PATH" >"$entitlements_file" 2>"$err_file" || {
    echo "could not read host app entitlements for FileProvider testing mode: $APP_PATH" >&2
    cat "$err_file" >&2 || true
    exit 1
  }

  if command -v plutil >/dev/null 2>&1; then
    plutil_bin="$(command -v plutil)"
  elif [[ -x /usr/bin/plutil ]]; then
    plutil_bin="/usr/bin/plutil"
  fi

  if [[ -n "$plutil_bin" ]] &&
    [[ "$("$plutil_bin" -extract 'com\.apple\.developer\.fileprovider\.testing-mode' raw -o - "$entitlements_file" 2>/dev/null || true)" == "true" ]]; then
    echo "host app FileProvider testing-mode entitlement present"
    return 0
  fi

  awk '
    BEGIN {
      RS = "\0"
    }
    /<key>com[.]apple[.]developer[.]fileprovider[.]testing-mode<\/key>[[:space:]]*<true[[:space:]]*\/>/ {
      ok = 1
    }
    END {
      exit ok ? 0 : 1
    }
  ' "$entitlements_file" || {
    echo "host app missing com.apple.developer.fileprovider.testing-mode entitlement" >&2
    echo "rebuild with TCFS_FILEPROVIDER_TESTING_MODE_ENTITLEMENT=1 and an Apple profile that grants the entitlement" >&2
    exit 1
  }

  echo "host app FileProvider testing-mode entitlement present"
}

enable_fileprovider_testing_mode() {
  [[ "$FILEPROVIDER_TESTING_MODE" == "1" ]] || return 0

  require_fileprovider_testing_mode_entitlement
  echo "requesting FileProvider testing mode: always enabled"
  launchctl setenv TCFS_FILEPROVIDER_TESTING_MODE_ALWAYS_ENABLED 1 || {
    echo "failed to set FileProvider testing-mode launch environment" >&2
    exit 1
  }
}

clear_fileprovider_testing_mode() {
  [[ "$FILEPROVIDER_TESTING_MODE" == "1" ]] || return 0

  launchctl unsetenv TCFS_FILEPROVIDER_TESTING_MODE_ALWAYS_ENABLED >/dev/null 2>&1 || true
}

clear_fileprovider_request_download() {
  launchctl unsetenv TCFS_FILEPROVIDER_REQUEST_DOWNLOAD_IDENTIFIER >/dev/null 2>&1 || true
}

clear_fileprovider_evict() {
  launchctl unsetenv TCFS_FILEPROVIDER_EVICT_IDENTIFIER >/dev/null 2>&1 || true
}

clear_fileprovider_action_nonce() {
  launchctl unsetenv TCFS_FILEPROVIDER_ACTION_NONCE >/dev/null 2>&1 || true
}

new_fileprovider_action_nonce() {
  printf 'tcfs-smoke-%s-%s-%s\n' "$$" "${RANDOM:-0}" "${RANDOM:-0}"
}

host_app_binary_path() {
  local info_plist="$APP_PATH/Contents/Info.plist"
  local executable_name

  if [[ -f "$info_plist" ]] && [[ -x /usr/libexec/PlistBuddy ]]; then
    executable_name="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$info_plist" 2>/dev/null || true)"
  fi
  [[ -n "${executable_name:-}" ]] || executable_name="$(basename "$APP_PATH" .app)"

  printf '%s/Contents/MacOS/%s\n' "$APP_PATH" "$executable_name"
}

launch_host_app_for_domain_add() {
  local app_binary
  local deadline
  local host_launch_pid=""
  local -a host_env

  app_binary="$(host_app_binary_path)"
  if [[ "$REBUILD_DOMAIN" == "1" && ! -x "$app_binary" ]]; then
    echo "--rebuild-domain requires an executable host app binary: $app_binary" >&2
    exit 1
  fi

  if [[ ("$FILEPROVIDER_TESTING_MODE" == "1" || "$DIRECT_HOST_LAUNCH" == "1" || "$REBUILD_DOMAIN" == "1") && -x "$app_binary" ]]; then
    echo "launching host app binary for domain add: $app_binary"
    host_env=(
      TCFS_FILEPROVIDER_HOST_STDERR_LOG=1
      TCFS_FILEPROVIDER_HOST_ACTION_TIMEOUT_SECS="${TCFS_FILEPROVIDER_HOST_ACTION_TIMEOUT_SECS:-30}"
    )
    if [[ "$FILEPROVIDER_TESTING_MODE" == "1" ]]; then
      host_env+=(TCFS_FILEPROVIDER_TESTING_MODE_ALWAYS_ENABLED=1)
    fi
    if [[ "$REBUILD_DOMAIN" == "1" ]]; then
      host_env+=(TCFS_FILEPROVIDER_REBUILD_DOMAIN=1)
    fi
    env "${host_env[@]}" "$app_binary" >"$LOG_DIR/host-domain-launch.log" 2>&1 &
    host_launch_pid="$!"
  else
    echo "launching host app: $APP_PATH"
    open "$APP_PATH"
  fi

  HOST_LOG_WAIT=0
  deadline=$((SECONDS + TIMEOUT_SECS))
  until check_host_log; do
    short_pause
    HOST_LOG_WAIT=$((HOST_LOG_WAIT + 1))
    if (( SECONDS >= deadline )); then
      echo "timed out waiting for host app log showing domain add" >&2
      if [[ -f "$LOG_DIR/host-domain-launch.log" ]]; then
        echo "host app domain-launch log: $LOG_DIR/host-domain-launch.log" >&2
        cat "$LOG_DIR/host-domain-launch.log" >&2 || true
      fi
      if [[ -n "$host_launch_pid" ]]; then
        kill "$host_launch_pid" 2>/dev/null || true
        wait "$host_launch_pid" 2>/dev/null || true
      fi
      run_log_show --style compact --last 2m \
        --predicate 'subsystem == "io.tinyland.tcfs" && category == "host"' || true
      exit 1
    fi
  done

  if [[ -n "$host_launch_pid" ]]; then
    wait_for_pid_with_timeout "$host_launch_pid" 20 "host app domain add helper" || true
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
  local direct_log="$LOG_DIR/host-domain-launch.log"
  local output

  if [[ -f "$direct_log" ]]; then
    if grep -Fq "add: OK" "$direct_log"; then
      return 0
    fi
    if grep -Eq '^add: ' "$direct_log"; then
      cat "$direct_log" >&2 || true
      echo "FileProvider host app domain add failed" >&2
      exit 1
    fi
  fi

  output="$(run_log_show --style compact --last 45s \
    --predicate 'subsystem == "io.tinyland.tcfs" && category == "host"' || true)"

  [[ -n "$output" ]] || return 1
  if grep -q "add: OK" <<<"$output"; then
    return 0
  fi
  if grep -Eq 'add: ' <<<"$output"; then
    printf '%s\n' "$output" >&2
    echo "FileProvider host app domain add failed" >&2
    exit 1
  fi
  return 1
}

check_host_download_request_log() {
  local item_identifier="$1"
  local action_nonce="${2:-}"
  local direct_log="$LOG_DIR/host-request-launch.log"
  local success_pattern="requestDownload: $item_identifier: OK"
  local failure_output
  local output

  if [[ -f "$direct_log" ]]; then
    if [[ -n "$action_nonce" ]]; then
      if grep -Fq "$success_pattern nonce=$action_nonce" "$direct_log"; then
        return 0
      fi
      failure_output="$(grep -F "requestDownload: $item_identifier:" "$direct_log" | grep -F "nonce=$action_nonce" || true)"
    else
      if grep -Fq "$success_pattern" "$direct_log"; then
        return 0
      fi
      failure_output="$(grep -F "requestDownload: $item_identifier:" "$direct_log" || true)"
    fi
    if [[ -n "$failure_output" ]]; then
      if grep -Fq "$success_pattern" <<<"$failure_output"; then
        return 0
      fi
      printf '%s\n' "$failure_output" >&2
      echo "FileProvider host app download request failed" >&2
      exit 1
    fi
  fi

  output="$(run_log_show --style compact --last 45s \
    --predicate 'subsystem == "io.tinyland.tcfs" && category == "host"' || true)"

  [[ -n "$output" ]] || return 1
  if [[ -n "$action_nonce" ]]; then
    success_pattern="$success_pattern nonce=$action_nonce"
  fi
  if grep -Fq "$success_pattern" <<<"$output"; then
    return 0
  fi

  failure_output="$(grep -F "requestDownload: $item_identifier:" <<<"$output" || true)"
  if [[ -n "$action_nonce" && -n "$failure_output" ]]; then
    failure_output="$(grep -F "nonce=$action_nonce" <<<"$failure_output" || true)"
  fi
  if [[ -n "$failure_output" ]]; then
    if grep -Fq "$success_pattern" <<<"$failure_output"; then
      return 0
    fi
    printf '%s\n' "$failure_output" >&2
    echo "FileProvider host app download request failed" >&2
    exit 1
  fi

  return 1
}

check_host_evict_log() {
  local item_identifier="$1"
  local action_nonce="${2:-}"
  local direct_log="$LOG_DIR/host-evict-launch.log"
  local success_pattern="evict: $item_identifier: OK"
  local failure_output
  local output

  if [[ -f "$direct_log" ]]; then
    if [[ -n "$action_nonce" ]]; then
      if grep -Fq "$success_pattern nonce=$action_nonce" "$direct_log"; then
        return 0
      fi
      failure_output="$(grep -F "evict: $item_identifier:" "$direct_log" | grep -F "nonce=$action_nonce" || true)"
    else
      if grep -Fq "$success_pattern" "$direct_log"; then
        return 0
      fi
      failure_output="$(grep -F "evict: $item_identifier:" "$direct_log" || true)"
    fi
    if [[ -n "$failure_output" ]]; then
      if grep -Fq "$success_pattern" <<<"$failure_output"; then
        return 0
      fi
      printf '%s\n' "$failure_output" >&2
      echo "FileProvider host app eviction request failed" >&2
      exit 1
    fi
  fi

  output="$(run_log_show --style compact --last 45s \
    --predicate 'subsystem == "io.tinyland.tcfs" && category == "host"' || true)"

  [[ -n "$output" ]] || return 1
  if [[ -n "$action_nonce" ]]; then
    success_pattern="$success_pattern nonce=$action_nonce"
  fi
  if grep -Fq "$success_pattern" <<<"$output"; then
    return 0
  fi

  failure_output="$(grep -F "evict: $item_identifier:" <<<"$output" || true)"
  if [[ -n "$action_nonce" && -n "$failure_output" ]]; then
    failure_output="$(grep -F "nonce=$action_nonce" <<<"$failure_output" || true)"
  fi
  if [[ -n "$failure_output" ]]; then
    if grep -Fq "$success_pattern" <<<"$failure_output"; then
      return 0
    fi
    printf '%s\n' "$failure_output" >&2
    echo "FileProvider host app eviction request failed" >&2
    exit 1
  fi

  return 1
}

validate_relative_file_path() {
  local path="$1"

  if [[ -z "$path" || "$path" == /* || "$path" == */ ]]; then
    echo "expected relative file path, got: $path" >&2
    exit 2
  fi

  local component
  IFS='/' read -r -a components <<<"$path"
  for component in "${components[@]}"; do
    if [[ -z "$component" || "$component" == "." || "$component" == ".." ]]; then
      echo "expected normalized relative file path, got: $path" >&2
      exit 2
    fi
  done
}

extension_log_path() {
  printf '%s/extension-config.log\n' "$LOG_DIR"
}

fileprovider_system_log_path() {
  printf '%s/fileprovider-system.log\n' "$LOG_DIR"
}

fileprovider_activity_log_path() {
  printf '%s/fileprovider-extension-activity.log\n' "$LOG_DIR"
}

collect_extension_config_log() {
  run_log_show --style compact --last 2m \
    --predicate 'subsystem == "io.tinyland.tcfs.fileprovider" && category == "extension" && eventMessage CONTAINS "loadConfig:"' \
    || true
}

collect_fileprovider_activity_log() {
  run_log_show --style compact --last 5m \
    --predicate 'subsystem == "io.tinyland.tcfs.fileprovider" && (category == "extension" || category == "enumerator") && (eventMessage CONTAINS[c] "loadConfig" || eventMessage CONTAINS[c] "createProvider" || eventMessage CONTAINS[c] "provider" || eventMessage CONTAINS[c] "fetchContents" || eventMessage CONTAINS[c] "fetch_with_progress" || eventMessage CONTAINS[c] "fetch failed" || eventMessage CONTAINS[c] "enumerateItems" || eventMessage CONTAINS[c] "enumerateProviderItems")' \
    || true
}

collect_fileprovider_system_log() {
  run_log_show --style compact --last 2m \
    --predicate '((subsystem == "com.apple.FileProvider" || process == "fileproviderd" || process == "Finder" || process == "filecoordinationd" || process == "amfid" || process == "taskgated-helper" || process == "syspolicyd" || process == "sandboxd" || process == "tccd" || process == "pkd" || process == "lsd") && (eventMessage CONTAINS[c] "io.tinyland.tcfs" || eventMessage CONTAINS[c] "TCFSProvider" || eventMessage CONTAINS[c] "Sync is not enabled" || eventMessage CONTAINS[c] "FP -2011" || eventMessage CONTAINS[c] "DomainDisabled" || eventMessage CONTAINS[c] "AppleSystemPolicy" || eventMessage CONTAINS[c] "Security policy would not allow")) || (eventMessage CONTAINS[c] "Security policy would not allow process" && eventMessage CONTAINS[c] "TCFSFileProvider")' \
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

wait_for_expected_parent() {
  local path="$1"
  local attempt=0

  [[ "$path" == "$CLOUD_ROOT" ]] && return

  while (( attempt < TIMEOUT_SECS )); do
    if [[ -d "$path" ]]; then
      return
    fi
    short_pause
    attempt=$((attempt + 1))
  done

  echo "timed out waiting for expected file parent: $path" >&2
  exit 1
}

wait_for_expected_parent_chain() {
  local target_parent="$1"
  local rel_parent
  local current
  local component

  [[ "$target_parent" == "$CLOUD_ROOT" ]] && return
  [[ "$target_parent" == "$CLOUD_ROOT/"* ]] || {
    echo "expected parent is outside CloudStorage root: $target_parent" >&2
    exit 1
  }

  rel_parent="${target_parent#"$CLOUD_ROOT"/}"
  current="$CLOUD_ROOT"

  IFS='/' read -r -a components <<<"$rel_parent"
  for component in "${components[@]}"; do
    [[ -n "$component" ]] || continue
    current="$current/$component"
    wait_for_expected_parent "$current"
    nudge_expected_parent_enumeration "$current"
  done
}

nudge_cloud_root_enumeration() {
  local root="$1"
  local fileproviderctl_help=""

  echo "nudging CloudStorage enumeration"

  run_bounded_to_log \
    "cloud-root-ls" \
    "$LOG_SHOW_TIMEOUT_SECS" \
    ls -la "$root" || true

  # A headed workstation usually triggers FileProvider enumeration through
  # Finder naturally. GitHub-hosted macOS runners can create the CloudStorage
  # root without launching the extension until something explicitly opens or
  # materializes it, so use bounded best-effort nudges before the hard wait.
  run_bounded_to_log \
    "cloud-root-open" \
    "$LOG_SHOW_TIMEOUT_SECS" \
    open "$root" || true

  if command -v fileproviderctl >/dev/null 2>&1; then
    fileproviderctl_help="$(fileproviderctl 2>&1 || true)"

    if grep -q 'materialize' <<<"$fileproviderctl_help"; then
      run_bounded_to_log \
        "fileproviderctl-materialize-root" \
        "$LOG_SHOW_TIMEOUT_SECS" \
        fileproviderctl materialize "$root" || true
    else
      printf 'fileproviderctl materialize is unavailable on this host\n' \
        >"$LOG_DIR/fileproviderctl-materialize-root.log"
    fi

    if grep -q 'evaluate' <<<"$fileproviderctl_help"; then
      run_bounded_to_log \
        "fileproviderctl-evaluate-root" \
        "$LOG_SHOW_TIMEOUT_SECS" \
        fileproviderctl evaluate "$root" || true
    fi

    if grep -q 'check | repair' <<<"$fileproviderctl_help"; then
      run_bounded_to_log \
        "fileproviderctl-check-root" \
        "$LOG_SHOW_TIMEOUT_SECS" \
        fileproviderctl check -P -a "$root" || true
    fi
  fi

  run_coordinated_root_listing "$root" "cloud-root-coordinated-list-nudge" >/dev/null || true
}

nudge_expected_parent_enumeration() {
  local parent="$1"
  local fileproviderctl_help=""

  [[ "$parent" == "$CLOUD_ROOT" ]] && return

  echo "nudging expected parent enumeration: $parent"

  run_bounded_to_log \
    "expected-parent-ls" \
    "$LOG_SHOW_TIMEOUT_SECS" \
    ls -la "$parent" || true

  run_bounded_to_log \
    "expected-parent-open" \
    "$LOG_SHOW_TIMEOUT_SECS" \
    open "$parent" || true

  if command -v fileproviderctl >/dev/null 2>&1; then
    fileproviderctl_help="$(fileproviderctl 2>&1 || true)"

    if grep -q 'evaluate' <<<"$fileproviderctl_help"; then
      run_bounded_to_log \
        "fileproviderctl-evaluate-expected-parent" \
        "$LOG_SHOW_TIMEOUT_SECS" \
        fileproviderctl evaluate "$parent" || true
    fi

    if grep -q 'check | repair' <<<"$fileproviderctl_help"; then
      run_bounded_to_log \
        "fileproviderctl-check-expected-parent" \
        "$LOG_SHOW_TIMEOUT_SECS" \
        fileproviderctl check -P -a "$parent" || true
    fi
  fi
}

collect_cloud_root_permission_inventory() {
  local root="$1"
  local parent

  parent="$(dirname "$root")"
  echo "collecting CloudStorage root permission inventory: $root" >&2

  run_bounded_to_log \
    "cloud-root-stat" \
    "$LOG_SHOW_TIMEOUT_SECS" \
    sh -c '
      for path in "$1" "$2"; do
        printf -- "--- %s ---\n" "$path"
        if command -v stat >/dev/null 2>&1; then
          stat -x "$path" 2>/dev/null || stat "$path" 2>/dev/null || true
        else
          printf "stat unavailable\n"
        fi
        ls -ldeO@ "$path" 2>/dev/null || ls -la "$path" 2>/dev/null || true
      done
    ' sh "$root" "$parent" || true

  run_bounded_to_log \
    "cloud-root-access" \
    "$LOG_SHOW_TIMEOUT_SECS" \
    sh -c '
      id || true
      printf "pwd=%s\n" "$(pwd)"
      for path in "$1" "$2"; do
        printf -- "--- access %s ---\n" "$path"
        if [ -e "$path" ]; then printf "exists=yes\n"; else printf "exists=no\n"; fi
        if [ -r "$path" ]; then printf "readable=yes\n"; else printf "readable=no\n"; fi
        if [ -x "$path" ]; then printf "searchable=yes\n"; else printf "searchable=no\n"; fi
      done
    ' sh "$root" "$parent" || true

  if command -v xattr >/dev/null 2>&1; then
    run_bounded_to_log \
      "cloud-root-xattr" \
      "$LOG_SHOW_TIMEOUT_SECS" \
      sh -c '
        for path in "$1" "$2"; do
          printf -- "--- %s ---\n" "$path"
          xattr -l "$path" 2>/dev/null || true
        done
      ' sh "$root" "$parent" || true
  else
    printf 'xattr unavailable on this host\n' >"$LOG_DIR/cloud-root-xattr.log"
  fi

  if command -v fileproviderctl >/dev/null 2>&1; then
    run_bounded_to_log \
      "fileproviderctl-domain-list" \
      "$LOG_SHOW_TIMEOUT_SECS" \
      fileproviderctl domain list || true
  fi

  if command -v launchctl >/dev/null 2>&1; then
    run_bounded_to_log \
      "launchctl-fileprovider-state" \
      "$LOG_SHOW_TIMEOUT_SECS" \
      sh -c 'launchctl print "gui/$(id -u)" 2>/dev/null | grep -E "io\\.tinyland\\.tcfs|TCFSProvider|fileprovider" || true' || true
  fi
}

collect_expected_file_diagnostics() {
  local path="$1"
  local rel="$2"
  local parent
  local fileproviderctl_help=""

  parent="$(dirname "$path")"
  echo "collecting FileProvider diagnostics for expected file: $rel" >&2

  archive_expected_remote_index "$rel"
  collect_extension_config_log >"$(extension_log_path)" || true
  collect_fileprovider_activity_log >"$(fileprovider_activity_log_path)" || true
  collect_fileprovider_system_log >"$(fileprovider_system_log_path)" || true
  collect_cloud_root_permission_inventory "$CLOUD_ROOT" || true

  run_bounded_to_log \
    "expected-file-ls" \
    "$LOG_SHOW_TIMEOUT_SECS" \
    sh -c 'ls -lO@ "$1" 2>/dev/null || ls -la "$1"' sh "$path" || true

  run_bounded_to_log \
    "expected-file-stat" \
    "$LOG_SHOW_TIMEOUT_SECS" \
    sh -c 'stat -x "$1" 2>/dev/null || stat "$1"' sh "$path" || true

  if command -v xattr >/dev/null 2>&1; then
    run_bounded_to_log \
      "expected-file-xattr" \
      "$LOG_SHOW_TIMEOUT_SECS" \
      xattr -l "$path" || true
  else
    printf 'xattr unavailable on this host\n' >"$LOG_DIR/expected-file-xattr.log"
  fi

  if command -v fileproviderctl >/dev/null 2>&1; then
    fileproviderctl_help="$(fileproviderctl 2>&1 || true)"

    if grep -q 'evaluate' <<<"$fileproviderctl_help"; then
      run_bounded_to_log \
        "fileproviderctl-evaluate-expected-file" \
        "$LOG_SHOW_TIMEOUT_SECS" \
        fileproviderctl evaluate "$path" || true
    fi

    if grep -q 'check | repair' <<<"$fileproviderctl_help"; then
      run_bounded_to_log \
        "fileproviderctl-check-expected-file" \
        "$LOG_SHOW_TIMEOUT_SECS" \
        fileproviderctl check -P -a "$path" || true
      run_bounded_to_log \
        "fileproviderctl-check-expected-parent-postfailure" \
        "$LOG_SHOW_TIMEOUT_SECS" \
        fileproviderctl check -P -a "$parent" || true
    fi
  fi
}

classify_expected_file_read_failure() {
  local path="$1"
  local read_error="$2"
  local classification_path="$LOG_DIR/failure-classification.txt"
  local evidence_path="$LOG_DIR/failure-classification-input.log"
  local file

  : >"$evidence_path"
  for file in \
    "$read_error" \
    "$LOG_DIR/cloud-root-ls.log" \
    "$LOG_DIR/cloud-root-open.log" \
    "$LOG_DIR/cloud-root-find.log" \
    "$LOG_DIR/cloud-root-find.err" \
    "$LOG_DIR/cloud-root-stat.log" \
    "$LOG_DIR/cloud-root-access.log" \
    "$LOG_DIR/cloud-root-xattr.log" \
    "$LOG_DIR/fileproviderctl-domain-list.log" \
    "$LOG_DIR/launchctl-fileprovider-state.log" \
    "$LOG_DIR/expected-parent-ls.log" \
    "$LOG_DIR/expected-parent-open.log" \
    "$LOG_DIR/expected-file-ls.log" \
    "$LOG_DIR/expected-file-stat.log" \
    "$LOG_DIR/expected-file-xattr.log" \
    "$LOG_DIR/fileproviderctl-evaluate-expected-file.log" \
    "$LOG_DIR/fileproviderctl-check-expected-file.log"
  do
    [[ -f "$file" ]] || continue
    {
      printf -- '--- %s ---\n' "$file"
      cat "$file"
      printf '\n'
    } >>"$evidence_path" || true
  done

  if grep -Eiq \
    'Operation not permitted|NSPOSIXErrorDomain\(1\)|NSCocoaErrorDomain code=257|permission to view|Permission denied|EPERM' \
    "$evidence_path"; then
    {
      printf 'classification=cloudstorage_permission_denied\n'
      printf 'path=%s\n' "$path"
      printf 'reason=CloudStorage/FileProvider access returned EPERM or NSCocoaErrorDomain 257 after requestDownload\n'
      printf 'evidence=%s\n' "$evidence_path"
    } >"$classification_path"
    echo "FileProvider CloudStorage permission denied for expected file: $path" >&2
    echo "classification: cloudstorage_permission_denied" >&2
    echo "classification log: $classification_path" >&2
    return 0
  fi

  return 1
}

classify_cloud_root_enumeration_failure() {
  local root="$1"
  local classification_path="$LOG_DIR/failure-classification.txt"
  local evidence_path="$LOG_DIR/failure-classification-input.log"
  local file

  : >"$evidence_path"
  for file in \
    "$LOG_DIR/cloud-root-ls.log" \
    "$LOG_DIR/cloud-root-open.log" \
    "$LOG_DIR/cloud-root-find.log" \
    "$LOG_DIR/cloud-root-find.err" \
    "$LOG_DIR/cloud-root-coordinated-list.log" \
    "$LOG_DIR/cloud-root-coordinated-list-nudge.log" \
    "$LOG_DIR/host-root-probe.log" \
    "$LOG_DIR/cloud-root-stat.log" \
    "$LOG_DIR/cloud-root-access.log" \
    "$LOG_DIR/cloud-root-xattr.log" \
    "$LOG_DIR/fileproviderctl-materialize-root.log" \
    "$LOG_DIR/fileproviderctl-evaluate-root.log" \
    "$LOG_DIR/fileproviderctl-check-root.log" \
    "$LOG_DIR/fileproviderctl-domain-list.log" \
    "$LOG_DIR/launchctl-fileprovider-state.log" \
    "$(fileprovider_system_log_path)"
  do
    [[ -f "$file" ]] || continue
    {
      printf -- '--- %s ---\n' "$file"
      cat "$file"
      printf '\n'
    } >>"$evidence_path" || true
  done

  if grep -Eiq \
    'Operation not permitted|NSPOSIXErrorDomain\(1\)|NSCocoaErrorDomain code=257|permission to view|Permission denied|EPERM' \
    "$evidence_path"; then
    {
      printf 'classification=cloudstorage_root_permission_denied\n'
      printf 'path=%s\n' "$root"
      printf 'reason=CloudStorage/FileProvider root enumeration returned EPERM before expected-file requestDownload\n'
      printf 'evidence=%s\n' "$evidence_path"
    } >"$classification_path"
    echo "FileProvider CloudStorage root permission denied: $root" >&2
    echo "classification: cloudstorage_root_permission_denied" >&2
    echo "classification log: $classification_path" >&2
    return 0
  fi

  if grep -Eiq 'failed to start FPFS for domain' "$evidence_path"; then
    {
      printf 'classification=fileprovider_fpfs_start_failed\n'
      printf 'path=%s\n' "$root"
      printf 'reason=fileproviderd reported FPFS startup failure before CloudStorage enumeration produced entries\n'
      printf 'evidence=%s\n' "$evidence_path"
    } >"$classification_path"
    echo "FileProvider FPFS startup failed before CloudStorage enumeration: $root" >&2
    echo "classification: fileprovider_fpfs_start_failed" >&2
    echo "classification log: $classification_path" >&2
    return 0
  fi

  return 1
}

run_coordinated_root_listing() {
  local root="$1"
  local label="$2"
  local helper="$REPO_ROOT/scripts/macos-fileprovider-coordinated-list.swift"
  local listing

  [[ "$COORDINATED_READ" != "0" ]] || return 2
  [[ -f "$helper" ]] || return 2
  command -v swift >/dev/null 2>&1 || return 2

  run_bounded_to_log \
    "$label" \
    "$LOG_SHOW_TIMEOUT_SECS" \
    swift "$helper" "$root" || return "$?"

  listing="$(cat "$LOG_DIR/${label}.log" 2>/dev/null || true)"
  [[ -n "$listing" ]] || return 1
  printf '%s\n' "$listing"
}

run_host_root_probe() {
  local root="$1"
  local app_binary
  local action_nonce
  local host_probe_pid
  local status=0
  local log_file="$LOG_DIR/host-root-probe.log"

  app_binary="$(host_app_binary_path)"
  [[ -x "$app_binary" ]] || return 2

  echo "launching host app binary for root probe: $app_binary" >&2
  action_nonce="$(new_fileprovider_action_nonce)"
  TCFS_FILEPROVIDER_ACTION_NONCE="$action_nonce" \
  TCFS_FILEPROVIDER_ROOT_PROBE=1 \
  TCFS_FILEPROVIDER_ROOT_PROBE_PATH="$root" \
  TCFS_FILEPROVIDER_HOST_STDERR_LOG=1 \
  TCFS_FILEPROVIDER_HOST_ACTION_TIMEOUT_SECS="${TCFS_FILEPROVIDER_HOST_ACTION_TIMEOUT_SECS:-30}" \
    "$app_binary" >"$log_file" 2>&1 &
  host_probe_pid="$!"

  wait_for_pid_with_timeout "$host_probe_pid" 20 "host app root probe helper" || status="$?"
  [[ "$status" == "0" ]] || return "$status"

  awk -v nonce="nonce=$action_nonce" '
    index($0, "rootProbe:") && index($0, " entry: ") && index($0, nonce) {
      line = $0
      sub(/^.* entry: /, "", line)
      sub(/[[:space:]]+nonce=.*$/, "", line)
      print line
      found = 1
    }
    END { exit found ? 0 : 1 }
  ' "$log_file"
}

require_host_root_probe() {
  local root="$1"
  local host_probe_listing
  local status=0

  host_probe_listing="$(run_host_root_probe "$root")" || status="$?"
  if [[ "$status" != "0" ]]; then
    echo "host app root probe failed for $root (status $status)" >&2
    if [[ -f "$LOG_DIR/host-root-probe.log" ]]; then
      echo "host app root probe log: $LOG_DIR/host-root-probe.log" >&2
      cat "$LOG_DIR/host-root-probe.log" >&2 || true
    fi
    exit 1
  fi

  echo "host app root probe sample:"
  echo "$host_probe_listing"
}

enumerate_root() {
  local root="$1"
  local listing
  local coordinated_listing
  local host_probe_listing
  local attempt=0
  local system_log

  while (( attempt < TIMEOUT_SECS )); do
    if (( attempt % 5 == 0 )); then
      run_bounded_append_to_log \
        "cloud-root-ls" \
        "$LOG_SHOW_TIMEOUT_SECS" \
        ls -la "$root" || true
    fi
    run_bounded_to_log \
      "cloud-root-find" \
      "$LOG_SHOW_TIMEOUT_SECS" \
      sh -c 'find "$1" -mindepth 1 -maxdepth 4 2>"$2" | head -n 10' sh "$root" "$LOG_DIR/cloud-root-find.err" || true
    listing="$(cat "$LOG_DIR/cloud-root-find.log" 2>/dev/null || true)"
    if [[ -n "$listing" ]]; then
      echo "enumeration sample:"
      echo "$listing"
      return
    fi
    if coordinated_listing="$(run_coordinated_root_listing "$root" "cloud-root-coordinated-list")"; then
      echo "coordinated enumeration sample:"
      echo "$coordinated_listing"
      return
    fi
    short_pause
    attempt=$((attempt + 1))
  done

  if host_probe_listing="$(run_host_root_probe "$root")"; then
    echo "host app root probe sample:"
    echo "$host_probe_listing"
    HOST_APP_ROOT_ENUMERATION=1
    return
  fi

  system_log="$(collect_fileprovider_system_log)"
  printf '%s\n' "$system_log" >"$(fileprovider_system_log_path)"
  collect_cloud_root_permission_inventory "$root" || true
  if grep -Eqi 'Sync is not enabled|FP -2011|NSFileProviderErrorDomain Code=-2011|NSFileProviderErrorDomainDisabled|DomainDisabled' <<<"$system_log"; then
    echo "FileProvider domain is disabled by macOS (NSFileProviderErrorDomain -2011)" >&2
    echo "The package installed and registered, but macOS has not user-enabled the provider for this account." >&2
    echo "Enable the File Provider in System Settings/Login Items & Extensions, or use a test build with Apple's FileProvider testing-mode entitlement for headless CI." >&2
    echo "diagnostic log: $(fileprovider_system_log_path)" >&2
    exit 1
  fi

  classify_cloud_root_enumeration_failure "$root" || true
  echo "enumeration found no entries under $root" >&2
  exit 1
}

read_fileprovider_file() {
  local path="$1"
  local hydrated_copy="$2"
  local helper="$REPO_ROOT/scripts/macos-fileprovider-coordinated-read.swift"
  local pid

  if [[ "$COORDINATED_READ" != "0" && -f "$helper" ]] && command -v swift >/dev/null 2>&1; then
    swift "$helper" "$path" "$hydrated_copy" &
    pid="$!"
    wait_for_pid_with_timeout "$pid" "$READ_TIMEOUT_SECS" "coordinated FileProvider read"
  else
    cat "$path" >"$hydrated_copy" &
    pid="$!"
    wait_for_pid_with_timeout "$pid" "$READ_TIMEOUT_SECS" "FileProvider read"
  fi
}

request_expected_file_download() {
  local item_identifier="$1"
  local app_binary
  local action_nonce
  local deadline
  local host_request_pid=""
  local used_launchctl_env=0
  local wait_count=0

  echo "requesting FileProvider download for expected file: $item_identifier"

  clear_fileprovider_request_download
  clear_fileprovider_action_nonce
  action_nonce="$(new_fileprovider_action_nonce)"
  app_binary="$(host_app_binary_path)"
  if [[ -x "$app_binary" ]]; then
    echo "launching host app binary for download request: $app_binary"
    TCFS_FILEPROVIDER_ACTION_NONCE="$action_nonce" \
    TCFS_FILEPROVIDER_REQUEST_DOWNLOAD_IDENTIFIER="$item_identifier" \
    TCFS_FILEPROVIDER_HOST_STDERR_LOG=1 \
    TCFS_FILEPROVIDER_HOST_ACTION_TIMEOUT_SECS="${TCFS_FILEPROVIDER_HOST_ACTION_TIMEOUT_SECS:-30}" \
      "$app_binary" >"$LOG_DIR/host-request-launch.log" 2>&1 &
    host_request_pid="$!"
  else
    echo "warning: host app binary not executable at $app_binary; falling back to LaunchServices" >&2
    used_launchctl_env=1
    launchctl setenv TCFS_FILEPROVIDER_ACTION_NONCE "$action_nonce" || {
      echo "failed to set FileProvider action nonce launch environment" >&2
      exit 1
    }
    launchctl setenv TCFS_FILEPROVIDER_REQUEST_DOWNLOAD_IDENTIFIER "$item_identifier" || {
      echo "failed to set FileProvider request-download launch environment" >&2
      exit 1
    }
    open -n "$APP_PATH" || open "$APP_PATH"
  fi

  deadline=$((SECONDS + TIMEOUT_SECS))
  until check_host_download_request_log "$item_identifier" "$action_nonce"; do
    short_pause
    wait_count=$((wait_count + 1))
    if (( SECONDS >= deadline )); then
      clear_fileprovider_request_download
      clear_fileprovider_action_nonce
      echo "timed out waiting for host app download request log: $item_identifier" >&2
      if [[ -f "$LOG_DIR/host-request-launch.log" ]]; then
        echo "host app request-launch log: $LOG_DIR/host-request-launch.log" >&2
        cat "$LOG_DIR/host-request-launch.log" >&2 || true
      fi
      run_log_show --style compact --last 2m \
        --predicate 'subsystem == "io.tinyland.tcfs" && category == "host"' || true
      exit 1
    fi
  done

  clear_fileprovider_request_download
  clear_fileprovider_action_nonce
  if [[ -n "$host_request_pid" ]]; then
    wait_for_pid_with_timeout "$host_request_pid" 20 "host app download request helper" || true
  fi
  if [[ "$used_launchctl_env" == "1" ]]; then
    clear_fileprovider_request_download
    clear_fileprovider_action_nonce
  fi
  echo "host app requested FileProvider download for expected file"
}

request_expected_file_eviction() {
  local item_identifier="$1"
  local app_binary
  local action_nonce
  local deadline
  local host_request_pid=""
  local used_launchctl_env=0
  local wait_count=0

  echo "requesting FileProvider eviction for expected file: $item_identifier"

  clear_fileprovider_evict
  clear_fileprovider_action_nonce
  action_nonce="$(new_fileprovider_action_nonce)"
  app_binary="$(host_app_binary_path)"
  if [[ -x "$app_binary" ]]; then
    echo "launching host app binary for eviction request: $app_binary"
    TCFS_FILEPROVIDER_ACTION_NONCE="$action_nonce" \
    TCFS_FILEPROVIDER_EVICT_IDENTIFIER="$item_identifier" \
    TCFS_FILEPROVIDER_HOST_STDERR_LOG=1 \
    TCFS_FILEPROVIDER_HOST_ACTION_TIMEOUT_SECS="${TCFS_FILEPROVIDER_HOST_ACTION_TIMEOUT_SECS:-30}" \
      "$app_binary" >"$LOG_DIR/host-evict-launch.log" 2>&1 &
    host_request_pid="$!"
  else
    echo "warning: host app binary not executable at $app_binary; falling back to LaunchServices" >&2
    used_launchctl_env=1
    launchctl setenv TCFS_FILEPROVIDER_ACTION_NONCE "$action_nonce" || {
      echo "failed to set FileProvider action nonce launch environment" >&2
      exit 1
    }
    launchctl setenv TCFS_FILEPROVIDER_EVICT_IDENTIFIER "$item_identifier" || {
      echo "failed to set FileProvider eviction launch environment" >&2
      exit 1
    }
    open -n "$APP_PATH" || open "$APP_PATH"
  fi

  deadline=$((SECONDS + TIMEOUT_SECS))
  until check_host_evict_log "$item_identifier" "$action_nonce"; do
    short_pause
    wait_count=$((wait_count + 1))
    if (( SECONDS >= deadline )); then
      clear_fileprovider_evict
      clear_fileprovider_action_nonce
      echo "timed out waiting for host app eviction log: $item_identifier" >&2
      if [[ -f "$LOG_DIR/host-evict-launch.log" ]]; then
        echo "host app evict-launch log: $LOG_DIR/host-evict-launch.log" >&2
        cat "$LOG_DIR/host-evict-launch.log" >&2 || true
      fi
      run_log_show --style compact --last 2m \
        --predicate 'subsystem == "io.tinyland.tcfs" && category == "host"' || true
      exit 1
    fi
  done

  clear_fileprovider_evict
  clear_fileprovider_action_nonce
  if [[ -n "$host_request_pid" ]]; then
    wait_for_pid_with_timeout "$host_request_pid" 20 "host app eviction request helper" || true
  fi
  if [[ "$used_launchctl_env" == "1" ]]; then
    clear_fileprovider_evict
    clear_fileprovider_action_nonce
  fi
  echo "host app requested FileProvider eviction for expected file"
}

hydrate_expected_file() {
  local path="$1"
  local hydrated_copy="$LOG_DIR/hydrated-expected-file"
  local expected_copy="$LOG_DIR/expected-content"
  local read_error="$LOG_DIR/hydrate-read-error.log"
  local hydrated_bytes
  local deadline
  local read_status
  local attempt=0

  [[ -f "$path" ]] || {
    echo "expected path is not a regular file: $path" >&2
    exit 1
  }

  deadline=$((SECONDS + TIMEOUT_SECS))
  while (( SECONDS < deadline )); do
    if read_fileprovider_file "$path" "$hydrated_copy" 2>"$read_error"; then
      break
    else
      read_status="$?"
    fi
    rm -f "$hydrated_copy"
    if [[ "$read_status" == "124" ]]; then
      attempt="$TIMEOUT_SECS"
      break
    fi
    short_pause
    attempt=$((attempt + 1))
  done

  if (( attempt >= TIMEOUT_SECS || SECONDS >= deadline )); then
    cat "$read_error" >&2 || true
    collect_expected_file_diagnostics "$path" "$EXPECTED_FILE_REL"
    classify_expected_file_read_failure "$path" "$read_error" || true
    echo "failed to read expected file for hydration: $path" >&2
    exit 1
  fi

  echo "hydrated file: $path"
  hydrated_bytes="$(wc -c <"$hydrated_copy" | tr -d '[:space:]')"
  echo "  size: $hydrated_bytes bytes"

  if [[ -n "$EXPECTED_CONTENT_FILE" ]]; then
    if ! cmp -s "$EXPECTED_CONTENT_FILE" "$hydrated_copy"; then
      echo "hydrated file content mismatch against $EXPECTED_CONTENT_FILE" >&2
      collect_expected_file_diagnostics "$path" "$EXPECTED_FILE_REL"
      exit 1
    fi
    echo "hydrated file content matched expected content file"
  elif [[ -n "$EXPECTED_CONTENT" ]]; then
    printf '%s' "$EXPECTED_CONTENT" >"$expected_copy"
    if ! cmp -s "$expected_copy" "$hydrated_copy"; then
      echo "hydrated file content mismatch against --expected-content" >&2
      collect_expected_file_diagnostics "$path" "$EXPECTED_FILE_REL"
      exit 1
    fi
    echo "hydrated file content matched expected content"
  fi
}

prepare_mutation_content_file() {
  local target="$1"

  if [[ -n "$MUTATION_CONTENT_FILE" ]]; then
    cp "$MUTATION_CONTENT_FILE" "$target"
  elif [[ -n "$MUTATION_CONTENT" ]]; then
    printf '%s' "$MUTATION_CONTENT" >"$target"
  else
    printf 'tcfs FileProvider mutation smoke %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" >"$target"
  fi
}

prepare_conflict_content_file() {
  local target="$1"

  if [[ -n "$CONFLICT_CONTENT_FILE" ]]; then
    cp "$CONFLICT_CONTENT_FILE" "$target"
  else
    printf '%s' "$CONFLICT_CONTENT" >"$target"
  fi
}

resolve_config_path_value() {
  local section="$1"
  local key="$2"

  python3 - "$CONFIG_PATH" "$section" "$key" <<'PY'
import pathlib
import sys

try:
    import tomllib
except ModuleNotFoundError:
    import tomli as tomllib

config_path = pathlib.Path(sys.argv[1])
section = sys.argv[2]
key = sys.argv[3]
with config_path.open("rb") as fh:
    config = tomllib.load(fh)
value = config.get(section, {}).get(key)
if not value:
    raise SystemExit(1)
print(value)
PY
}

resolve_conflict_state_path() {
  if [[ -n "$CONFLICT_STATE_PATH" ]]; then
    printf '%s\n' "$CONFLICT_STATE_PATH"
    return
  fi

  python3 - "$CONFIG_PATH" <<'PY'
import pathlib
import sys

try:
    import tomllib
except ModuleNotFoundError:
    import tomli as tomllib

config_path = pathlib.Path(sys.argv[1])
with config_path.open("rb") as fh:
    config = tomllib.load(fh)
state_db = config.get("sync", {}).get("state_db")
if not state_db:
    raise SystemExit(1)
print(pathlib.Path(state_db).with_suffix(".json"))
PY
}

pull_mutation_from_remote() {
  local rel="$1"
  local expected_file="$2"
  local pulled_copy="$LOG_DIR/mutation-remote-pull"
  local attempt=0
  local status=0

  rm -f "$pulled_copy"
  while (( attempt < TIMEOUT_SECS )); do
    status=0
    if run_bounded_to_log \
      "mutation-remote-pull" \
      "$READ_TIMEOUT_SECS" \
      "$TCFS_PATH" --config "$CONFIG_PATH" pull "$rel" "$pulled_copy" --prefix "$REMOTE_PREFIX"; then
      if cmp -s "$expected_file" "$pulled_copy"; then
        echo "remote mutation pull matched expected content"
        return
      fi
      echo "remote mutation content mismatch for $rel" >&2
      exit 1
    else
      status="$?"
    fi

    if [[ "$status" == "124" ]]; then
      echo "remote mutation pull timed out for $rel" >&2
      cat "$LOG_DIR/mutation-remote-pull.log" >&2 || true
      exit 1
    fi
    short_pause
    attempt=$((attempt + 1))
  done

  echo "timed out waiting for mutation to appear in remote index: $rel" >&2
  cat "$LOG_DIR/mutation-remote-pull.log" >&2 || true
  exit 1
}

exercise_fileprovider_mutation() {
  local rel="$MUTATION_FILE_REL"
  local expected_file="$LOG_DIR/mutation-expected-content"
  local path
  local parent

  if [[ -z "$rel" ]]; then
    rel="tcfs-fileprovider-mutation-${USER:-user}-$$-${RANDOM:-0}.txt"
  fi
  validate_relative_file_path "$rel"

  path="$CLOUD_ROOT/$rel"
  parent="$(dirname "$path")"
  wait_for_expected_parent_chain "$parent"
  prepare_mutation_content_file "$expected_file"

  echo "writing FileProvider mutation fixture: $rel"
  run_bounded_to_log "mutation-write" "$TIMEOUT_SECS" cp "$expected_file" "$path" || {
    echo "FileProvider mutation write failed: $path" >&2
    cat "$LOG_DIR/mutation-write.log" >&2 || true
    exit 1
  }

  wait_for_expected_file "$path"
  if ! cmp -s "$expected_file" "$path"; then
    echo "FileProvider mutation local content mismatch: $path" >&2
    exit 1
  fi
  echo "FileProvider mutation local content matched"

  pull_mutation_from_remote "$rel" "$expected_file"
  run_status "post-mutation"
}

collect_conflict_enumerator_log() {
  run_log_show --style compact --last 2m \
    --predicate 'subsystem == "io.tinyland.tcfs.fileprovider" && category == "enumerator" && eventMessage CONTAINS "hydration_state=conflict"' \
    || true
}

exercise_fileprovider_conflict_status() {
  local rel="$CONFLICT_FILE_REL"
  local expected_file="$LOG_DIR/conflict-expected-content"
  local hydrated_copy="$LOG_DIR/conflict-hydrated-file"
  local read_error="$LOG_DIR/conflict-read-error.log"
  local status_log="$LOG_DIR/conflict-sync-status.log"
  local enum_log="$LOG_DIR/conflict-enumerator.log"
  local state_path
  local sync_root
  local sync_path
  local cloud_path
  local cloud_parent
  local attempt=0
  local deadline
  local read_status=0
  local enum_output

  validate_relative_file_path "$rel"
  prepare_conflict_content_file "$expected_file"

  state_path="$(resolve_conflict_state_path)" || {
    echo "could not resolve conflict state path from --state or config [sync].state_db" >&2
    exit 1
  }
  sync_root="${SYNC_ROOT_OVERRIDE:-$(resolve_config_path_value sync sync_root)}" || {
    echo "could not resolve conflict sync root from --sync-root or config [sync].sync_root" >&2
    exit 1
  }
  sync_path="$sync_root/$rel"
  cloud_path="$CLOUD_ROOT/$rel"
  cloud_parent="$(dirname "$cloud_path")"

  require_file "$state_path"
  require_file "$sync_path"

  if ! cmp -s "$expected_file" "$sync_path"; then
    echo "conflict fixture sync-root content mismatch: $sync_path" >&2
    exit 1
  fi

  "$TCFS_PATH" --config "$CONFIG_PATH" sync-status "$sync_path" --state "$state_path" >"$status_log" 2>&1 || {
    echo "tcfs sync-status failed for conflict fixture" >&2
    cat "$status_log" >&2 || true
    exit 1
  }
  cat "$status_log"
  grep -q "sync state: conflict" "$status_log" || {
    echo "tcfs sync-status did not report conflict for $sync_path" >&2
    exit 1
  }
  echo "CLI conflict status verified: $rel"

  wait_for_expected_parent_chain "$cloud_parent"
  wait_for_expected_file "$cloud_path"
  request_expected_file_download "$rel"

  deadline=$((SECONDS + TIMEOUT_SECS))
  while (( SECONDS < deadline )); do
    if read_fileprovider_file "$cloud_path" "$hydrated_copy" 2>"$read_error"; then
      break
    else
      read_status="$?"
    fi
    rm -f "$hydrated_copy"
    if [[ "$read_status" == "124" ]]; then
      attempt="$TIMEOUT_SECS"
      break
    fi
    short_pause
    attempt=$((attempt + 1))
  done

  if (( attempt >= TIMEOUT_SECS || SECONDS >= deadline )); then
    cat "$read_error" >&2 || true
    echo "failed to read conflict fixture through FileProvider: $cloud_path" >&2
    exit 1
  fi

  if ! cmp -s "$expected_file" "$hydrated_copy"; then
    echo "conflict fixture FileProvider content mismatch: $cloud_path" >&2
    exit 1
  fi
  echo "FileProvider conflict fixture content matched"

  enum_output="$(collect_conflict_enumerator_log)"
  printf '%s\n' "$enum_output" >"$enum_log"
  if grep -F "item=$rel hydration_state=conflict" "$enum_log" >/dev/null; then
    echo "FileProvider enumerator conflict status observed"
  else
    echo "warning: FileProvider enumerator did not log conflict hydration state for $rel; treating Finder status as captured evidence only" >&2
    cat "$enum_log" >&2 || true
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
  if [[ -n "$EXPECTED_FILE_REL" || "$SEED_EXPECTED_FILE" == "1" ]]; then
    TCFS_PATH="$(resolve_bin "$TCFS_BIN")"
  else
    TCFS_PATH=""
  fi
fi

seed_expected_file_if_requested

require_file "$HOME/.config/tcfs/fileprovider/config.json"
echo "status logs: $LOG_DIR"
check_pluginkit
elect_plugin_use
enable_fileprovider_testing_mode

launch_host_app_for_domain_add
clear_fileprovider_testing_mode
echo "host app log confirmed domain add"
if check_domain_listing; then
  echo "fileproviderctl domain listing includes $DOMAIN_ID"
else
  echo "warning: could not confirm domain via fileproviderctl; relying on host log + CloudStorage root" >&2
fi

CLOUD_ROOT="$(wait_for_cloud_root)"
echo "CloudStorage root: $CLOUD_ROOT"

if [[ "$HOST_ROOT_PROBE" == "1" ]]; then
  require_host_root_probe "$CLOUD_ROOT"
fi

nudge_cloud_root_enumeration "$CLOUD_ROOT"
enumerate_root "$CLOUD_ROOT"

if [[ -n "$EXPECTED_FILE_REL" ]]; then
  EXPECTED_PATH="$CLOUD_ROOT/$EXPECTED_FILE_REL"
  EXPECTED_PARENT="$(dirname "$EXPECTED_PATH")"
  require_expected_remote_index "$EXPECTED_FILE_REL"
  if [[ "$HOST_APP_ROOT_ENUMERATION" == "1" ]]; then
    request_expected_file_download "$EXPECTED_FILE_REL"
  fi
  wait_for_expected_parent_chain "$EXPECTED_PARENT"
  wait_for_expected_file "$EXPECTED_PATH"
  if [[ "$HOST_APP_ROOT_ENUMERATION" != "1" ]]; then
    request_expected_file_download "$EXPECTED_FILE_REL"
  fi
  if [[ "$REQUIRE_KEYCHAIN_CONFIG" == "1" ]]; then
    check_keychain_config_log
  fi
  hydrate_expected_file "$EXPECTED_PATH"
  if [[ "$EXERCISE_EVICT_REHYDRATE" == "1" ]]; then
    request_expected_file_eviction "$EXPECTED_FILE_REL"
    request_expected_file_download "$EXPECTED_FILE_REL"
    hydrate_expected_file "$EXPECTED_PATH"
    echo "FileProvider evict/rehydrate cycle passed"
  fi
else
  echo "warning: --expected-file not provided; hydration was not exercised" >&2
fi

if [[ "$EXERCISE_MUTATION" == "1" ]]; then
  exercise_fileprovider_mutation
fi

if [[ "$EXERCISE_CONFLICT_STATUS" == "1" ]]; then
  exercise_fileprovider_conflict_status
fi

run_status "post-hydrate"

echo "macOS post-install FileProvider smoke passed"
