#!/usr/bin/env bash
#
# Regression tests for fleet-parity-pilot-demo.sh.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="${REPO_ROOT}/scripts/fleet-parity-pilot-demo.sh"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-fleet-pilot-test.XXXXXX")"
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
  --remote seaweedfs://example.invalid/tcfs/fleet-pilot-test \
  --pilot-root "$HOME_OK/TCFS Pilot/run" \
  --evidence-dir "$EVIDENCE" \
  --honey-host honey-test \
  --honey-tcfs-bin /tmp/tcfs-current \
  >"$OUT"

assert_contains "$OUT" "fleet pilot root:"
assert_contains "$OUT" "honey fleet commands:"
assert_contains "$OUT" "honey linux lifecycle commands:"
assert_contains "$EVIDENCE/run-metadata.env" "honey_host=honey-test"
assert_contains "$EVIDENCE/run-metadata.env" "push=0"
assert_contains "$EVIDENCE/run-metadata.env" "run_linux_lifecycle=0"
assert_contains "$EVIDENCE/run-metadata.env" "linux_lifecycle_remote=seaweedfs://example.invalid/tcfs/fleet-pilot-test/linux-lifecycle"
assert_contains "$EVIDENCE/README.md" "TCFS Fleet Parity Pilot Evidence"
assert_contains "$EVIDENCE/README.md" "honey-linux-lifecycle-commands.txt"
assert_contains "$EVIDENCE/fleet-pilot-tree.txt" "Documents/fleet-readiness.md"
assert_contains "$EVIDENCE/fleet-pilot-tree.txt" "git/tcfs-pilot-repo/README.md"
assert_contains "$EVIDENCE/fleet-documents-expected.txt" "TCFS Fleet Pilot"
assert_contains "$EVIDENCE/honey-fleet-run.sh" "Documents/fleet-readiness.md"
assert_contains "$EVIDENCE/honey-fleet-run.sh" "--expect-entry git/tcfs-pilot-repo"
assert_contains "$EVIDENCE/honey-fleet-commands.txt" "ssh honey-test"
assert_contains "$EVIDENCE/honey-linux-lifecycle-commands.txt" "lazy-hydration-linux-lifecycle-demo.sh"
assert_contains "$EVIDENCE/honey-linux-lifecycle-run.sh" "TCFS_BIN_RESOLVED"
assert_contains "$EVIDENCE/linux-lifecycle-status.env" "ran=0"
assert_contains "$EVIDENCE/desktop-honey/honey-commands.txt" "ssh honey-test"
test -f "$HOME_OK/TCFS Pilot/run/Documents/fleet-readiness.md"
test -f "$HOME_OK/TCFS Pilot/run/git/tcfs-pilot-repo/.git/HEAD"
test ! -e "$HOME_OK/Documents"
test ! -e "$HOME_OK/git"

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
  *honey-fleet-run.sh*) printf 'fake fleet honey smoke passed\n' ;;
  *honey-linux-lifecycle-run.sh*) printf 'fake linux lifecycle passed\n' ;;
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
TCFS_FAKE_SSH_LOG="$SSH_LOG" \
TCFS_FAKE_SCP_LOG="$SCP_LOG" \
bash "$SCRIPT" \
  --remote seaweedfs://example.invalid/tcfs/fleet-pilot-run-test \
  --pilot-root "$RUN_HOME/TCFS Pilot/run" \
  --evidence-dir "$RUN_EVIDENCE" \
  --honey-host honey-run-test \
  --honey-remote-dir /tmp/tcfs-fleet-pilot-run-test \
  --run-honey \
  --run-linux-lifecycle \
  --honey-existing-mount \
  >"${TMPDIR}/run.out"

assert_contains "$RUN_EVIDENCE/desktop-honey/honey-run.log" "fake honey smoke passed"
assert_contains "$RUN_EVIDENCE/honey-fleet-run.log" "fake fleet honey smoke passed"
assert_contains "$RUN_EVIDENCE/honey-linux-lifecycle.log" "fake linux lifecycle passed"
assert_contains "$RUN_EVIDENCE/linux-lifecycle-status.env" "ran=1"
assert_contains "$RUN_EVIDENCE/linux-lifecycle-status.env" "status=0"
assert_contains "$SSH_LOG" "honey-run-test"
assert_contains "$SSH_LOG" "honey-fleet-run.sh"
assert_contains "$SSH_LOG" "honey-linux-lifecycle-run.sh"
assert_contains "$SCP_LOG" "fleet-documents-expected.txt"
assert_contains "$SCP_LOG" "honey-fleet-run.sh"
assert_contains "$SCP_LOG" "lazy-hydration-linux-demo.sh"
assert_contains "$SCP_LOG" "linux-lifecycle/evidence/."

HOME_BAD="${TMPDIR}/home-bad"
mkdir -p "$HOME_BAD"

assert_fails_contains \
  "refusing to use HOME as pilot root" \
  env HOME="$HOME_BAD" bash "$SCRIPT" \
    --remote seaweedfs://example.invalid/tcfs/fleet-pilot-test \
    --pilot-root "$HOME_BAD" \
    --evidence-dir "${TMPDIR}/bad-home"

assert_fails_contains \
  "refusing to use real Documents as pilot root" \
  env HOME="$HOME_BAD" bash "$SCRIPT" \
    --remote seaweedfs://example.invalid/tcfs/fleet-pilot-test \
    --pilot-root "$HOME_BAD/Documents" \
    --evidence-dir "${TMPDIR}/bad-documents"

assert_fails_contains \
  "refusing to use real git as pilot root" \
  env HOME="$HOME_BAD" bash "$SCRIPT" \
    --remote seaweedfs://example.invalid/tcfs/fleet-pilot-test \
    --pilot-root "$HOME_BAD/git" \
    --evidence-dir "${TMPDIR}/bad-git"

assert_fails_contains \
  "--honey-remote-dir contains unsafe shell characters" \
  env HOME="$HOME_BAD" bash "$SCRIPT" \
    --remote seaweedfs://example.invalid/tcfs/fleet-pilot-test \
    --pilot-root "$HOME_BAD/TCFS Pilot/run" \
    --honey-remote-dir '/tmp/tcfs;bad' \
    --evidence-dir "${TMPDIR}/bad-remote-dir"

printf 'fleet parity pilot demo tests passed\n'
