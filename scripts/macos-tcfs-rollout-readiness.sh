#!/usr/bin/env bash
#
# Read-only rollout readiness check for the macOS TCFS daily-driver install.
#
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/macos-tcfs-rollout-readiness.sh [options]

Check whether the local macOS TCFS install is aligned with the repo/lab rollout
state before running FileProvider or cross-host proofs. The helper is read-only:
it does not run Home Manager, restart launchd, launch TCFSProvider.app, or
execute the LaunchAgent wrapper.

Options:
  --expected-version <ver>       Require tcfs/tcfsd --version to include <ver>
  --expected-tummycrypt-rev <sha>
                                 Require lab flake.lock tummycrypt rev to match
  --lab-root <path>              Lab checkout to inspect (default: ~/git/lab)
  --launch-agent <path>          TCFS LaunchAgent plist
                                 (default: ~/Library/LaunchAgents/dev.tinyland.tcfsd.plist)
  --launch-label <label>         launchd label (default: dev.tinyland.tcfsd)
  --app-path <path>              TCFSProvider.app path
                                 (default: ~/Applications/TCFSProvider.app)
  --plugin-id <id>               FileProvider extension id
                                 (default: io.tinyland.tcfs.fileprovider)
  --status-timeout <seconds>     Bound tcfs status (default: 8)
  --skip-status                  Do not run bounded tcfs status
  --tcfs <path-or-name>          CLI binary (default: tcfs)
  --tcfsd <path-or-name>         Daemon binary (default: tcfsd)
  -h, --help                     Show this help
EOF
}

EXPECTED_VERSION="${TCFS_EXPECTED_VERSION:-}"
EXPECTED_TUMMYCRYPT_REV="${TCFS_EXPECTED_TUMMYCRYPT_REV:-}"
LAB_ROOT="${TCFS_LAB_ROOT:-$HOME/git/lab}"
LAUNCH_AGENT="${TCFS_LAUNCH_AGENT:-$HOME/Library/LaunchAgents/dev.tinyland.tcfsd.plist}"
LAUNCH_LABEL="${TCFS_LAUNCH_LABEL:-dev.tinyland.tcfsd}"
APP_PATH="${TCFS_APP_PATH:-$HOME/Applications/TCFSProvider.app}"
PLUGIN_ID="${TCFS_PLUGIN_ID:-io.tinyland.tcfs.fileprovider}"
STATUS_TIMEOUT="${TCFS_STATUS_TIMEOUT:-8}"
SKIP_STATUS="${TCFS_SKIP_STATUS:-0}"
TCFS_BIN="${TCFS_BIN:-tcfs}"
TCFSD_BIN="${TCFSD_BIN:-tcfsd}"

LAUNCHCTL_BIN="${TCFS_LAUNCHCTL_BIN:-launchctl}"
PLISTBUDDY_BIN="${TCFS_PLISTBUDDY_BIN:-/usr/libexec/PlistBuddy}"
PLUGINKIT_BIN="${TCFS_PLUGINKIT_BIN:-pluginkit}"
SPCTL_BIN="${TCFS_SPCTL_BIN:-spctl}"
PYTHON_BIN="${TCFS_PYTHON_BIN:-python3}"
PERL_BIN="${TCFS_PERL_BIN:-perl}"

failures=0
warnings=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --expected-version)
      EXPECTED_VERSION="$2"
      shift 2
      ;;
    --expected-tummycrypt-rev)
      EXPECTED_TUMMYCRYPT_REV="$2"
      shift 2
      ;;
    --lab-root)
      LAB_ROOT="$2"
      shift 2
      ;;
    --launch-agent)
      LAUNCH_AGENT="$2"
      shift 2
      ;;
    --launch-label)
      LAUNCH_LABEL="$2"
      shift 2
      ;;
    --app-path)
      APP_PATH="$2"
      shift 2
      ;;
    --plugin-id)
      PLUGIN_ID="$2"
      shift 2
      ;;
    --status-timeout)
      STATUS_TIMEOUT="$2"
      shift 2
      ;;
    --skip-status)
      SKIP_STATUS=1
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

note() {
  printf '%s\n' "$*"
}

warn() {
  warnings=$((warnings + 1))
  printf 'warning: %s\n' "$*" >&2
}

fail() {
  failures=$((failures + 1))
  printf 'failure: %s\n' "$*" >&2
}

resolve_bin() {
  local candidate="$1"

  if [[ "$candidate" == */* ]]; then
    if [[ -x "$candidate" ]]; then
      printf '%s\n' "$candidate"
      return 0
    fi
    return 1
  fi

  command -v "$candidate" 2>/dev/null
}

check_darwin() {
  if [[ "$(uname -s)" != "Darwin" ]]; then
    fail "macOS rollout readiness only runs on Darwin"
  fi
}

check_version() {
  local label="$1"
  local requested="$2"
  local bin
  local output

  if ! bin="$(resolve_bin "$requested")"; then
    fail "$label binary not found or not executable: $requested"
    return
  fi

  if ! output="$("$bin" --version 2>&1)"; then
    fail "$label --version failed: $output"
    return
  fi

  note "$label binary: $bin"
  note "$label version: $output"

  if [[ -n "$EXPECTED_VERSION" && "$output" != *"$EXPECTED_VERSION"* ]]; then
    fail "$label version mismatch: expected output containing '$EXPECTED_VERSION'"
  fi
}

lab_tummycrypt_rev() {
  local lock="$LAB_ROOT/flake.lock"

  [[ -f "$lock" ]] || return 1
  command -v "$PYTHON_BIN" >/dev/null 2>&1 || return 1

  "$PYTHON_BIN" - "$lock" <<'PY'
import json
import sys

with open(sys.argv[1], "r", encoding="utf-8") as fh:
    data = json.load(fh)
print(data["nodes"]["tummycrypt"]["locked"]["rev"])
PY
}

check_lab_pin() {
  local rev

  if ! rev="$(lab_tummycrypt_rev 2>/dev/null)"; then
    warn "could not read lab tummycrypt rev from $LAB_ROOT/flake.lock"
    return
  fi

  note "lab tummycrypt rev: $rev"
  if [[ -n "$EXPECTED_TUMMYCRYPT_REV" && "$rev" != "$EXPECTED_TUMMYCRYPT_REV" ]]; then
    fail "lab tummycrypt rev mismatch: expected $EXPECTED_TUMMYCRYPT_REV"
  fi
}

plist_value() {
  local key="$1"
  local path="$2"

  [[ -f "$path" ]] || return 1
  "$PLISTBUDDY_BIN" -c "Print :$key" "$path" 2>/dev/null
}

check_launch_agent() {
  local program=""
  local status=""

  if [[ ! -f "$LAUNCH_AGENT" ]]; then
    fail "LaunchAgent plist not found: $LAUNCH_AGENT"
    return
  fi

  note "launch agent: $LAUNCH_AGENT"
  note "launch label: $LAUNCH_LABEL"

  if [[ -x "$PLISTBUDDY_BIN" ]]; then
    program="$(plist_value "ProgramArguments:2" "$LAUNCH_AGENT" || true)"
    if [[ -n "$program" ]]; then
      note "launch agent command: $program"
      note "launch agent command inspection: not executed"
    else
      warn "could not read ProgramArguments:2 from LaunchAgent"
    fi
  else
    warn "PlistBuddy not executable: $PLISTBUDDY_BIN"
  fi

  if command -v "$LAUNCHCTL_BIN" >/dev/null 2>&1; then
    status="$("$LAUNCHCTL_BIN" print "gui/$(id -u)/$LAUNCH_LABEL" 2>&1 || true)"
    if grep -Fq "Could not find service" <<<"$status"; then
      fail "launchd service not found: $LAUNCH_LABEL"
    else
      note "launchd service: present ($LAUNCH_LABEL)"
    fi
  else
    warn "launchctl not found"
  fi
}

check_app_bundle() {
  local host_version=""
  local extension_plist
  local extension_version=""
  local spctl_out=""

  if [[ ! -d "$APP_PATH" ]]; then
    fail "TCFSProvider.app not found: $APP_PATH"
    return
  fi

  note "app path: $APP_PATH"
  if [[ -x "$PLISTBUDDY_BIN" ]]; then
    host_version="$(plist_value "CFBundleShortVersionString" "$APP_PATH/Contents/Info.plist" || true)"
    [[ -n "$host_version" ]] && note "host app version: $host_version"

    extension_plist="$APP_PATH/Contents/Extensions/TCFSFileProvider.appex/Contents/Info.plist"
    extension_version="$(plist_value "CFBundleShortVersionString" "$extension_plist" || true)"
    [[ -n "$extension_version" ]] && note "extension version: $extension_version"
  fi

  if command -v "$SPCTL_BIN" >/dev/null 2>&1; then
    if spctl_out="$("$SPCTL_BIN" -a -vv "$APP_PATH" 2>&1)"; then
      note "Gatekeeper: accepted"
    else
      fail "Gatekeeper rejected $APP_PATH: $spctl_out"
    fi
  else
    warn "spctl not found"
  fi
}

check_pluginkit() {
  local output
  local count

  if ! command -v "$PLUGINKIT_BIN" >/dev/null 2>&1; then
    warn "pluginkit not found"
    return
  fi

  output="$("$PLUGINKIT_BIN" -m -A -p com.apple.fileprovider-nonui 2>&1 || true)"
  count="$(grep -F -c "$PLUGIN_ID" <<<"$output" || true)"

  note "FileProvider plugin registrations for $PLUGIN_ID: $count"
  case "$count" in
    0) fail "FileProvider plugin registration missing: $PLUGIN_ID" ;;
    1) ;;
    *) fail "multiple FileProvider plugin registrations for $PLUGIN_ID: $count" ;;
  esac
}

check_status() {
  local tcfs
  local tmp
  local rc=0

  [[ "$SKIP_STATUS" == "0" ]] || {
    note "tcfs status: skipped"
    return
  }

  if ! tcfs="$(resolve_bin "$TCFS_BIN")"; then
    fail "tcfs binary not found for status: $TCFS_BIN"
    return
  fi

  if ! command -v "$PERL_BIN" >/dev/null 2>&1; then
    warn "perl not found; cannot bound tcfs status"
    return
  fi

  tmp="$(mktemp "${TMPDIR:-/tmp}/tcfs-rollout-status.XXXXXX")"
  if "$PERL_BIN" -e 'alarm shift; exec @ARGV' "$STATUS_TIMEOUT" "$tcfs" status >"$tmp" 2>&1; then
    rc=0
  else
    rc=$?
  fi

  if [[ "$rc" == "142" ]]; then
    fail "tcfs status timed out after ${STATUS_TIMEOUT}s"
  elif [[ "$rc" != "0" ]]; then
    fail "tcfs status failed with rc=$rc"
  else
    note "tcfs status: returned within ${STATUS_TIMEOUT}s"
    if grep -Fq "[ok]" "$tmp"; then
      note "tcfs status storage marker: ok"
    else
      warn "tcfs status did not include an [ok] marker"
    fi
  fi

  rm -f "$tmp"
}

main() {
  check_darwin
  check_lab_pin
  check_version "tcfs" "$TCFS_BIN"
  check_version "tcfsd" "$TCFSD_BIN"
  check_launch_agent
  check_app_bundle
  check_pluginkit
  check_status

  note "rollout-readiness-warnings=$warnings"
  note "rollout-readiness-failures=$failures"

  if (( failures > 0 )); then
    note "rollout-readiness=fail"
    exit 1
  fi

  note "rollout-readiness=pass"
}

main "$@"
