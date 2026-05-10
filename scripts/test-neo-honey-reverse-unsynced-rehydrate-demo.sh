#!/usr/bin/env bash
#
# Regression tests for neo-honey-reverse-unsynced-rehydrate-demo.sh.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$REPO_ROOT/scripts/neo-honey-reverse-unsynced-rehydrate-demo.sh"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-reverse-unsynced-rehydrate-test.XXXXXX")"
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

  local out="$TMPDIR/failure.out"
  local err="$TMPDIR/failure.err"

  if "$@" >"$out" 2>"$err"; then
    printf 'expected command to fail: %s\n' "$*" >&2
    exit 1
  fi

  cat "$out" "$err" >"$TMPDIR/failure.combined"
  assert_contains "$TMPDIR/failure.combined" "$expected"
}

HOME_OK="$TMPDIR/home-ok"
EVIDENCE="$TMPDIR/evidence"
NEO_ROOT="$HOME_OK/TCFS Pilot/reverse-unsynced-rehydrate"
OUT="$TMPDIR/positive.out"
mkdir -p "$HOME_OK"

HOME="$HOME_OK" bash "$SCRIPT" \
  --remote seaweedfs://example.invalid/tcfs/reverse-unsynced-rehydrate-test \
  --neo-root "$NEO_ROOT" \
  --evidence-dir "$EVIDENCE" \
  --honey-host honey-test \
  --honey-root /tmp/tcfs-reverse-test/root \
  --honey-tcfs-bin /tmp/tcfs-current \
  >"$OUT"

assert_contains "$OUT" "plan-only: fixture created but not pushed"
assert_contains "$OUT" "reverse unsynced rehydrate evidence:"
assert_contains "$OUT" "honey reverse commands:"
assert_contains "$EVIDENCE/run-metadata.env" "honey_host=honey-test"
assert_contains "$EVIDENCE/run-metadata.env" "honey_root=/tmp/tcfs-reverse-test/root"
assert_contains "$EVIDENCE/run-metadata.env" "push=0"
assert_contains "$EVIDENCE/run-metadata.env" "run_honey=0"
assert_contains "$EVIDENCE/result.env" "status=plan-only"
assert_contains "$EVIDENCE/result.env" "proof=pending-push-or-honey-reverse"
assert_contains "$EVIDENCE/README.md" "TCFS neo/honey Reverse Unsynced Rehydrate Evidence"
assert_contains "$EVIDENCE/README.md" "honey keeps only"
assert_contains "$EVIDENCE/tcfs-reverse-unsynced-rehydrate.toml" "sync_empty_dirs = true"
assert_contains "$EVIDENCE/honey-reverse-commands.txt" "ssh honey-test"
assert_contains "$EVIDENCE/honey-reverse-run.sh" "prepare-unsync"
assert_contains "$EVIDENCE/honey-reverse-run.sh" "rehydrate"
assert_contains "$EVIDENCE/neo-tree.txt" "Projects/shared/reverse-notes.md"
test -f "$NEO_ROOT/Projects/shared/reverse-notes.md"
test ! -e "$HOME_OK/Documents"
test ! -e "$HOME_OK/git"

MOUNTED_EVIDENCE="$TMPDIR/mounted-plan-evidence"
MOUNTED_NEO_ROOT="$HOME_OK/TCFS Pilot/honey-mounted-reverse-read"
MOUNTED_OUT="$TMPDIR/mounted-plan.out"
HOME="$HOME_OK" bash "$SCRIPT" \
  --remote seaweedfs://example.invalid/tcfs/honey-mounted-reverse-read-test \
  --neo-root "$MOUNTED_NEO_ROOT" \
  --evidence-dir "$MOUNTED_EVIDENCE" \
  --honey-host honey-test \
  --honey-root /tmp/tcfs-mounted-reverse-test/root \
  --honey-mount-root /tmp/tcfs-mounted-reverse-test/mount \
  --honey-mounted-read \
  --honey-start-mount \
  >"$MOUNTED_OUT"

assert_contains "$MOUNTED_EVIDENCE/run-metadata.env" "honey_mounted_read=1"
assert_contains "$MOUNTED_EVIDENCE/run-metadata.env" "honey_start_mount=1"
assert_contains "$MOUNTED_EVIDENCE/run-metadata.env" "honey_mount_root=/tmp/tcfs-mounted-reverse-test/mount"
assert_contains "$MOUNTED_EVIDENCE/result.env" "status=plan-only"
assert_contains "$MOUNTED_EVIDENCE/result.env" "proof=pending-push-or-honey-mounted-reverse-read"
assert_contains "$MOUNTED_EVIDENCE/honey-reverse-run.sh" "mounted-read"
assert_contains "$MOUNTED_EVIDENCE/honey-reverse-commands.txt" "lazy-hydration-mounted-smoke.sh"

FAKE_BIN="$TMPDIR/fake-bin"
RUN_HOME="$TMPDIR/home-run"
RUN_EVIDENCE="$TMPDIR/run-evidence"
RUN_NEO="$RUN_HOME/TCFS Pilot/run"
SSH_LOG="$TMPDIR/ssh.log"
SCP_LOG="$TMPDIR/scp.log"
TCFS_LOG="$TMPDIR/tcfs.log"
mkdir -p "$FAKE_BIN" "$RUN_HOME"

cat >"$FAKE_BIN/fake-tcfs" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

printf 'tcfs' >>"$TCFS_FAKE_TCFS_LOG"
printf ' %q' "$@" >>"$TCFS_FAKE_TCFS_LOG"
printf '\n' >>"$TCFS_FAKE_TCFS_LOG"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --config)
      shift 2
      ;;
    *)
      break
      ;;
  esac
done

cmd="$1"
shift

case "$cmd" in
  push)
    printf 'fake push ok\n'
    ;;
  *)
    printf 'unexpected fake tcfs command: %s\n' "$cmd" >&2
    exit 1
    ;;
esac
EOF

cat >"$FAKE_BIN/ssh" <<'EOF'
#!/usr/bin/env bash
printf 'ssh' >>"$TCFS_FAKE_SSH_LOG"
printf ' %q' "$@" >>"$TCFS_FAKE_SSH_LOG"
printf '\n' >>"$TCFS_FAKE_SSH_LOG"
case "$*" in
  *prepare-unsync*) printf 'honey reverse prepare unsync ok: Projects/shared/reverse-notes.md\n' ;;
  *mounted-read*) printf 'honey reverse mounted read ok: Projects/shared/reverse-notes.md\nhoney_physical_after_mounted_read=stub_present\n' ;;
  *rehydrate*) printf 'honey reverse rehydrate ok: Projects/shared/reverse-notes.md\nstub_after_pull=absent\n' ;;
esac
EOF

cat >"$FAKE_BIN/scp" <<'EOF'
#!/usr/bin/env bash
printf 'scp' >>"$TCFS_FAKE_SCP_LOG"
printf ' %q' "$@" >>"$TCFS_FAKE_SCP_LOG"
printf '\n' >>"$TCFS_FAKE_SCP_LOG"
EOF
chmod +x "$FAKE_BIN/fake-tcfs" "$FAKE_BIN/ssh" "$FAKE_BIN/scp"

PATH="$FAKE_BIN:$PATH" \
HOME="$RUN_HOME" \
AWS_ACCESS_KEY_ID=test \
AWS_SECRET_ACCESS_KEY=test \
TCFS_FAKE_TCFS_LOG="$TCFS_LOG" \
TCFS_FAKE_SSH_LOG="$SSH_LOG" \
TCFS_FAKE_SCP_LOG="$SCP_LOG" \
bash "$SCRIPT" \
  --remote seaweedfs://example.invalid/tcfs/reverse-unsynced-rehydrate-run-test \
  --neo-root "$RUN_NEO" \
  --evidence-dir "$RUN_EVIDENCE" \
  --tcfs-bin "$FAKE_BIN/fake-tcfs" \
  --honey-host honey-run-test \
  --honey-root /tmp/tcfs-reverse-run-test/root \
  --honey-remote-dir /tmp/tcfs-reverse-run-test \
  --push \
  --run-honey \
  >"$TMPDIR/run.out"

assert_contains "$RUN_EVIDENCE/neo-initial-push.log" "fake push ok"
assert_contains "$RUN_EVIDENCE/neo-mutated-push.log" "fake push ok"
assert_contains "$RUN_EVIDENCE/honey-prepare-unsync.log" "honey reverse prepare unsync ok"
assert_contains "$RUN_EVIDENCE/honey-rehydrate.log" "honey reverse rehydrate ok"
assert_contains "$RUN_EVIDENCE/honey-rehydrate.log" "stub_after_pull=absent"
assert_contains "$RUN_EVIDENCE/result.env" "status=0"
assert_contains "$RUN_EVIDENCE/result.env" "proof=reverse-same-fixture-unsynced-rehydrate"
assert_contains "$RUN_NEO/Projects/shared/reverse-notes.md" "version: neo-mutated"
assert_contains "$SSH_LOG" "honey-run-test"
assert_contains "$SSH_LOG" "prepare-unsync"
assert_contains "$SSH_LOG" "rehydrate"
assert_contains "$SCP_LOG" "honey-reverse-run.sh"
assert_contains "$TCFS_LOG" "push"

RUN_MOUNTED_EVIDENCE="$TMPDIR/run-mounted-evidence"
RUN_MOUNTED_NEO="$RUN_HOME/TCFS Pilot/mounted-run"
PATH="$FAKE_BIN:$PATH" \
HOME="$RUN_HOME" \
AWS_ACCESS_KEY_ID=test \
AWS_SECRET_ACCESS_KEY=test \
TCFS_FAKE_TCFS_LOG="$TCFS_LOG" \
TCFS_FAKE_SSH_LOG="$SSH_LOG" \
TCFS_FAKE_SCP_LOG="$SCP_LOG" \
bash "$SCRIPT" \
  --remote seaweedfs://example.invalid/tcfs/honey-mounted-reverse-read-run-test \
  --neo-root "$RUN_MOUNTED_NEO" \
  --evidence-dir "$RUN_MOUNTED_EVIDENCE" \
  --tcfs-bin "$FAKE_BIN/fake-tcfs" \
  --honey-host honey-mounted-run-test \
  --honey-root /tmp/tcfs-mounted-reverse-run-test/root \
  --honey-mount-root /tmp/tcfs-mounted-reverse-run-test/mount \
  --honey-remote-dir /tmp/tcfs-mounted-reverse-run-test \
  --push \
  --run-honey \
  --honey-mounted-read \
  --honey-start-mount \
  >"$TMPDIR/mounted-run.out"

assert_contains "$RUN_MOUNTED_EVIDENCE/honey-prepare-unsync.log" "honey reverse prepare unsync ok"
assert_contains "$RUN_MOUNTED_EVIDENCE/honey-mounted-read.log" "honey reverse mounted read ok"
assert_contains "$RUN_MOUNTED_EVIDENCE/honey-mounted-read.log" "honey_physical_after_mounted_read=stub_present"
assert_contains "$RUN_MOUNTED_EVIDENCE/result.env" "status=0"
assert_contains "$RUN_MOUNTED_EVIDENCE/result.env" "proof=linux-mounted-reverse-read-current-behavior"
assert_contains "$RUN_MOUNTED_EVIDENCE/run-metadata.env" "honey_mounted_read=1"
assert_contains "$RUN_MOUNTED_EVIDENCE/run-metadata.env" "honey_start_mount=1"
assert_contains "$SCP_LOG" "lazy-hydration-mounted-smoke.sh"
assert_contains "$SSH_LOG" "mounted-read"

HOME_BAD="$TMPDIR/home-bad"
mkdir -p "$HOME_BAD"

assert_fails_contains \
  "refusing to use HOME as neo root" \
  env HOME="$HOME_BAD" bash "$SCRIPT" \
    --remote seaweedfs://example.invalid/tcfs/reverse-unsynced-rehydrate-test \
    --neo-root "$HOME_BAD" \
    --evidence-dir "$TMPDIR/bad-home"

assert_fails_contains \
  "--run-honey requires --push" \
  env HOME="$HOME_BAD" bash "$SCRIPT" \
    --remote seaweedfs://example.invalid/tcfs/reverse-unsynced-rehydrate-test \
    --neo-root "$HOME_BAD/TCFS Pilot/run" \
    --evidence-dir "$TMPDIR/bad-honey" \
    --run-honey

assert_fails_contains \
  "--honey-root contains unsafe shell characters" \
  env HOME="$HOME_BAD" bash "$SCRIPT" \
    --remote seaweedfs://example.invalid/tcfs/reverse-unsynced-rehydrate-test \
    --neo-root "$HOME_BAD/TCFS Pilot/run" \
    --honey-root '/tmp/tcfs;bad' \
    --evidence-dir "$TMPDIR/bad-root"

assert_fails_contains \
  "--honey-remote-dir contains unsafe shell characters" \
  env HOME="$HOME_BAD" bash "$SCRIPT" \
    --remote seaweedfs://example.invalid/tcfs/reverse-unsynced-rehydrate-test \
    --neo-root "$HOME_BAD/TCFS Pilot/run" \
    --honey-remote-dir '/tmp/tcfs;bad' \
    --evidence-dir "$TMPDIR/bad-remote-dir"

printf 'neo honey reverse unsynced rehydrate demo tests passed\n'
