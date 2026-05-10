#!/usr/bin/env bash
#
# Regression tests for neo-honey-unsynced-rehydrate-demo.sh.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$REPO_ROOT/scripts/neo-honey-unsynced-rehydrate-demo.sh"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-unsynced-rehydrate-test.XXXXXX")"
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
NEO_ROOT="$HOME_OK/TCFS Pilot/unsynced-rehydrate"
OUT="$TMPDIR/positive.out"
mkdir -p "$HOME_OK"

HOME="$HOME_OK" bash "$SCRIPT" \
  --remote seaweedfs://example.invalid/tcfs/unsynced-rehydrate-test \
  --neo-root "$NEO_ROOT" \
  --evidence-dir "$EVIDENCE" \
  --honey-host honey-test \
  --honey-tcfs-bin /tmp/tcfs-current \
  >"$OUT"

assert_contains "$OUT" "plan-only: fixture created but not pushed"
assert_contains "$OUT" "neo root:"
assert_contains "$OUT" "honey mutator commands:"
assert_contains "$EVIDENCE/run-metadata.env" "honey_host=honey-test"
assert_contains "$EVIDENCE/run-metadata.env" "push=0"
assert_contains "$EVIDENCE/run-metadata.env" "run_honey=0"
assert_contains "$EVIDENCE/result.env" "status=plan-only"
assert_contains "$EVIDENCE/result.env" "proof=pending-push-or-honey"
assert_contains "$EVIDENCE/README.md" "TCFS neo/honey Unsynced Rehydrate Evidence"
assert_contains "$EVIDENCE/README.md" "adjacent \`.tc\` stub must be gone"
assert_contains "$EVIDENCE/tcfs-unsynced-rehydrate.toml" "sync_empty_dirs = true"
assert_contains "$EVIDENCE/honey-mutator-commands.txt" "ssh honey-test"
assert_contains "$EVIDENCE/honey-mutator-run.sh" "honey mounted mutation wrote exact content"
assert_contains "$EVIDENCE/neo-tree.txt" "Projects/shared/notes.md"
test -f "$NEO_ROOT/Projects/shared/notes.md"
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
  unsync)
    target="$1"
    rm -f "$target"
    cat >"${target}.tc" <<'STUB'
version https://tummycrypt.io/tcfs/v1
chunks 1
compressed 0
fetched 0
oid blake3:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
origin seaweedfs://example.invalid/tcfs/Projects/shared/notes.md
size 91
STUB
    printf 'fake unsync ok\n'
    ;;
  sync-status)
    target="$1"
    if [[ -f "$target" ]]; then
      printf 'sync state: synced\n'
    else
      printf 'sync state: not_synced\n'
    fi
    ;;
  pull)
    manifest="$1"
    local_path="$2"
    mkdir -p "$(dirname "$local_path")"
    cp "$TCFS_FAKE_MUTATED_CONTENT" "$local_path"
    rm -f "${local_path}.tc"
    printf 'fake pull ok: %s -> %s\n' "$manifest" "$local_path"
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
  *honey-mutator-run.sh*) printf 'fake honey mounted mutation wrote exact content\n' ;;
  *mount.log*) printf 'fake mount log\n' ;;
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
TCFS_FAKE_MUTATED_CONTENT="$RUN_EVIDENCE/honey-mutated-content.txt" \
bash "$SCRIPT" \
  --remote seaweedfs://example.invalid/tcfs/unsynced-rehydrate-run-test \
  --neo-root "$RUN_NEO" \
  --evidence-dir "$RUN_EVIDENCE" \
  --tcfs-bin "$FAKE_BIN/fake-tcfs" \
  --honey-host honey-run-test \
  --honey-remote-dir /tmp/tcfs-unsynced-rehydrate-run-test \
  --honey-existing-mount \
  --push \
  --run-honey \
  >"$TMPDIR/run.out"

assert_contains "$RUN_EVIDENCE/push.log" "fake push ok"
assert_contains "$RUN_EVIDENCE/unsync.out" "fake unsync ok"
assert_contains "$RUN_EVIDENCE/rehydrate-pull.log" "fake pull ok"
assert_contains "$RUN_EVIDENCE/sync-status-after-unsync.out" "sync state: not_synced"
assert_contains "$RUN_EVIDENCE/sync-status-after-rehydrate.out" "sync state: synced"
assert_contains "$RUN_EVIDENCE/honey-mutator.log" "fake honey mounted mutation wrote exact content"
assert_contains "$RUN_EVIDENCE/honey-mount.log" "fake mount log"
assert_contains "$RUN_EVIDENCE/stub-status.env" "stub_after_pull=absent"
assert_contains "$RUN_EVIDENCE/result.env" "status=0"
assert_contains "$RUN_EVIDENCE/result.env" "proof=same-fixture-unsynced-rehydrate"
assert_contains "$SSH_LOG" "honey-run-test"
assert_contains "$SSH_LOG" "honey-mutator-run.sh"
assert_contains "$SCP_LOG" "lazy-hydration-mounted-smoke.sh"
assert_contains "$SCP_LOG" "honey-mutator-run.sh"
assert_contains "$TCFS_LOG" "push"
assert_contains "$TCFS_LOG" "unsync"
assert_contains "$TCFS_LOG" "pull"
cmp -s "$RUN_EVIDENCE/honey-mutated-content.txt" "$RUN_NEO/Projects/shared/notes.md"
test ! -e "$RUN_NEO/Projects/shared/notes.md.tc"

HOME_BAD="$TMPDIR/home-bad"
mkdir -p "$HOME_BAD"

assert_fails_contains \
  "refusing to use HOME as neo root" \
  env HOME="$HOME_BAD" bash "$SCRIPT" \
    --remote seaweedfs://example.invalid/tcfs/unsynced-rehydrate-test \
    --neo-root "$HOME_BAD" \
    --evidence-dir "$TMPDIR/bad-home"

assert_fails_contains \
  "refusing to use real Documents as neo root" \
  env HOME="$HOME_BAD" bash "$SCRIPT" \
    --remote seaweedfs://example.invalid/tcfs/unsynced-rehydrate-test \
    --neo-root "$HOME_BAD/Documents" \
    --evidence-dir "$TMPDIR/bad-documents"

assert_fails_contains \
  "refusing to use real git as neo root" \
  env HOME="$HOME_BAD" bash "$SCRIPT" \
    --remote seaweedfs://example.invalid/tcfs/unsynced-rehydrate-test \
    --neo-root "$HOME_BAD/git" \
    --evidence-dir "$TMPDIR/bad-git"

assert_fails_contains \
  "--run-honey requires --push" \
  env HOME="$HOME_BAD" bash "$SCRIPT" \
    --remote seaweedfs://example.invalid/tcfs/unsynced-rehydrate-test \
    --neo-root "$HOME_BAD/TCFS Pilot/run" \
    --evidence-dir "$TMPDIR/bad-honey" \
    --run-honey

assert_fails_contains \
  "--honey-remote-dir contains unsafe shell characters" \
  env HOME="$HOME_BAD" bash "$SCRIPT" \
    --remote seaweedfs://example.invalid/tcfs/unsynced-rehydrate-test \
    --neo-root "$HOME_BAD/TCFS Pilot/run" \
    --honey-remote-dir '/tmp/tcfs;bad' \
    --evidence-dir "$TMPDIR/bad-remote-dir"

printf 'neo honey unsynced rehydrate demo tests passed\n'
