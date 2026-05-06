#!/usr/bin/env bash
#
# Regression tests for the non-production PZM FileProvider lab Gatekeeper
# SystemPolicyRule profile helper.
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

cat >"$FAKE_BIN/codesign" <<'EOF'
#!/usr/bin/env bash
path="${*: -1}"
case "$path" in
  *TCFSProvider.app)
    printf 'designated => identifier "io.tinyland.tcfs" and anchor apple generic and certificate leaf[subject.OU] = "HR66U669JW"\n' >&2
    ;;
  *TCFSFileProvider.appex)
    printf 'designated => identifier "io.tinyland.tcfs.fileprovider" and anchor apple generic and certificate leaf[subject.OU] = "HR66U669JW"\n' >&2
    ;;
  *)
    printf 'unexpected codesign path: %s\n' "$path" >&2
    exit 1
    ;;
esac
EOF
chmod +x "$FAKE_BIN/codesign"

cat >"$FAKE_BIN/profiles" <<'EOF'
#!/usr/bin/env bash
printf 'profiles' >>"$TCFS_FAKE_POLICY_LOG"
printf ' %q' "$@" >>"$TCFS_FAKE_POLICY_LOG"
printf '\n' >>"$TCFS_FAKE_POLICY_LOG"

case "${TCFS_FAKE_PROFILE_INSTALLED:-0}" in
  1)
    printf 'PayloadIdentifier: io.tinyland.tcfs.fileprovider.lab.system-policy\n'
    printf 'PayloadType: com.apple.systempolicy.rule\n'
    ;;
  *)
    printf 'There are no configuration profiles installed.\n'
    ;;
esac
EOF
chmod +x "$FAKE_BIN/profiles"

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
PROFILE="${TMPDIR}/tcfs-lab.mobileconfig"
POLICY_LOG="${TMPDIR}/policy.log"
mkdir -p "$EXT"

PATH="$FAKE_BIN:$PATH" \
TCFS_FAKE_POLICY_LOG="$POLICY_LOG" \
TCFS_FAKE_PROFILE_INSTALLED=1 \
TCFS_RUNNER_SUDO_PASSWORD="fake-password" \
bash "$SCRIPT" apply \
  --app-path "$APP" \
  --extension-path "$EXT" \
  --profile-output "$PROFILE" \
  --log-dir "$LOG_DIR"

assert_contains "$LOG_DIR/summary.txt" "mode=generate"
assert_contains "$LOG_DIR/verify-summary.txt" "installed=true"
assert_contains "$PROFILE" "com.apple.systempolicy.rule"
assert_contains "$PROFILE" "io.tinyland.tcfs"
assert_contains "$PROFILE" "io.tinyland.tcfs.fileprovider"
assert_contains "$LOG_DIR/host-app-designated-requirement.txt" "identifier \"io.tinyland.tcfs\""
assert_contains "$LOG_DIR/fileprovider-extension-designated-requirement.txt" "identifier \"io.tinyland.tcfs.fileprovider\""
assert_contains "$POLICY_LOG" "profiles show -type configuration -all"

MISSING_PROFILE_LOG_DIR="${TMPDIR}/missing-profile-logs"
MISSING_PROFILE="${TMPDIR}/missing-profile.mobileconfig"
assert_fails_contains \
  "TCFS lab SystemPolicyRule configuration profile is not installed" \
  env PATH="$FAKE_BIN:$PATH" \
    TCFS_FAKE_POLICY_LOG="$POLICY_LOG" \
    TCFS_FAKE_PROFILE_INSTALLED=0 \
    TCFS_RUNNER_SUDO_PASSWORD="fake-password" \
    bash "$SCRIPT" apply \
      --app-path "$APP" \
      --extension-path "$EXT" \
      --profile-output "$MISSING_PROFILE" \
      --log-dir "$MISSING_PROFILE_LOG_DIR"
assert_contains "$MISSING_PROFILE_LOG_DIR/verify-summary.txt" "installed=false"
assert_contains "$MISSING_PROFILE" "com.apple.systempolicy.rule"

bash "$SCRIPT" cleanup --log-dir "${TMPDIR}/cleanup"
assert_contains "${TMPDIR}/cleanup/cleanup-summary.txt" "mode=cleanup"
assert_contains "${TMPDIR}/cleanup/cleanup-summary.txt" "manual or MDM-managed"

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
