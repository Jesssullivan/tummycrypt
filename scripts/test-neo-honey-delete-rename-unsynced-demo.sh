#!/usr/bin/env bash
#
# Regression tests for neo-honey-delete-rename-unsynced-demo.sh.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$REPO_ROOT/scripts/neo-honey-delete-rename-unsynced-demo.sh"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-delete-rename-unsynced-test.XXXXXX")"
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
NEO_ROOT="$HOME_OK/TCFS Pilot/delete-rename-unsynced"
OUT="$TMPDIR/positive.out"
mkdir -p "$HOME_OK"

HOME="$HOME_OK" bash "$SCRIPT" \
  --remote seaweedfs://example.invalid/tcfs/delete-rename-unsynced-test \
  --neo-root "$NEO_ROOT" \
  --evidence-dir "$EVIDENCE" \
  --honey-host honey-test \
  --honey-root /tmp/tcfs-delete-rename-test/root \
  --honey-tcfs-bin /tmp/tcfs-current \
  >"$OUT"

assert_contains "$OUT" "plan-only: fixtures created but not pushed"
assert_contains "$OUT" "delete/rename unsynced evidence:"
assert_contains "$OUT" "honey delete/rename commands:"
assert_contains "$EVIDENCE/run-metadata.env" "honey_host=honey-test"
assert_contains "$EVIDENCE/run-metadata.env" "honey_root=/tmp/tcfs-delete-rename-test/root"
assert_contains "$EVIDENCE/run-metadata.env" "push=0"
assert_contains "$EVIDENCE/run-metadata.env" "run_honey=0"
assert_contains "$EVIDENCE/result.env" "status=plan-only"
assert_contains "$EVIDENCE/result.env" "proof=pending-push-or-honey-delete-rename"
assert_contains "$EVIDENCE/README.md" "TCFS neo/honey Delete/Rename While Peer Unsynced Evidence"
assert_contains "$EVIDENCE/README.md" "stale old stubs are recorded as an open product"
assert_contains "$EVIDENCE/tcfs-delete-rename-unsynced.toml" "sync_empty_dirs = true"
assert_contains "$EVIDENCE/honey-delete-rename-commands.txt" "ssh honey-test"
assert_contains "$EVIDENCE/honey-delete-rename-run.sh" "verify-delete"
assert_contains "$EVIDENCE/honey-delete-rename-run.sh" "verify-rename"
assert_contains "$EVIDENCE/neo-tree.txt" "Projects/shared/delete-me.md"
assert_contains "$EVIDENCE/neo-tree.txt" "Projects/shared/rename-old.md"
test -f "$NEO_ROOT/Projects/shared/delete-me.md"
test -f "$NEO_ROOT/Projects/shared/rename-old.md"
test ! -e "$HOME_OK/Documents"
test ! -e "$HOME_OK/git"

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
  rm)
    target="$1"
    rm -f "$target"
    printf 'fake rm ok: %s\n' "$target"
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
  *prepare-unsync*) printf 'honey delete/rename prepare unsync ok\n' ;;
  *verify-delete*) printf 'honey peer-delete verify ok: Projects/shared/delete-me.md\ndelete_old_pull=failed_as_expected\ndelete_old_pull_repeat=failed_as_expected\ndelete_stub_after_failed_pull=present\n' ;;
  *verify-rename*) printf 'honey peer-rename verify ok: Projects/shared/rename-old.md -> Projects/shared/rename-new.md\nrename_old_pull=failed_as_expected\nrename_old_pull_repeat=failed_as_expected\nrename_new_pull=synced\nrename_new_pull_repeat=synced\nrename_old_stub_after_new_pull=present\n' ;;
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
  --remote seaweedfs://example.invalid/tcfs/delete-rename-unsynced-run-test \
  --neo-root "$RUN_NEO" \
  --evidence-dir "$RUN_EVIDENCE" \
  --tcfs-bin "$FAKE_BIN/fake-tcfs" \
  --honey-host honey-run-test \
  --honey-root /tmp/tcfs-delete-rename-run-test/root \
  --honey-remote-dir /tmp/tcfs-delete-rename-run-test \
  --push \
  --run-honey \
  >"$TMPDIR/run.out"

assert_contains "$RUN_EVIDENCE/neo-initial-push.log" "fake push ok"
assert_contains "$RUN_EVIDENCE/neo-delete.log" "fake rm ok"
assert_contains "$RUN_EVIDENCE/neo-rename-push.log" "fake push ok"
assert_contains "$RUN_EVIDENCE/neo-rename-delete-old.log" "fake rm ok"
assert_contains "$RUN_EVIDENCE/honey-prepare-unsync.log" "honey delete/rename prepare unsync ok"
assert_contains "$RUN_EVIDENCE/honey-verify-delete.log" "delete_old_pull=failed_as_expected"
assert_contains "$RUN_EVIDENCE/honey-verify-delete.log" "delete_old_pull_repeat=failed_as_expected"
assert_contains "$RUN_EVIDENCE/honey-verify-rename.log" "rename_new_pull=synced"
assert_contains "$RUN_EVIDENCE/honey-verify-rename.log" "rename_old_pull_repeat=failed_as_expected"
assert_contains "$RUN_EVIDENCE/honey-verify-rename.log" "rename_new_pull_repeat=synced"
assert_contains "$RUN_EVIDENCE/result.env" "status=0"
assert_contains "$RUN_EVIDENCE/result.env" "proof=delete-rename-peer-unsynced-current-behavior"
assert_contains "$RUN_EVIDENCE/result.env" "delete_old_pull_repeat=failed_as_expected"
assert_contains "$RUN_EVIDENCE/result.env" "rename_old_pull_repeat=failed_as_expected"
assert_contains "$RUN_EVIDENCE/result.env" "rename_new_pull_repeat=synced"
assert_contains "$RUN_EVIDENCE/result.env" "stale_old_stub_cleanup=not-implemented"
assert_contains "$RUN_EVIDENCE/neo-tree-after-delete-rename.txt" "Projects/shared/rename-new.md"
test ! -e "$RUN_NEO/Projects/shared/delete-me.md"
test -f "$RUN_NEO/Projects/shared/rename-new.md"
test ! -e "$RUN_NEO/Projects/shared/rename-old.md"
assert_contains "$SSH_LOG" "honey-run-test"
assert_contains "$SSH_LOG" "prepare-unsync"
assert_contains "$SSH_LOG" "verify-delete"
assert_contains "$SSH_LOG" "verify-rename"
assert_contains "$SCP_LOG" "honey-delete-rename-run.sh"
assert_contains "$TCFS_LOG" "push"
assert_contains "$TCFS_LOG" "rm"

HOME_BAD="$TMPDIR/home-bad"
mkdir -p "$HOME_BAD"

assert_fails_contains \
  "refusing to use HOME as neo root" \
  env HOME="$HOME_BAD" bash "$SCRIPT" \
    --remote seaweedfs://example.invalid/tcfs/delete-rename-unsynced-test \
    --neo-root "$HOME_BAD" \
    --evidence-dir "$TMPDIR/bad-home"

assert_fails_contains \
  "--run-honey requires --push" \
  env HOME="$HOME_BAD" bash "$SCRIPT" \
    --remote seaweedfs://example.invalid/tcfs/delete-rename-unsynced-test \
    --neo-root "$HOME_BAD/TCFS Pilot/run" \
    --evidence-dir "$TMPDIR/bad-honey" \
    --run-honey

assert_fails_contains \
  "--honey-root contains unsafe shell characters" \
  env HOME="$HOME_BAD" bash "$SCRIPT" \
    --remote seaweedfs://example.invalid/tcfs/delete-rename-unsynced-test \
    --neo-root "$HOME_BAD/TCFS Pilot/run" \
    --honey-root '/tmp/tcfs;bad' \
    --evidence-dir "$TMPDIR/bad-root"

assert_fails_contains \
  "--honey-remote-dir contains unsafe shell characters" \
  env HOME="$HOME_BAD" bash "$SCRIPT" \
    --remote seaweedfs://example.invalid/tcfs/delete-rename-unsynced-test \
    --neo-root "$HOME_BAD/TCFS Pilot/run" \
    --honey-remote-dir '/tmp/tcfs;bad' \
    --evidence-dir "$TMPDIR/bad-remote-dir"

printf 'neo honey delete/rename unsynced demo tests passed\n'
