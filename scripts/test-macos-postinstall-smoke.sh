#!/usr/bin/env bash
#
# Regression tests for macos-postinstall-smoke.sh using fake platform tools.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="${REPO_ROOT}/scripts/macos-postinstall-smoke.sh"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-macos-postinstall-test.XXXXXX")"
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

assert_fails_contains() {
  local expected="$1"
  shift

  local out="${TMPDIR}/failure.out"
  local err="${TMPDIR}/failure.err"

  if "$@" >"$out" 2>"$err"; then
    printf 'expected command to fail: %s\n' "$*" >&2
    exit 1
  fi

  cat "$out" "$err" >"${TMPDIR}/failure.combined"
  assert_contains "${TMPDIR}/failure.combined" "$expected"
}

FAKE_BIN="${TMPDIR}/fake-bin"
HOME_DIR="${TMPDIR}/home"
APP_PATH="${TMPDIR}/TCFSProvider.app"
CLOUD_ROOT="${HOME_DIR}/Library/CloudStorage/TCFS"
SYNC_ROOT="${TMPDIR}/sync-root"
STATE_PATH="${TMPDIR}/tcfs-state.json"
LOG_DIR="${TMPDIR}/logs"
CONFIG_PATH="${HOME_DIR}/.config/tcfs/config.toml"
FILEPROVIDER_CONFIG="${HOME_DIR}/.config/tcfs/fileprovider/config.json"
EXPECTED_REL="Projects/tcfs-odrive-parity/honey-readme.txt"
MUTATION_REL="Projects/tcfs-odrive-parity/fileprovider-mutation.txt"
CONFLICT_REL="Projects/tcfs-odrive-parity/conflict-status.txt"
EXPECTED_CONTENT_FILE="${TMPDIR}/expected-content.txt"
MUTATION_CONTENT_FILE="${TMPDIR}/mutation-content.txt"
CONFLICT_CONTENT_FILE="${TMPDIR}/conflict-content.txt"
OPEN_LOG="${TMPDIR}/open.log"
PLUGINKIT_LOG="${TMPDIR}/pluginkit.log"
LAUNCHCTL_LOG="${TMPDIR}/launchctl.log"
SWIFT_LOG="${TMPDIR}/swift.log"
HOST_BINARY_LOG="${TMPDIR}/host-binary.log"
REQUEST_MARKER="${TMPDIR}/host-request-download.marker"
EVICT_MARKER="${TMPDIR}/host-evict.marker"

mkdir -p \
  "$FAKE_BIN" \
  "$APP_PATH/Contents/MacOS" \
  "$(dirname "$CONFIG_PATH")" \
  "$(dirname "$FILEPROVIDER_CONFIG")" \
  "$(dirname "$CLOUD_ROOT/$EXPECTED_REL")" \
  "$(dirname "$CLOUD_ROOT/$CONFLICT_REL")" \
  "$(dirname "$SYNC_ROOT/$CONFLICT_REL")" \
  "$LOG_DIR"

cat >"$CONFIG_PATH" <<EOF
[storage]
endpoint = "http://example.invalid:8333"
bucket = "tcfs"

[sync]
state_db = "${TMPDIR}/tcfs-state.db"
sync_root = "${SYNC_ROOT}"
EOF
printf '{"socket_path":"/tmp/tcfs-fileprovider.sock"}\n' >"$FILEPROVIDER_CONFIG"
printf 'TCFS Finder hydration fixture\n' >"$EXPECTED_CONTENT_FILE"
printf 'TCFS Finder mutation fixture\n' >"$MUTATION_CONTENT_FILE"
printf 'TCFS Finder conflict fixture\n' >"$CONFLICT_CONTENT_FILE"
cp "$EXPECTED_CONTENT_FILE" "$CLOUD_ROOT/$EXPECTED_REL"
cp "$CONFLICT_CONTENT_FILE" "$CLOUD_ROOT/$CONFLICT_REL"
cp "$CONFLICT_CONTENT_FILE" "$SYNC_ROOT/$CONFLICT_REL"
printf '{"entries":{}}\n' >"$STATE_PATH"

cat >"$FAKE_BIN/uname" <<'EOF'
#!/usr/bin/env bash
printf 'Darwin\n'
EOF
cat >"$FAKE_BIN/tcfs" <<'EOF'
#!/usr/bin/env bash
case "${1:-}" in
  --version)
    printf 'tcfs 0.12.2\n'
    ;;
  --config)
    config="$2"
    shift 2
    case "${1:-status}" in
      status)
        printf 'tcfsd v0.12.2\nstorage: [ok]\n'
        ;;
      index)
        [[ "${2:-}" == "inspect" ]] || {
          printf 'expected fake index inspect\n' >&2
          exit 1
        }
        rel="${3:-}"
        status="${TCFS_FAKE_INDEX_STATUS:-visible}"
        if [[ "$status" == "visible" ]]; then
          cat <<JSON
{
  "rel_path": "$rel",
  "remote_prefix": "data",
  "index_key": "data/index/$rel",
  "index_exists": true,
  "status": "visible",
  "parse_error": null,
  "entry_state": "committed",
  "visible_entry": {
    "manifest_hash": "fakehash",
    "manifest_key": "data/manifests/fakehash",
    "manifest_exists": true,
    "size": 29,
    "chunks": 1,
    "kind": "regular_file",
    "symlink_target": null
  },
  "pending_entry": null
}
JSON
        else
          cat <<JSON
{
  "rel_path": "$rel",
  "remote_prefix": "data",
  "index_key": "data/index/$rel",
  "index_exists": false,
  "status": "$status",
  "parse_error": null,
  "entry_state": null,
  "visible_entry": null,
  "pending_entry": null
}
JSON
        fi
        ;;
      pull)
        rel="$2"
        dest="$3"
        [[ "${4:-}" == "--prefix" ]] || {
          printf 'expected --prefix for fake pull\n' >&2
          exit 1
        }
        [[ -n "${5:-}" ]] || {
          printf 'missing fake pull prefix\n' >&2
          exit 1
        }
        src="${TCFS_FAKE_REMOTE_ROOT:?}/$rel"
        /bin/cat "$src" >"$dest"
        printf 'Pulling %s -> %s using %s\n' "$rel" "$dest" "$config"
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
      sync-status)
        path="$2"
        printf 'State cache: %s\n' "${4:-unknown-state}"
        printf 'Tracked files: 1\n\n'
        printf 'File: %s\n' "$path"
        printf '  hash:       deadbeef\n'
        printf '  size:       29 B\n'
        printf '  chunks:     1\n'
        printf '  remote:     data/index/Projects/tcfs-odrive-parity/conflict-status.txt\n'
        printf '  last sync:  0 seconds ago\n'
        printf '  sync state: conflict\n'
        printf '  sync check: up to date\n'
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
  printf 'tcfsd 0.12.2\n'
else
  printf 'unexpected tcfsd invocation:'
  printf ' %q' "$@"
  printf '\n'
  exit 1
fi
EOF
cat >"$FAKE_BIN/pluginkit" <<'EOF'
#!/usr/bin/env bash
if [[ -n "${TCFS_FAKE_PLUGINKIT_LOG:-}" ]]; then
  printf 'pluginkit' >>"$TCFS_FAKE_PLUGINKIT_LOG"
  printf ' %q' "$@" >>"$TCFS_FAKE_PLUGINKIT_LOG"
  printf '\n' >>"$TCFS_FAKE_PLUGINKIT_LOG"
fi
if [[ "${1:-}" == "-e" && "${2:-}" == "use" ]]; then
  printf 'elected use for %s\n' "${4:-unknown}"
  exit 0
fi
printf 'com.apple.FileProvider.NonUI extension io.tinyland.tcfs.fileprovider\n'
printf '            Path = /Users/test/Applications/TCFSProvider.app/Contents/Extensions/TCFSFileProvider.appex\n'
if [[ "${TCFS_FAKE_PLUGIN_DUPES:-0}" == "1" ]]; then
  printf 'com.apple.FileProvider.NonUI extension io.tinyland.tcfs.fileprovider\n'
  printf '            Path = /Users/test/git/tummycrypt/build/TCFSProvider.app/Contents/Extensions/TCFSFileProvider.appex\n'
fi
if [[ "${TCFS_FAKE_PLUGIN_SAME_PATH_DUPES:-0}" == "1" ]]; then
  printf 'com.apple.FileProvider.NonUI extension io.tinyland.tcfs.fileprovider\n'
  printf '            Path = /Users/test/Applications/TCFSProvider.app/Contents/Extensions/TCFSFileProvider.appex\n'
fi
EOF
cat >"$FAKE_BIN/codesign" <<'EOF'
#!/usr/bin/env bash
if [[ "$*" == *"--entitlements :-"* ]]; then
  if [[ "${TCFS_FAKE_COMPACT_ENTITLEMENTS:-0}" == "1" ]]; then
    if [[ "${TCFS_FAKE_TESTING_MODE_ENTITLEMENT:-0}" == "1" ]]; then
      printf '%s\n' '<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>com.apple.developer.fileprovider.testing-mode</key><true/></dict></plist>'
    else
      printf '%s\n' '<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict></dict></plist>'
    fi
    exit 0
  fi

  cat <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
PLIST
  if [[ "${TCFS_FAKE_TESTING_MODE_ENTITLEMENT:-0}" == "1" ]]; then
    cat <<PLIST
  <key>com.apple.developer.fileprovider.testing-mode</key>
  <true/>
PLIST
  fi
  cat <<PLIST
</dict>
</plist>
PLIST
  exit 0
fi

printf 'unexpected codesign invocation:'
printf ' %q' "$@"
printf '\n'
exit 1
EOF
cat >"$FAKE_BIN/fileproviderctl" <<'EOF'
#!/usr/bin/env bash
case "${1:-}" in
  "")
    printf 'materialize <item>\n'
    printf 'evaluate <item>\n'
    printf 'check | repair\n'
    ;;
  domain)
    printf 'io.tinyland.tcfs\n'
    ;;
  materialize|evaluate|check)
    printf 'fileproviderctl %s' "$1"
    shift
    printf ' %q' "$@"
    printf '\n'
    ;;
  *)
    printf 'unexpected fileproviderctl invocation:'
    printf ' %q' "$@"
    printf '\n'
    exit 1
    ;;
esac
EOF
cat >"$FAKE_BIN/log" <<'EOF'
#!/usr/bin/env bash
args="$*"
if [[ "$args" == *"io.tinyland.tcfs.fileprovider"* && "$args" == *"loadConfig:"* ]]; then
  if [[ "${TCFS_FAKE_EXTENSION_LOG+x}" == "x" ]]; then
    if [[ -n "$TCFS_FAKE_EXTENSION_LOG" ]]; then
      printf '%s\n' "$TCFS_FAKE_EXTENSION_LOG"
    fi
  else
    printf '2026-04-30 TCFSFileProvider extension loadConfig: loaded from shared Keychain\n'
  fi
elif [[ "$args" == *"io.tinyland.tcfs.fileprovider"* && "$args" == *"hydration_state=conflict"* ]]; then
  if [[ "${TCFS_FAKE_SKIP_ENUM_CONFLICT_LOG:-0}" != "1" ]]; then
    printf '2026-04-30 TCFSFileProvider enumerator enumerateProviderItems: item=Projects/tcfs-odrive-parity/conflict-status.txt hydration_state=conflict\n'
  fi
elif [[ "$args" == *"com.apple.FileProvider"* || "$args" == *"fileproviderd"* || "$args" == *"Sync is not enabled"* ]]; then
  if [[ "${TCFS_FAKE_FILEPROVIDER_SYSTEM_LOG+x}" == "x" && -n "$TCFS_FAKE_FILEPROVIDER_SYSTEM_LOG" ]]; then
    printf '%s\n' "$TCFS_FAKE_FILEPROVIDER_SYSTEM_LOG"
  fi
else
  printf '2026-04-30 TCFSProvider host add: OK\n'
  if [[ -n "${TCFS_FAKE_REQUEST_MARKER:-}" && -s "$TCFS_FAKE_REQUEST_MARKER" ]]; then
    marker_content="$(cat "$TCFS_FAKE_REQUEST_MARKER")"
    item="${marker_content%%|*}"
    nonce="${marker_content#*|}"
    [[ "$nonce" == "$marker_content" ]] && nonce=""
    printf '2026-04-30 TCFSProvider host requestDownload: %s: OK' "$item"
    [[ -z "$nonce" ]] || printf ' nonce=%s' "$nonce"
    printf '\n'
  fi
  if [[ -n "${TCFS_FAKE_EVICT_MARKER:-}" && -s "$TCFS_FAKE_EVICT_MARKER" ]]; then
    marker_content="$(cat "$TCFS_FAKE_EVICT_MARKER")"
    item="${marker_content%%|*}"
    nonce="${marker_content#*|}"
    [[ "$nonce" == "$marker_content" ]] && nonce=""
    printf '2026-04-30 TCFSProvider host evict: %s: OK' "$item"
    [[ -z "$nonce" ]] || printf ' nonce=%s' "$nonce"
    printf '\n'
  fi
fi
EOF
cat >"$FAKE_BIN/open" <<'EOF'
#!/usr/bin/env bash
if [[ -n "${TCFS_FAKE_OPEN_HANG_TARGET:-}" && "${1:-}" == "$TCFS_FAKE_OPEN_HANG_TARGET" ]]; then
  exec perl -e 'select undef, undef, undef, 60'
fi
printf 'open' >>"$TCFS_FAKE_OPEN_LOG"
printf ' %q' "$@" >>"$TCFS_FAKE_OPEN_LOG"
printf '\n' >>"$TCFS_FAKE_OPEN_LOG"
EOF
cat >"$FAKE_BIN/ls" <<'EOF'
#!/usr/bin/env bash
if [[ -n "${TCFS_FAKE_LS_HANG_TARGET:-}" ]]; then
  for arg in "$@"; do
    if [[ "$arg" == "$TCFS_FAKE_LS_HANG_TARGET" ]]; then
      exec perl -e 'select undef, undef, undef, 60'
    fi
  done
fi
exec /bin/ls "$@"
EOF
cat >"$FAKE_BIN/find" <<'EOF'
#!/usr/bin/env bash
if [[ -n "${TCFS_FAKE_FIND_HANG_TARGET:-}" ]]; then
  for arg in "$@"; do
    if [[ "$arg" == "$TCFS_FAKE_FIND_HANG_TARGET" ]]; then
      exec perl -e 'select undef, undef, undef, 60'
    fi
  done
fi
exec /usr/bin/find "$@"
EOF
cat >"$APP_PATH/Contents/MacOS/TCFSProvider" <<'EOF'
#!/usr/bin/env bash
if [[ -n "${TCFS_FAKE_HOST_BINARY_LOG:-}" ]]; then
  printf 'host-binary' >>"$TCFS_FAKE_HOST_BINARY_LOG"
  if [[ -n "${TCFS_FILEPROVIDER_REQUEST_DOWNLOAD_IDENTIFIER:-}" ]]; then
    printf ' requestDownload=%s' "$TCFS_FILEPROVIDER_REQUEST_DOWNLOAD_IDENTIFIER" >>"$TCFS_FAKE_HOST_BINARY_LOG"
  fi
  if [[ -n "${TCFS_FILEPROVIDER_EVICT_IDENTIFIER:-}" ]]; then
    printf ' evict=%s' "$TCFS_FILEPROVIDER_EVICT_IDENTIFIER" >>"$TCFS_FAKE_HOST_BINARY_LOG"
  fi
  if [[ -n "${TCFS_FILEPROVIDER_ACTION_NONCE:-}" ]]; then
    printf ' nonce=%s' "$TCFS_FILEPROVIDER_ACTION_NONCE" >>"$TCFS_FAKE_HOST_BINARY_LOG"
  fi
  printf '\n' >>"$TCFS_FAKE_HOST_BINARY_LOG"
fi
if [[ "${TCFS_FILEPROVIDER_HOST_STDERR_LOG:-0}" == "1" ]]; then
  if [[ -n "${TCFS_FILEPROVIDER_REQUEST_DOWNLOAD_IDENTIFIER:-}" ]]; then
    printf 'requestDownload: %s: OK' "$TCFS_FILEPROVIDER_REQUEST_DOWNLOAD_IDENTIFIER"
    [[ -z "${TCFS_FILEPROVIDER_ACTION_NONCE:-}" ]] || printf ' nonce=%s' "$TCFS_FILEPROVIDER_ACTION_NONCE"
    printf '\n'
  elif [[ -n "${TCFS_FILEPROVIDER_EVICT_IDENTIFIER:-}" ]]; then
    printf 'evict: %s: OK' "$TCFS_FILEPROVIDER_EVICT_IDENTIFIER"
    [[ -z "${TCFS_FILEPROVIDER_ACTION_NONCE:-}" ]] || printf ' nonce=%s' "$TCFS_FILEPROVIDER_ACTION_NONCE"
    printf '\n'
  else
    [[ "${TCFS_FILEPROVIDER_TESTING_MODE_ALWAYS_ENABLED:-0}" != "1" ]] || printf 'testingMode: requested alwaysEnabled for FileProvider domain\n'
    printf 'add: OK - domain available\n'
  fi
fi
if [[ -n "${TCFS_FAKE_REQUEST_MARKER:-}" && -n "${TCFS_FILEPROVIDER_REQUEST_DOWNLOAD_IDENTIFIER:-}" ]]; then
  printf '%s|%s\n' "$TCFS_FILEPROVIDER_REQUEST_DOWNLOAD_IDENTIFIER" "${TCFS_FILEPROVIDER_ACTION_NONCE:-}" >"$TCFS_FAKE_REQUEST_MARKER"
fi
if [[ -n "${TCFS_FAKE_EVICT_MARKER:-}" && -n "${TCFS_FILEPROVIDER_EVICT_IDENTIFIER:-}" ]]; then
  printf '%s|%s\n' "$TCFS_FILEPROVIDER_EVICT_IDENTIFIER" "${TCFS_FILEPROVIDER_ACTION_NONCE:-}" >"$TCFS_FAKE_EVICT_MARKER"
fi
EOF
cat >"$FAKE_BIN/launchctl" <<'EOF'
#!/usr/bin/env bash
if [[ -n "${TCFS_FAKE_LAUNCHCTL_LOG:-}" ]]; then
  printf 'launchctl' >>"$TCFS_FAKE_LAUNCHCTL_LOG"
  printf ' %q' "$@" >>"$TCFS_FAKE_LAUNCHCTL_LOG"
  printf '\n' >>"$TCFS_FAKE_LAUNCHCTL_LOG"
fi
EOF
cat >"$FAKE_BIN/cat" <<'EOF'
#!/usr/bin/env bash
marker="${TCFS_FAKE_CAT_MARKER:-}"
target="${TCFS_FAKE_CAT_TARGET:-}"
if [[ -n "$marker" && -n "$target" && "${1:-}" == "$target" && ! -e "$marker" ]]; then
  : >"$marker"
  printf 'Operation timed out\n' >&2
  exit 1
fi
exec /bin/cat "$@"
EOF
cat >"$FAKE_BIN/swift" <<'EOF'
#!/usr/bin/env bash
if [[ -n "${TCFS_FAKE_SWIFT_LOG:-}" ]]; then
  printf 'swift' >>"$TCFS_FAKE_SWIFT_LOG"
  printf ' %q' "$@" >>"$TCFS_FAKE_SWIFT_LOG"
  printf '\n' >>"$TCFS_FAKE_SWIFT_LOG"
fi

helper="${1:-}"
source="${2:-}"
destination="${3:-}"
marker="${TCFS_FAKE_SWIFT_MARKER:-}"
target="${TCFS_FAKE_SWIFT_TARGET:-}"
hang_target="${TCFS_FAKE_SWIFT_HANG_TARGET:-}"

if [[ "$helper" != *"macos-fileprovider-coordinated-read.swift" || -z "$source" || -z "$destination" ]]; then
  printf 'unexpected swift invocation:'
  printf ' %q' "$@"
  printf '\n'
  exit 1
fi

if [[ -n "$hang_target" && "$source" == "$hang_target" ]]; then
  exec perl -e 'select undef, undef, undef, 60'
fi

if [[ -n "$marker" && -n "$target" && "$source" == "$target" && ! -e "$marker" ]]; then
  : >"$marker"
  printf 'coordinated read timed out\n' >&2
  exit 1
fi

/bin/cat "$source" >"$destination"
EOF
cat >"$FAKE_BIN/stat" <<'EOF'
#!/usr/bin/env bash
if [[ "${1:-}" == "-f" ]]; then
  bytes="$(wc -c <"$3" | tr -d '[:space:]')"
  printf '  size: %s bytes\n' "$bytes"
else
  /usr/bin/stat "$@"
fi
EOF
chmod +x "$FAKE_BIN"/* "$APP_PATH/Contents/MacOS/TCFSProvider"
export TCFS_FAKE_HOST_BINARY_LOG="$HOST_BINARY_LOG"
export TCFS_FAKE_REQUEST_MARKER="$REQUEST_MARKER"
export TCFS_FAKE_EVICT_MARKER="$EVICT_MARKER"

OUT="${TMPDIR}/positive.out"
READ_RETRY_MARKER="${TMPDIR}/read-failed-once"
PATH="$FAKE_BIN:$PATH" \
HOME="$HOME_DIR" \
TCFS_FAKE_OPEN_LOG="$OPEN_LOG" \
TCFS_FAKE_PLUGINKIT_LOG="$PLUGINKIT_LOG" \
TCFS_FAKE_LAUNCHCTL_LOG="$LAUNCHCTL_LOG" \
TCFS_FAKE_SWIFT_LOG="$SWIFT_LOG" \
TCFS_FAKE_TESTING_MODE_ENTITLEMENT=1 \
TCFS_FAKE_COMPACT_ENTITLEMENTS=1 \
TCFS_FAKE_SKIP_ENUM_CONFLICT_LOG=1 \
TCFS_FAKE_REMOTE_ROOT="$CLOUD_ROOT" \
TCFS_FAKE_SWIFT_TARGET="$CLOUD_ROOT/$EXPECTED_REL" \
TCFS_FAKE_SWIFT_MARKER="$READ_RETRY_MARKER" \
bash "$SCRIPT" \
  --expected-version 0.12.2 \
  --config "$CONFIG_PATH" \
  --expected-file "$EXPECTED_REL" \
  --expected-content-file "$EXPECTED_CONTENT_FILE" \
  --app-path "$APP_PATH" \
  --cloud-root "$CLOUD_ROOT" \
  --log-dir "$LOG_DIR" \
  --elect-plugin-use \
  --fileprovider-testing-mode \
  --require-keychain-config \
  --exercise-evict-rehydrate \
  --exercise-mutation \
  --mutation-file "$MUTATION_REL" \
  --mutation-content-file "$MUTATION_CONTENT_FILE" \
  --exercise-conflict-status \
  --conflict-file "$CONFLICT_REL" \
  --conflict-content-file "$CONFLICT_CONTENT_FILE" \
  --state "$STATE_PATH" \
  --sync-root "$SYNC_ROOT" \
  --remote-prefix "gha/fake-prefix" \
  --timeout 5 \
  >"$OUT" 2>&1

assert_contains "$OUT" "tcfsd version: tcfsd 0.12.2"
assert_contains "$OUT" "tcfs version: tcfs 0.12.2"
assert_contains "$OUT" "tcfsd binary: $FAKE_BIN/tcfsd"
assert_contains "$OUT" "tcfs binary: $FAKE_BIN/tcfs"
assert_contains "$OUT" "pluginkit registration:"
assert_contains "$OUT" "host app log confirmed domain add"
assert_contains "$OUT" "fileproviderctl domain listing includes io.tinyland.tcfs"
assert_contains "$OUT" "CloudStorage root: $CLOUD_ROOT"
assert_contains "$OUT" "nudging CloudStorage enumeration"
assert_contains "$OUT" "nudging expected parent enumeration: $CLOUD_ROOT/Projects"
assert_contains "$OUT" "nudging expected parent enumeration: $(dirname "$CLOUD_ROOT/$EXPECTED_REL")"
assert_contains "$OUT" "electing FileProvider plug-in for current user: io.tinyland.tcfs.fileprovider"
assert_contains "$OUT" "host app FileProvider testing-mode entitlement present"
assert_contains "$OUT" "requesting FileProvider testing mode: always enabled"
assert_contains "$OUT" "launching host app binary for domain add: $APP_PATH/Contents/MacOS/TCFSProvider"
assert_contains "$OUT" "requesting FileProvider download for expected file: $EXPECTED_REL"
assert_contains "$OUT" "launching host app binary for download request: $APP_PATH/Contents/MacOS/TCFSProvider"
assert_contains "$OUT" "host app requested FileProvider download for expected file"
assert_contains "$OUT" "remote index status for expected file: visible"
assert_contains "$OUT" "requesting FileProvider eviction for expected file: $EXPECTED_REL"
assert_contains "$OUT" "launching host app binary for eviction request: $APP_PATH/Contents/MacOS/TCFSProvider"
assert_contains "$OUT" "host app requested FileProvider eviction for expected file"
assert_contains "$OUT" "FileProvider evict/rehydrate cycle passed"
assert_contains "$OUT" "writing FileProvider mutation fixture: $MUTATION_REL"
assert_contains "$OUT" "FileProvider mutation local content matched"
assert_contains "$OUT" "remote mutation pull matched expected content"
assert_contains "$OUT" "tcfs status (post-mutation):"
assert_contains "$OUT" "CLI conflict status verified: $CONFLICT_REL"
assert_contains "$OUT" "FileProvider conflict fixture content matched"
assert_contains "$OUT" "warning: FileProvider enumerator did not log conflict hydration state for $CONFLICT_REL"
assert_contains "$OUT" "hydrated file content matched expected content file"
assert_contains "$OUT" "FileProvider extension config source: shared Keychain"
assert_contains "$OUT" "macOS post-install FileProvider smoke passed"
assert_contains "$OPEN_LOG" "$CLOUD_ROOT"
assert_contains "$OPEN_LOG" "$CLOUD_ROOT/Projects"
assert_contains "$OPEN_LOG" "$(dirname "$CLOUD_ROOT/$EXPECTED_REL")"
assert_contains "$PLUGINKIT_LOG" "pluginkit -e use -i io.tinyland.tcfs.fileprovider"
assert_contains "$LAUNCHCTL_LOG" "launchctl setenv TCFS_FILEPROVIDER_TESTING_MODE_ALWAYS_ENABLED 1"
assert_contains "$LAUNCHCTL_LOG" "launchctl unsetenv TCFS_FILEPROVIDER_TESTING_MODE_ALWAYS_ENABLED"
assert_contains "$LAUNCHCTL_LOG" "launchctl unsetenv TCFS_FILEPROVIDER_REQUEST_DOWNLOAD_IDENTIFIER"
assert_contains "$LAUNCHCTL_LOG" "launchctl unsetenv TCFS_FILEPROVIDER_EVICT_IDENTIFIER"
assert_contains "$LAUNCHCTL_LOG" "launchctl unsetenv TCFS_FILEPROVIDER_ACTION_NONCE"
assert_contains "$SWIFT_LOG" "macos-fileprovider-coordinated-read.swift"
assert_contains "$HOST_BINARY_LOG" "host-binary"
assert_contains "$HOST_BINARY_LOG" "host-binary requestDownload=$EXPECTED_REL"
assert_contains "$HOST_BINARY_LOG" "host-binary evict=$EXPECTED_REL"
assert_contains "$HOST_BINARY_LOG" "nonce=tcfs-smoke-"
assert_contains "$LOG_DIR/host-domain-launch.log" "add: OK - domain available"
assert_contains "$LOG_DIR/host-request-launch.log" "requestDownload: $CONFLICT_REL: OK"
assert_contains "$LOG_DIR/host-evict-launch.log" "evict: $EXPECTED_REL: OK"
assert_contains "$LOG_DIR/extension-config.log" "loadConfig: loaded from shared Keychain"
assert_contains "$LOG_DIR/expected-file-index.json" '"status": "visible"'
assert_contains "$LOG_DIR/conflict-sync-status.log" "sync state: conflict"
test -f "$LOG_DIR/conflict-enumerator.log"
assert_contains "$LOG_DIR/fileproviderctl-materialize-root.log" "fileproviderctl materialize"
assert_contains "$LOG_DIR/fileproviderctl-evaluate-root.log" "fileproviderctl evaluate"
assert_contains "$LOG_DIR/fileproviderctl-check-root.log" "fileproviderctl check -P -a"
assert_contains "$LOG_DIR/fileproviderctl-evaluate-expected-parent.log" "fileproviderctl evaluate"
assert_contains "$LOG_DIR/fileproviderctl-check-expected-parent.log" "fileproviderctl check -P -a"
test -f "$LOG_DIR/cloud-root-ls.log"
test -f "$LOG_DIR/cloud-root-open.log"
test -f "$LOG_DIR/cloud-root-find.log"
test -f "$LOG_DIR/expected-parent-open.log"
cmp -s "$EXPECTED_CONTENT_FILE" "$LOG_DIR/hydrated-expected-file"
cmp -s "$MUTATION_CONTENT_FILE" "$CLOUD_ROOT/$MUTATION_REL"
cmp -s "$MUTATION_CONTENT_FILE" "$LOG_DIR/mutation-remote-pull"
cmp -s "$CONFLICT_CONTENT_FILE" "$LOG_DIR/conflict-hydrated-file"
[[ -e "$READ_RETRY_MARKER" ]] || {
  printf 'expected coordinated read to fail once before hydration retry succeeded\n' >&2
  exit 1
}

DIRECT_OUT="${TMPDIR}/direct-host-launch.out"
DIRECT_HOST_BINARY_LOG="${TMPDIR}/direct-host-binary.log"
env PATH="$FAKE_BIN:$PATH" HOME="$HOME_DIR" \
  TCFS_FAKE_OPEN_LOG="$OPEN_LOG" \
  TCFS_FAKE_HOST_BINARY_LOG="$DIRECT_HOST_BINARY_LOG" \
  TCFS_FAKE_SWIFT_LOG="$SWIFT_LOG" \
  bash "$SCRIPT" \
    --expected-version 0.12.2 \
    --config "$CONFIG_PATH" \
    --expected-file "$EXPECTED_REL" \
    --expected-content-file "$EXPECTED_CONTENT_FILE" \
    --app-path "$APP_PATH" \
    --cloud-root "$CLOUD_ROOT" \
    --log-dir "${TMPDIR}/direct-host-logs" \
    --direct-host-launch \
    --timeout 5 \
    >"$DIRECT_OUT" 2>&1
assert_contains "$DIRECT_OUT" "launching host app binary for domain add: $APP_PATH/Contents/MacOS/TCFSProvider"
assert_contains "$DIRECT_OUT" "host app log confirmed domain add"
assert_contains "$DIRECT_OUT" "macOS post-install FileProvider smoke passed"
assert_contains "$DIRECT_HOST_BINARY_LOG" "host-binary"

SEED_OUT="${TMPDIR}/seed.out"
SEED_CLOUD_ROOT="${HOME_DIR}/Library/CloudStorage/TCFSSeeded"
mkdir -p "$SEED_CLOUD_ROOT"
env PATH="$FAKE_BIN:$PATH" HOME="$HOME_DIR" \
  TCFS_FAKE_OPEN_LOG="$OPEN_LOG" \
  TCFS_FAKE_HOST_BINARY_LOG="$HOST_BINARY_LOG" \
  TCFS_FAKE_SWIFT_LOG="$SWIFT_LOG" \
  TCFS_FAKE_REMOTE_ROOT="$SEED_CLOUD_ROOT" \
  bash "$SCRIPT" \
    --expected-version 0.12.2 \
    --config "$CONFIG_PATH" \
    --seed-expected-file \
    --app-path "$APP_PATH" \
    --cloud-root "$SEED_CLOUD_ROOT" \
    --log-dir "${TMPDIR}/seed-logs" \
    --timeout 5 \
    >"$SEED_OUT" 2>&1
assert_contains "$SEED_OUT" "seeding expected FileProvider fixture: finder-smoke-"
assert_contains "$SEED_OUT" "remote index status for expected file: visible"
assert_contains "$SEED_OUT" "hydrated file content matched expected content file"
assert_contains "$SEED_OUT" "macOS post-install FileProvider smoke passed"
assert_contains "${TMPDIR}/seed-logs/seed-expected-file-push.log" "Push complete"

assert_fails_contains \
  "--require-keychain-config requires --expected-file or --seed-expected-file" \
  env PATH="$FAKE_BIN:$PATH" HOME="$HOME_DIR" \
    bash "$SCRIPT" \
      --require-keychain-config

assert_fails_contains \
  "--exercise-evict-rehydrate requires --expected-file or --seed-expected-file" \
  env PATH="$FAKE_BIN:$PATH" HOME="$HOME_DIR" \
    bash "$SCRIPT" \
      --exercise-evict-rehydrate

assert_fails_contains \
  "--exercise-evict-rehydrate requires --expected-content, --expected-content-file, or --seed-expected-file" \
  env PATH="$FAKE_BIN:$PATH" HOME="$HOME_DIR" \
    bash "$SCRIPT" \
      --expected-file "$EXPECTED_REL" \
      --exercise-evict-rehydrate

assert_fails_contains \
  "--exercise-mutation requires --remote-prefix" \
  env PATH="$FAKE_BIN:$PATH" HOME="$HOME_DIR" \
    bash "$SCRIPT" \
      --exercise-mutation

assert_fails_contains \
  "--exercise-conflict-status requires --conflict-file" \
  env PATH="$FAKE_BIN:$PATH" HOME="$HOME_DIR" \
    bash "$SCRIPT" \
      --exercise-conflict-status

assert_fails_contains \
  "--exercise-conflict-status requires --conflict-content or --conflict-content-file" \
  env PATH="$FAKE_BIN:$PATH" HOME="$HOME_DIR" \
    bash "$SCRIPT" \
      --exercise-conflict-status \
      --conflict-file "$CONFLICT_REL"

assert_fails_contains \
  "host app missing com.apple.developer.fileprovider.testing-mode entitlement" \
  env PATH="$FAKE_BIN:$PATH" HOME="$HOME_DIR" TCFS_FAKE_OPEN_LOG="$OPEN_LOG" TCFS_FAKE_LAUNCHCTL_LOG="$LAUNCHCTL_LOG" \
    bash "$SCRIPT" \
      --expected-version 0.12.2 \
      --config "$CONFIG_PATH" \
      --expected-file "$EXPECTED_REL" \
      --expected-content-file "$EXPECTED_CONTENT_FILE" \
      --app-path "$APP_PATH" \
      --cloud-root "$CLOUD_ROOT" \
      --log-dir "${TMPDIR}/missing-testing-mode-entitlement-logs" \
      --fileprovider-testing-mode \
      --timeout 2

assert_fails_contains \
  "FileProvider extension used build-time embedded config" \
  env PATH="$FAKE_BIN:$PATH" HOME="$HOME_DIR" TCFS_FAKE_OPEN_LOG="$OPEN_LOG" \
    TCFS_FAKE_EXTENSION_LOG="2026-04-30 TCFSFileProvider extension loadConfig: loaded from build-time embedded config" \
    bash "$SCRIPT" \
      --expected-version 0.12.2 \
      --config "$CONFIG_PATH" \
      --expected-file "$EXPECTED_REL" \
      --expected-content-file "$EXPECTED_CONTENT_FILE" \
      --app-path "$APP_PATH" \
      --cloud-root "$CLOUD_ROOT" \
      --log-dir "${TMPDIR}/embedded-config-logs" \
      --require-keychain-config \
      --timeout 2

assert_fails_contains \
  "FileProvider extension did not log shared Keychain config load" \
  env PATH="$FAKE_BIN:$PATH" HOME="$HOME_DIR" TCFS_FAKE_OPEN_LOG="$OPEN_LOG" \
    TCFS_FAKE_EXTENSION_LOG="" \
    bash "$SCRIPT" \
      --expected-version 0.12.2 \
      --config "$CONFIG_PATH" \
      --expected-file "$EXPECTED_REL" \
      --expected-content-file "$EXPECTED_CONTENT_FILE" \
      --app-path "$APP_PATH" \
      --cloud-root "$CLOUD_ROOT" \
      --log-dir "${TMPDIR}/missing-keychain-log-logs" \
      --require-keychain-config \
      --timeout 2

assert_fails_contains \
  "expected file is not backed by a visible remote index entry" \
  env PATH="$FAKE_BIN:$PATH" HOME="$HOME_DIR" TCFS_FAKE_OPEN_LOG="$OPEN_LOG" \
    TCFS_FAKE_INDEX_STATUS="missing_index" \
    bash "$SCRIPT" \
      --expected-version 0.12.2 \
      --config "$CONFIG_PATH" \
      --expected-file "$EXPECTED_REL" \
      --expected-content-file "$EXPECTED_CONTENT_FILE" \
      --app-path "$APP_PATH" \
      --cloud-root "$CLOUD_ROOT" \
      --log-dir "${TMPDIR}/missing-index-logs" \
      --timeout 2
assert_contains "${TMPDIR}/missing-index-logs/expected-file-index.json" '"status": "missing_index"'

printf 'wrong content\n' >"${TMPDIR}/wrong-content.txt"
assert_fails_contains \
  "hydrated file content mismatch" \
  env PATH="$FAKE_BIN:$PATH" HOME="$HOME_DIR" TCFS_FAKE_OPEN_LOG="$OPEN_LOG" \
    bash "$SCRIPT" \
      --expected-version 0.12.2 \
      --config "$CONFIG_PATH" \
      --expected-file "$EXPECTED_REL" \
      --expected-content-file "${TMPDIR}/wrong-content.txt" \
      --app-path "$APP_PATH" \
      --cloud-root "$CLOUD_ROOT" \
      --log-dir "${TMPDIR}/mismatch-logs" \
      --timeout 2

assert_fails_contains \
  "coordinated FileProvider read timed out after 1s" \
  env PATH="$FAKE_BIN:$PATH" HOME="$HOME_DIR" TCFS_FAKE_OPEN_LOG="$OPEN_LOG" \
    TCFS_FAKE_SWIFT_HANG_TARGET="$CLOUD_ROOT/$EXPECTED_REL" \
    TCFS_FILEPROVIDER_READ_TIMEOUT_SECS=1 \
    bash "$SCRIPT" \
      --expected-version 0.12.2 \
      --config "$CONFIG_PATH" \
      --expected-file "$EXPECTED_REL" \
      --expected-content-file "$EXPECTED_CONTENT_FILE" \
      --app-path "$APP_PATH" \
      --cloud-root "$CLOUD_ROOT" \
      --log-dir "${TMPDIR}/hung-read-logs" \
      --timeout 2

BOUNDED_LS_OUT="${TMPDIR}/bounded-cloud-root-ls.out"
env PATH="$FAKE_BIN:$PATH" HOME="$HOME_DIR" \
  TCFS_FAKE_OPEN_LOG="$OPEN_LOG" \
  TCFS_FAKE_HOST_BINARY_LOG="$HOST_BINARY_LOG" \
  TCFS_FAKE_SWIFT_LOG="$SWIFT_LOG" \
  TCFS_FAKE_REMOTE_ROOT="$CLOUD_ROOT" \
  TCFS_FAKE_LS_HANG_TARGET="$CLOUD_ROOT" \
  LOG_SHOW_TIMEOUT_SECS=1 \
  bash "$SCRIPT" \
    --expected-version 0.12.2 \
    --config "$CONFIG_PATH" \
    --expected-file "$EXPECTED_REL" \
    --expected-content-file "$EXPECTED_CONTENT_FILE" \
    --app-path "$APP_PATH" \
    --cloud-root "$CLOUD_ROOT" \
    --log-dir "${TMPDIR}/bounded-cloud-root-ls-logs" \
    --timeout 3 \
    >"$BOUNDED_LS_OUT" 2>&1
assert_contains "$BOUNDED_LS_OUT" "cloud-root-ls timed out after 1s"
assert_contains "$BOUNDED_LS_OUT" "macOS post-install FileProvider smoke passed"
test -f "${TMPDIR}/bounded-cloud-root-ls-logs/cloud-root-ls.log"

assert_fails_contains \
  "cloud-root-find timed out after 1s" \
  env PATH="$FAKE_BIN:$PATH" HOME="$HOME_DIR" \
    TCFS_FAKE_OPEN_LOG="$OPEN_LOG" \
    TCFS_FAKE_FIND_HANG_TARGET="$CLOUD_ROOT" \
    LOG_SHOW_TIMEOUT_SECS=1 \
    bash "$SCRIPT" \
      --expected-version 0.12.2 \
      --config "$CONFIG_PATH" \
      --app-path "$APP_PATH" \
      --cloud-root "$CLOUD_ROOT" \
      --log-dir "${TMPDIR}/bounded-cloud-root-find-logs" \
      --timeout 2
test -f "${TMPDIR}/bounded-cloud-root-find-logs/cloud-root-find.log"

SAME_PATH_DUPES_OUT="${TMPDIR}/same-path-dupes.out"
SAME_PATH_DUPES_ERR="${TMPDIR}/same-path-dupes.err"
if env PATH="$FAKE_BIN:$PATH" HOME="$HOME_DIR" TCFS_FAKE_OPEN_LOG="$OPEN_LOG" TCFS_FAKE_PLUGIN_SAME_PATH_DUPES=1 \
  bash "$SCRIPT" \
    --expected-version 0.12.2 \
    --config "$CONFIG_PATH" \
    --expected-file "$EXPECTED_REL" \
    --expected-content-file "$EXPECTED_CONTENT_FILE" \
    --app-path "$APP_PATH" \
    --cloud-root "$CLOUD_ROOT" \
    --log-dir "${TMPDIR}/same-path-dupe-logs" \
    --timeout 2 \
    >"$SAME_PATH_DUPES_OUT" \
    2>"$SAME_PATH_DUPES_ERR"; then
  printf 'expected same-path duplicate pluginkit registrations to fail\n' >&2
  exit 1
fi
cat "$SAME_PATH_DUPES_OUT" "$SAME_PATH_DUPES_ERR" >"${TMPDIR}/same-path-dupes.combined"
assert_contains "${TMPDIR}/same-path-dupes.combined" "multiple FileProvider registrations found for one path"
assert_contains "${TMPDIR}/same-path-dupes.combined" "registered FileProvider extension paths:"

DUPES_OUT="${TMPDIR}/dupes.out"
DUPES_ERR="${TMPDIR}/dupes.err"
if env PATH="$FAKE_BIN:$PATH" HOME="$HOME_DIR" TCFS_FAKE_OPEN_LOG="$OPEN_LOG" TCFS_FAKE_PLUGIN_DUPES=1 \
  bash "$SCRIPT" \
    --expected-version 0.12.2 \
    --config "$CONFIG_PATH" \
    --expected-file "$EXPECTED_REL" \
    --expected-content-file "$EXPECTED_CONTENT_FILE" \
    --app-path "$APP_PATH" \
    --cloud-root "$CLOUD_ROOT" \
    --log-dir "${TMPDIR}/dupe-logs" \
    --timeout 2 \
    >"$DUPES_OUT" \
    2>"$DUPES_ERR"; then
  printf 'expected duplicate pluginkit registrations to fail\n' >&2
  exit 1
fi
cat "$DUPES_OUT" "$DUPES_ERR" >"${TMPDIR}/dupes.combined"
assert_contains "${TMPDIR}/dupes.combined" "multiple FileProvider extension paths found"
assert_contains "${TMPDIR}/dupes.combined" "registered FileProvider extension paths:"
assert_contains "${TMPDIR}/dupes.combined" "/Users/test/git/tummycrypt/build/TCFSProvider.app"
assert_contains "${TMPDIR}/dupes.combined" "cleanup is not performed automatically"

DISABLED_ROOT="${HOME_DIR}/Library/CloudStorage/TCFSDisabled"
mkdir -p "$DISABLED_ROOT"
assert_fails_contains \
  "FileProvider domain is disabled by macOS (NSFileProviderErrorDomain -2011)" \
  env PATH="$FAKE_BIN:$PATH" HOME="$HOME_DIR" TCFS_FAKE_OPEN_LOG="$OPEN_LOG" \
    TCFS_FAKE_FILEPROVIDER_SYSTEM_LOG='2026-05-01 fileproviderd FP -2011 "Sync is not enabled for TCFSProvider."' \
    bash "$SCRIPT" \
      --expected-version 0.12.2 \
      --config "$CONFIG_PATH" \
      --app-path "$APP_PATH" \
      --cloud-root "$DISABLED_ROOT" \
      --log-dir "${TMPDIR}/domain-disabled-logs" \
      --timeout 2

assert_contains \
  "${TMPDIR}/domain-disabled-logs/fileprovider-system.log" \
  'Sync is not enabled for TCFSProvider'

printf 'macOS post-install smoke tests passed\n'
