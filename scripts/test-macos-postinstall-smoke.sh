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
LOG_DIR="${TMPDIR}/logs"
CONFIG_PATH="${HOME_DIR}/.config/tcfs/config.toml"
FILEPROVIDER_CONFIG="${HOME_DIR}/.config/tcfs/fileprovider/config.json"
EXPECTED_REL="Projects/tcfs-odrive-parity/honey-readme.txt"
EXPECTED_CONTENT_FILE="${TMPDIR}/expected-content.txt"
OPEN_LOG="${TMPDIR}/open.log"

mkdir -p \
  "$FAKE_BIN" \
  "$APP_PATH" \
  "$(dirname "$CONFIG_PATH")" \
  "$(dirname "$FILEPROVIDER_CONFIG")" \
  "$(dirname "$CLOUD_ROOT/$EXPECTED_REL")" \
  "$LOG_DIR"

printf '[storage]\nendpoint = "http://example.invalid:8333"\nbucket = "tcfs"\n' >"$CONFIG_PATH"
printf '{"socket_path":"/tmp/tcfs-fileprovider.sock"}\n' >"$FILEPROVIDER_CONFIG"
printf 'TCFS Finder hydration fixture\n' >"$EXPECTED_CONTENT_FILE"
cp "$EXPECTED_CONTENT_FILE" "$CLOUD_ROOT/$EXPECTED_REL"

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
    printf 'tcfsd v0.12.2\nstorage: [ok]\n'
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
if [[ "$args" == *"io.tinyland.tcfs.fileprovider"* ]]; then
  if [[ -v TCFS_FAKE_EXTENSION_LOG ]]; then
    if [[ -n "$TCFS_FAKE_EXTENSION_LOG" ]]; then
      printf '%s\n' "$TCFS_FAKE_EXTENSION_LOG"
    fi
  else
    printf '2026-04-30 TCFSFileProvider extension loadConfig: loaded from shared Keychain\n'
  fi
else
  printf '2026-04-30 TCFSProvider host add: OK\n'
fi
EOF
cat >"$FAKE_BIN/open" <<'EOF'
#!/usr/bin/env bash
printf 'open' >>"$TCFS_FAKE_OPEN_LOG"
printf ' %q' "$@" >>"$TCFS_FAKE_OPEN_LOG"
printf '\n' >>"$TCFS_FAKE_OPEN_LOG"
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
cat >"$FAKE_BIN/stat" <<'EOF'
#!/usr/bin/env bash
if [[ "${1:-}" == "-f" ]]; then
  bytes="$(wc -c <"$3" | tr -d '[:space:]')"
  printf '  size: %s bytes\n' "$bytes"
else
  /usr/bin/stat "$@"
fi
EOF
chmod +x "$FAKE_BIN"/*

OUT="${TMPDIR}/positive.out"
CAT_RETRY_MARKER="${TMPDIR}/cat-failed-once"
PATH="$FAKE_BIN:$PATH" \
HOME="$HOME_DIR" \
TCFS_FAKE_OPEN_LOG="$OPEN_LOG" \
TCFS_FAKE_CAT_TARGET="$CLOUD_ROOT/$EXPECTED_REL" \
TCFS_FAKE_CAT_MARKER="$CAT_RETRY_MARKER" \
bash "$SCRIPT" \
  --expected-version 0.12.2 \
  --config "$CONFIG_PATH" \
  --expected-file "$EXPECTED_REL" \
  --expected-content-file "$EXPECTED_CONTENT_FILE" \
  --app-path "$APP_PATH" \
  --cloud-root "$CLOUD_ROOT" \
  --log-dir "$LOG_DIR" \
  --require-keychain-config \
  --timeout 2 \
  >"$OUT"

assert_contains "$OUT" "tcfsd version: tcfsd 0.12.2"
assert_contains "$OUT" "tcfs version: tcfs 0.12.2"
assert_contains "$OUT" "tcfsd binary: $FAKE_BIN/tcfsd"
assert_contains "$OUT" "tcfs binary: $FAKE_BIN/tcfs"
assert_contains "$OUT" "pluginkit registration:"
assert_contains "$OUT" "host app log confirmed domain re-add"
assert_contains "$OUT" "fileproviderctl domain listing includes io.tinyland.tcfs"
assert_contains "$OUT" "CloudStorage root: $CLOUD_ROOT"
assert_contains "$OUT" "nudging CloudStorage enumeration"
assert_contains "$OUT" "hydrated file content matched expected content file"
assert_contains "$OUT" "FileProvider extension config source: shared Keychain"
assert_contains "$OUT" "macOS post-install FileProvider smoke passed"
assert_contains "$OPEN_LOG" "$APP_PATH"
assert_contains "$OPEN_LOG" "$CLOUD_ROOT"
assert_contains "$LOG_DIR/extension-config.log" "loadConfig: loaded from shared Keychain"
assert_contains "$LOG_DIR/fileproviderctl-materialize-root.log" "fileproviderctl materialize"
assert_contains "$LOG_DIR/fileproviderctl-evaluate-root.log" "fileproviderctl evaluate"
assert_contains "$LOG_DIR/fileproviderctl-check-root.log" "fileproviderctl check -P -a"
cmp -s "$EXPECTED_CONTENT_FILE" "$LOG_DIR/hydrated-expected-file"
[[ -e "$CAT_RETRY_MARKER" ]] || {
  printf 'expected fake cat to fail once before hydration retry succeeded\n' >&2
  exit 1
}

assert_fails_contains \
  "--require-keychain-config requires --expected-file" \
  env PATH="$FAKE_BIN:$PATH" HOME="$HOME_DIR" \
    bash "$SCRIPT" \
      --require-keychain-config

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

SAME_PATH_DUPES_OUT="${TMPDIR}/same-path-dupes.out"
SAME_PATH_DUPES_ERR="${TMPDIR}/same-path-dupes.err"
env PATH="$FAKE_BIN:$PATH" HOME="$HOME_DIR" TCFS_FAKE_OPEN_LOG="$OPEN_LOG" TCFS_FAKE_PLUGIN_SAME_PATH_DUPES=1 \
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
    2>"$SAME_PATH_DUPES_ERR"
assert_contains "$SAME_PATH_DUPES_OUT" "macOS post-install FileProvider smoke passed"
assert_contains "$SAME_PATH_DUPES_ERR" "warning: pluginkit shows 2 records for one FileProvider path"

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

printf 'macOS post-install smoke tests passed\n'
