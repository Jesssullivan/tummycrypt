#!/usr/bin/env bash
#
# Regression tests for the non-production PZM FileProvider lab Gatekeeper
# override helper.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="${REPO_ROOT}/scripts/macos-fileprovider-lab-gatekeeper-override.sh"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-lab-gatekeeper-test.XXXXXX")"
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

bash -n "$SCRIPT"

FAKE_BIN="${TMPDIR}/fake-bin"
mkdir -p "$FAKE_BIN"

cat >"$FAKE_BIN/spctl" <<'EOF'
#!/usr/bin/env bash
printf 'spctl' >>"$TCFS_FAKE_POLICY_LOG"
printf ' %q' "$@" >>"$TCFS_FAKE_POLICY_LOG"
printf '\n' >>"$TCFS_FAKE_POLICY_LOG"

case " $* " in
  *" --assess "*)
    printf 'assessment rejected for lab test\n'
    exit 3
    ;;
  *)
    exit 0
    ;;
esac
EOF
chmod +x "$FAKE_BIN/spctl"

cat >"$FAKE_BIN/syspolicy_check" <<'EOF'
#!/usr/bin/env bash
printf 'syspolicy_check' >>"$TCFS_FAKE_POLICY_LOG"
printf ' %q' "$@" >>"$TCFS_FAKE_POLICY_LOG"
printf '\n' >>"$TCFS_FAKE_POLICY_LOG"
printf 'synthetic policy output\n'
EOF
chmod +x "$FAKE_BIN/syspolicy_check"

cat >"$FAKE_BIN/sudo" <<'EOF'
#!/usr/bin/env bash
if [[ "${1:-}" == "-n" ]]; then
  exit 1
fi

args=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    -S)
      shift
      ;;
    -p)
      shift 2
      ;;
    *)
      args+=("$1")
      shift
      ;;
  esac
done

printf 'sudo' >>"$TCFS_FAKE_POLICY_LOG"
printf ' %q' "${args[@]}" >>"$TCFS_FAKE_POLICY_LOG"
printf '\n' >>"$TCFS_FAKE_POLICY_LOG"
"${args[@]}"
EOF
chmod +x "$FAKE_BIN/sudo"

APP="${TMPDIR}/Applications/TCFSProvider.app"
EXT="${APP}/Contents/Extensions/TCFSFileProvider.appex"
LOG_DIR="${TMPDIR}/logs"
POLICY_LOG="${TMPDIR}/policy.log"
mkdir -p "$EXT"

PATH="$FAKE_BIN:$PATH" \
TCFS_FAKE_POLICY_LOG="$POLICY_LOG" \
TCFS_RUNNER_SUDO_PASSWORD="fake-password" \
bash "$SCRIPT" apply \
  --app-path "$APP" \
  --extension-path "$EXT" \
  --log-dir "$LOG_DIR"

assert_contains "$LOG_DIR/summary.txt" "mode=apply"
assert_contains "$LOG_DIR/summary.txt" "label=TCFSFileProviderLab"
assert_contains "$LOG_DIR/host-app-before-spctl-execute.txt" "exit=3"
assert_contains "$LOG_DIR/fileprovider-extension-after-spctl-execute.txt" "exit=3"
assert_contains "$POLICY_LOG" "spctl --add --label TCFSFileProviderLab $APP"
assert_contains "$POLICY_LOG" "spctl --add --label TCFSFileProviderLab $EXT"
assert_contains "$POLICY_LOG" "spctl --enable --label TCFSFileProviderLab"
assert_contains "$POLICY_LOG" "syspolicy_check distribution $APP"

PATH="$FAKE_BIN:$PATH" \
TCFS_FAKE_POLICY_LOG="$POLICY_LOG" \
TCFS_RUNNER_SUDO_PASSWORD="fake-password" \
bash "$SCRIPT" cleanup \
  --label TCFSFileProviderLab \
  --log-dir "${TMPDIR}/cleanup"

assert_contains "${TMPDIR}/cleanup/cleanup-summary.txt" "mode=cleanup"
assert_contains "$POLICY_LOG" "spctl --remove --label TCFSFileProviderLab"

assert_fails_contains \
  "host-app not found" \
  env PATH="$FAKE_BIN:$PATH" \
    TCFS_FAKE_POLICY_LOG="$POLICY_LOG" \
    TCFS_RUNNER_SUDO_PASSWORD="fake-password" \
    bash "$SCRIPT" apply \
      --app-path "${TMPDIR}/missing.app" \
      --extension-path "$EXT" \
      --log-dir "${TMPDIR}/missing-logs"

echo "macOS FileProvider lab Gatekeeper override tests passed"
