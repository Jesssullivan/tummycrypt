#!/usr/bin/env bash
#
# Regression tests for lazy-hydration-desktop-honey-demo.sh.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="${REPO_ROOT}/scripts/lazy-hydration-desktop-honey-demo.sh"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-desktop-honey-test.XXXXXX")"
trap 'rm -rf "${TMPDIR}"' EXIT

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

HOME_OK="${TMPDIR}/home-ok"
EVIDENCE="${TMPDIR}/evidence"
OUT="${TMPDIR}/positive.out"
mkdir -p "$HOME_OK"

HOME="$HOME_OK" bash "$SCRIPT" \
  --remote seaweedfs://example.invalid/tcfs/desktop-demo-test \
  --desktop-root "$HOME_OK/Desktop/TCFS Demo" \
  --evidence-dir "$EVIDENCE" \
  --honey-host honey-test \
  --honey-tcfs-bin /tmp/tcfs-current \
  >"$OUT"

assert_contains "$OUT" "plan-only: fixture created but not pushed"
assert_contains "$OUT" "desktop fixture root:"
assert_contains "$EVIDENCE/run-metadata.env" "honey_host=honey-test"
assert_contains "$EVIDENCE/run-metadata.env" "honey_tcfs_bin=/tmp/tcfs-current"
assert_contains "$EVIDENCE/run-metadata.env" "allow_honey_real_desktop=0"
assert_contains "$EVIDENCE/honey-commands.txt" "ssh honey-test"
assert_contains "$EVIDENCE/honey-run.sh" "TCFS_HONEY_START_MOUNT"
assert_contains "$EVIDENCE/honey-run.sh" "TCFS_BIN=/tmp/tcfs-current"
assert_contains "$EVIDENCE/honey-run.sh" "MOUNT_ROOT_RAW='~/tcfs-demo/Desktop'"
# shellcheck disable=SC2016
expected_mount_line='MOUNT_ROOT="${HOME}/${MOUNT_ROOT_RAW#\~/}"'
assert_contains "$EVIDENCE/honey-run.sh" "$expected_mount_line"
assert_contains "$EVIDENCE/expected-content.txt" "TCFS Desktop honey fixture"
test -f "$HOME_OK/Desktop/TCFS Demo/Projects/tcfs-odrive-parity/honey-readme.txt"
test -f "$EVIDENCE/tcfs.toml"
test -f "$EVIDENCE/local-tree.txt"

FAKE_SMOKE="${TMPDIR}/fake-mounted-smoke.sh"
MOUNT_ROOT_OUT="${TMPDIR}/mount-root.out"
cat >"$FAKE_SMOKE" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
while [[ $# -gt 0 ]]; do
  case "$1" in
    --mount-root)
      printf '%s\n' "$2" >"$TCFS_FAKE_MOUNT_ROOT_OUT"
      shift 2
      ;;
    --expected-file|--expected-content-file|--expect-entry|--max-depth)
      shift 2
      ;;
    *)
      shift
      ;;
  esac
done
EOF
chmod +x "$FAKE_SMOKE"
HOME="$HOME_OK" \
TCFS_HONEY_SMOKE_SCRIPT="$FAKE_SMOKE" \
TCFS_HONEY_EXPECTED_CONTENT_FILE="${TMPDIR}/expected-content-from-honey-run.txt" \
TCFS_FAKE_MOUNT_ROOT_OUT="$MOUNT_ROOT_OUT" \
bash "$EVIDENCE/honey-run.sh"
if [[ "$(cat "$MOUNT_ROOT_OUT")" != "$HOME_OK/tcfs-demo/Desktop" ]]; then
  printf 'unexpected normalized honey mount root: %s\n' "$(cat "$MOUNT_ROOT_OUT")" >&2
  exit 1
fi

FAKE_BIN="${TMPDIR}/fake-bin"
RUN_HOME="${TMPDIR}/home-run"
RUN_EVIDENCE="${TMPDIR}/run-evidence"
SSH_LOG="${TMPDIR}/ssh.log"
SCP_LOG="${TMPDIR}/scp.log"
mkdir -p "$FAKE_BIN" "$RUN_HOME"
cat >"$FAKE_BIN/ssh" <<'EOF'
#!/usr/bin/env bash
printf 'ssh' >>"$TCFS_FAKE_SSH_LOG"
printf ' %q' "$@" >>"$TCFS_FAKE_SSH_LOG"
printf '\n' >>"$TCFS_FAKE_SSH_LOG"
case "$*" in
  *honey-run.sh*) printf 'fake honey smoke passed\n' ;;
esac
EOF
cat >"$FAKE_BIN/scp" <<'EOF'
#!/usr/bin/env bash
printf 'scp' >>"$TCFS_FAKE_SCP_LOG"
printf ' %q' "$@" >>"$TCFS_FAKE_SCP_LOG"
printf '\n' >>"$TCFS_FAKE_SCP_LOG"
EOF
chmod +x "$FAKE_BIN/ssh" "$FAKE_BIN/scp"

PATH="$FAKE_BIN:$PATH" \
HOME="$RUN_HOME" \
AWS_ACCESS_KEY_ID="fake-access" \
AWS_SECRET_ACCESS_KEY="fake-secret" \
TCFS_FAKE_SSH_LOG="$SSH_LOG" \
TCFS_FAKE_SCP_LOG="$SCP_LOG" \
bash "$SCRIPT" \
  --remote seaweedfs://example.invalid/tcfs/desktop-demo-run-test \
  --desktop-root "$RUN_HOME/Desktop/TCFS Demo" \
  --evidence-dir "$RUN_EVIDENCE" \
  --honey-host honey-run-test \
  --honey-remote-dir /tmp/tcfs-desktop-honey-run-test \
  --run-honey \
  --honey-start-mount \
  --forward-aws-env \
  >"${TMPDIR}/run.out" \
  2>"${TMPDIR}/run.err"

assert_contains "${TMPDIR}/run.err" "forwarded AWS credentials are inherited"
assert_contains "$RUN_EVIDENCE/honey-run.log" "fake honey smoke passed"
assert_contains "$SSH_LOG" "honey-run-test"
assert_contains "$SSH_LOG" "TCFS_HONEY_START_MOUNT=1"
assert_contains "$SSH_LOG" "TCFS_HONEY_ENV_FILE="
assert_contains "$SSH_LOG" "rm\\ -f"
assert_contains "$SCP_LOG" "lazy-hydration-mounted-smoke.sh"
assert_contains "$SCP_LOG" "honey-run.sh"
if grep -Fq "fake-secret" "$SSH_LOG" "$SCP_LOG" "$RUN_EVIDENCE/honey-run.log"; then
  printf 'forwarded AWS secret leaked into test logs\n' >&2
  exit 1
fi

HOME_BAD="${TMPDIR}/home-bad"
HONEY_DESKTOP="~"/Desktop
mkdir -p "$HOME_BAD"
assert_fails_contains \
  "refusing to use real Desktop as demo root" \
  env HOME="$HOME_BAD" bash "$SCRIPT" \
    --remote seaweedfs://example.invalid/tcfs/desktop-demo-test \
    --desktop-root "$HOME_BAD/Desktop" \
    --evidence-dir "${TMPDIR}/bad-evidence"

assert_fails_contains \
  "refusing to use honey real Desktop as mount root" \
  env HOME="$HOME_BAD" bash "$SCRIPT" \
    --remote seaweedfs://example.invalid/tcfs/desktop-demo-test \
    --desktop-root "$HOME_BAD/Desktop/TCFS Demo" \
    --honey-mount-root "$HONEY_DESKTOP" \
    --evidence-dir "${TMPDIR}/bad-honey-desktop"

ALLOW_HONEY_OUT="${TMPDIR}/allow-honey.out"
HOME="$HOME_BAD" bash "$SCRIPT" \
  --remote seaweedfs://example.invalid/tcfs/desktop-demo-test \
  --desktop-root "$HOME_BAD/Desktop/TCFS Demo" \
  --honey-mount-root "$HONEY_DESKTOP" \
  --allow-honey-real-desktop \
  --evidence-dir "${TMPDIR}/allow-honey-desktop" \
  >"$ALLOW_HONEY_OUT"
assert_contains "$ALLOW_HONEY_OUT" "plan-only: fixture created but not pushed"
assert_contains "${TMPDIR}/allow-honey-desktop/run-metadata.env" "allow_honey_real_desktop=1"

assert_fails_contains \
  "--honey-remote-dir contains unsafe shell characters" \
  env HOME="$HOME_BAD" bash "$SCRIPT" \
    --remote seaweedfs://example.invalid/tcfs/desktop-demo-test \
    --desktop-root "$HOME_BAD/Desktop/TCFS Demo" \
    --honey-remote-dir '/tmp/tcfs;bad' \
    --evidence-dir "${TMPDIR}/bad-remote-dir"

printf 'desktop honey lazy demo tests passed\n'
