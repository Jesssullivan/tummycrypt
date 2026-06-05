#!/usr/bin/env bash
# Regression tests for macos-tcfs-rollout-readiness.sh using fake platform tools.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$REPO_ROOT/scripts/macos-tcfs-rollout-readiness.sh"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-rollout-readiness-test.XXXXXX")"
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

assert_not_exists() {
  local path="$1"
  if [[ -e "$path" ]]; then
    printf 'expected path not to exist: %s\n' "$path" >&2
    exit 1
  fi
}

assert_fails_contains() {
  local expected="$1"
  shift

  local out="$TMPDIR/failure.out"
  local err="$TMPDIR/failure.err"

  if "$@" >"$out" 2>"$err"; then
    printf 'expected command to fail: %s\n' "$*" >&2
    exit 1
  fi

  cat "$out" "$err" >"$TMPDIR/failure.combined"
  assert_contains "$TMPDIR/failure.combined" "$expected"
}

FAKE_BIN="$TMPDIR/fake-bin"
FAKE_HOME="$TMPDIR/home"
LAB_ROOT="$TMPDIR/lab"
APP_PATH="$TMPDIR/TCFSProvider.app"
LAUNCH_AGENT="$TMPDIR/dev.tinyland.tcfsd.plist"
WRAPPER_SENTINEL="$TMPDIR/wrapper-ran"

mkdir -p \
  "$FAKE_BIN" \
  "$FAKE_HOME" \
  "$LAB_ROOT" \
  "$APP_PATH/Contents/Extensions/TCFSFileProvider.appex/Contents"

cat >"$LAB_ROOT/flake.lock" <<'EOF'
{
  "nodes": {
    "tummycrypt": {
      "locked": {
        "rev": "f22f36ca7307e1db32f2ed4b7b0e69e3b7cea04e"
      }
    }
  }
}
EOF

cat >"$LAUNCH_AGENT" <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>dev.tinyland.tcfsd</string>
  <key>ProgramArguments</key>
  <array>
    <string>/bin/sh</string>
    <string>-c</string>
    <string>/bin/wait4path /nix/store && exec /tmp/fake-tcfsd-daemon-darwin</string>
  </array>
</dict>
</plist>
EOF

cat >"$TMPDIR/fake-tcfsd-daemon-darwin" <<EOF
#!/usr/bin/env bash
touch "$WRAPPER_SENTINEL"
exit 99
EOF
chmod +x "$TMPDIR/fake-tcfsd-daemon-darwin"

cat >"$FAKE_BIN/uname" <<'EOF'
#!/usr/bin/env bash
printf 'Darwin\n'
EOF
cat >"$FAKE_BIN/tcfs" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
case "${1:-}" in
  --version)
    printf 'tcfs %s\n' "${FAKE_TCFS_VERSION:-0.12.14}"
    ;;
  status)
    cat <<'OUT'
tcfsd v0.12.14
  storage:       http://seaweedfs-tcfs:8333 [ok]
OUT
    ;;
  *)
    exit 2
    ;;
esac
EOF
cat >"$FAKE_BIN/tcfsd" <<'EOF'
#!/usr/bin/env bash
if [[ "${1:-}" == "--version" ]]; then
  printf 'tcfsd %s\n' "${FAKE_TCFSD_VERSION:-0.12.14}"
else
  exit 2
fi
EOF
cat >"$FAKE_BIN/launchctl" <<'EOF'
#!/usr/bin/env bash
printf 'service = dev.tinyland.tcfsd\n'
EOF
cat >"$FAKE_BIN/pluginkit" <<'EOF'
#!/usr/bin/env bash
printf '+    io.tinyland.tcfs.fileprovider(0.2.0)\n'
EOF
cat >"$FAKE_BIN/spctl" <<'EOF'
#!/usr/bin/env bash
printf '%s: accepted\n' "${*: -1}"
EOF
cat >"$FAKE_BIN/PlistBuddy" <<EOF
#!/usr/bin/env bash
case "\$*" in
  *"ProgramArguments:2"*)
    printf '/bin/wait4path /nix/store && exec $TMPDIR/fake-tcfsd-daemon-darwin\n'
    ;;
  *"CFBundleShortVersionString"*"TCFSFileProvider.appex"*)
    printf '0.2.0\n'
    ;;
  *"CFBundleShortVersionString"*)
    printf '0.12.14\n'
    ;;
  *)
    exit 1
    ;;
esac
EOF
chmod +x "$FAKE_BIN"/*

COMMON_ENV=(
  HOME="$FAKE_HOME"
  PATH="$FAKE_BIN:$PATH"
  TCFS_LAB_ROOT="$LAB_ROOT"
  TCFS_APP_PATH="$APP_PATH"
  TCFS_LAUNCH_AGENT="$LAUNCH_AGENT"
  TCFS_PLISTBUDDY_BIN="$FAKE_BIN/PlistBuddy"
  TCFS_STATUS_TIMEOUT=2
)

PASS_OUT="$TMPDIR/pass.out"
env "${COMMON_ENV[@]}" \
  "$SCRIPT" \
  --expected-version 0.12.14 \
  --expected-tummycrypt-rev f22f36ca7307e1db32f2ed4b7b0e69e3b7cea04e \
  >"$PASS_OUT" 2>&1

assert_contains "$PASS_OUT" "lab tummycrypt rev: f22f36ca7307e1db32f2ed4b7b0e69e3b7cea04e"
assert_contains "$PASS_OUT" "tcfs version: tcfs 0.12.14"
assert_contains "$PASS_OUT" "launch agent command inspection: not executed"
assert_contains "$PASS_OUT" "tcfs status: returned within 2s"
assert_contains "$PASS_OUT" "rollout-readiness=pass"
assert_not_exists "$WRAPPER_SENTINEL"

assert_fails_contains \
  "tcfs version mismatch: expected output containing '0.12.14'" \
  env "${COMMON_ENV[@]}" FAKE_TCFS_VERSION=0.12.12 "$SCRIPT" --expected-version 0.12.14

assert_contains "$TMPDIR/failure.combined" "rollout-readiness=fail"
assert_not_exists "$WRAPPER_SENTINEL"

printf 'macOS TCFS rollout readiness tests passed\n'
