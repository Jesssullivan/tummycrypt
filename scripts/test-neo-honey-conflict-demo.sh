#!/usr/bin/env bash
#
# Regression tests for neo-honey-conflict-demo.sh.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$REPO_ROOT/scripts/neo-honey-conflict-demo.sh"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-neo-honey-conflict-test.XXXXXX")"
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
NEO_ROOT="$HOME_OK/TCFS Pilot/conflict"
OUT="$TMPDIR/positive.out"
mkdir -p "$HOME_OK"

HOME="$HOME_OK" bash "$SCRIPT" \
  --remote seaweedfs://example.invalid/tcfs/neo-honey-conflict-test \
  --neo-root "$NEO_ROOT" \
  --evidence-dir "$EVIDENCE" \
  --honey-host honey-test \
  --honey-root /tmp/tcfs-conflict-test/root \
  --honey-tcfs-bin /tmp/tcfs-current \
  >"$OUT"

assert_contains "$OUT" "plan-only: conflict fixture created but not pushed"
assert_contains "$OUT" "neo/honey conflict evidence:"
assert_contains "$OUT" "honey conflict commands:"
assert_contains "$EVIDENCE/run-metadata.env" "honey_host=honey-test"
assert_contains "$EVIDENCE/run-metadata.env" "honey_root=/tmp/tcfs-conflict-test/root"
assert_contains "$EVIDENCE/run-metadata.env" "push=0"
assert_contains "$EVIDENCE/run-metadata.env" "run_honey=0"
assert_contains "$EVIDENCE/result.env" "status=plan-only"
assert_contains "$EVIDENCE/result.env" "proof=pending-push-or-honey-conflict"
assert_contains "$EVIDENCE/README.md" "TCFS neo/honey Conflict Evidence"
assert_contains "$EVIDENCE/tcfs-neo-honey-conflict.toml" "device_name = \"neo-conflict\""
assert_contains "$EVIDENCE/tcfs-neo-honey-conflict.toml" "require_session = false"
assert_contains "$EVIDENCE/honey-conflict-commands.txt" "ssh honey-test"
assert_contains "$EVIDENCE/honey-conflict-run.sh" "push-conflict"
assert_contains "$EVIDENCE/honey-conflict-run.sh" "resolve-keep-both"
assert_contains "$EVIDENCE/neo-tree.txt" "Projects/shared/conflict-notes.md"
test -f "$NEO_ROOT/Projects/shared/conflict-notes.md"
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
  pull)
    manifest="$1"
    local_path="$2"
    mkdir -p "$(dirname "$local_path")"
    if [[ "$manifest" == *conflict-notes.conflict-honey.md || "$manifest" == *conflict-notes.conflict-00000000-0000-4000-8000-0000000000b2.md ]]; then
      cp "$TCFS_FAKE_CONFLICT_COPY_CONTENT" "$local_path"
    elif [[ "$manifest" == *conflict-independent-sibling.md ]]; then
      cp "$TCFS_FAKE_SIBLING_CONTENT" "$local_path"
    else
      cp "$TCFS_FAKE_REMOTE_CONTENT" "$local_path"
    fi
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
  *resolve-keep-both*) printf 'Conflict resolved (keep_both): /tmp/tcfs-conflict-run-test/root/Projects/shared/conflict-notes.md\n  Conflict copy: /tmp/tcfs-conflict-run-test/root/Projects/shared/conflict-notes.conflict-00000000-0000-4000-8000-0000000000b2.md\nhoney daemon resolve keep-both ok: Projects/shared/conflict-notes.md -> Projects/shared/conflict-notes.conflict-00000000-0000-4000-8000-0000000000b2.md\ndaemon_resolve_keep_both=completed\ndaemon_auth_bypass_required=1\noriginal_path_after_resolve=remote_neo_bytes\nconflict_copy_path=Projects/shared/conflict-notes.conflict-00000000-0000-4000-8000-0000000000b2.md\nconflict_copy_content=honey_bytes\nconflict_copy_pushed=1\n' ;;
  *recover-keep-both*) printf 'honey keep-both recovery ok: Projects/shared/conflict-notes.md -> Projects/shared/conflict-notes.conflict-honey.md\nkeep_both_recovery=completed\noriginal_path_after_recovery=remote_neo_bytes\nconflict_copy_path=Projects/shared/conflict-notes.conflict-honey.md\nconflict_copy_content=honey_bytes\nconflict_copy_pushed=1\n' ;;
  *push-sibling*) printf 'honey independent sibling push ok: Projects/shared/conflict-independent-sibling.md\nindependent_sibling_push=completed\nindependent_sibling_content=honey_bytes\nindependent_sibling_conflict=absent\n' ;;
  *prepare*) printf 'honey conflict prepare ok: Projects/shared/conflict-notes.md\n' ;;
  *push-conflict*) printf 'CONFLICT: Projects/shared/conflict-notes.md (local device: honey, remote device: neo)\n  skipped (unchanged since last sync)\nhoney conflict push ok: Projects/shared/conflict-notes.md\nhoney_push_conflict=detected\nhoney_local_content=preserved\nhoney_sync_state=conflict\n' ;;
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
TCFS_FAKE_REMOTE_CONTENT="$RUN_EVIDENCE/neo-conflict-content.txt" \
TCFS_FAKE_CONFLICT_COPY_CONTENT="$RUN_EVIDENCE/honey-conflict-content.txt" \
TCFS_FAKE_SIBLING_CONTENT="$RUN_EVIDENCE/honey-sibling-content.txt" \
bash "$SCRIPT" \
  --remote seaweedfs://example.invalid/tcfs/neo-honey-conflict-run-test \
  --neo-root "$RUN_NEO" \
  --evidence-dir "$RUN_EVIDENCE" \
  --tcfs-bin "$FAKE_BIN/fake-tcfs" \
  --honey-host honey-run-test \
  --honey-root /tmp/tcfs-conflict-run-test/root \
  --honey-remote-dir /tmp/tcfs-conflict-run-test \
  --push \
  --run-honey \
  --honey-recover-keep-both \
  --honey-independent-sibling \
  >"$TMPDIR/run.out"

assert_contains "$RUN_EVIDENCE/neo-initial-push.log" "fake push ok"
assert_contains "$RUN_EVIDENCE/neo-conflict-push.log" "fake push ok"
assert_contains "$RUN_EVIDENCE/honey-prepare.log" "honey conflict prepare ok"
assert_contains "$RUN_EVIDENCE/honey-conflict-push.log" "honey_push_conflict=detected"
assert_contains "$RUN_EVIDENCE/honey-conflict-push.log" "honey_sync_state=conflict"
assert_contains "$RUN_EVIDENCE/honey-independent-sibling-push.log" "independent_sibling_push=completed"
assert_contains "$RUN_EVIDENCE/honey-independent-sibling-push.log" "independent_sibling_conflict=absent"
assert_contains "$RUN_EVIDENCE/honey-keep-both-recovery.log" "keep_both_recovery=completed"
assert_contains "$RUN_EVIDENCE/honey-keep-both-recovery.log" "conflict_copy_pushed=1"
assert_contains "$RUN_EVIDENCE/remote-after-conflict-pull.log" "fake pull ok"
assert_contains "$RUN_EVIDENCE/remote-after-conflict.content" "version: neo-conflict"
assert_contains "$RUN_EVIDENCE/remote-sibling-after-progress.content" "version: honey-sibling"
assert_contains "$RUN_EVIDENCE/remote-original-after-recovery.content" "version: neo-conflict"
assert_contains "$RUN_EVIDENCE/remote-conflict-copy.content" "version: honey-conflict"
assert_contains "$RUN_EVIDENCE/result.env" "status=0"
assert_contains "$RUN_EVIDENCE/result.env" "proof=cross-host-conflict-keep-both-current-behavior"
assert_contains "$RUN_EVIDENCE/result.env" "remote_after_conflict=neo_mutated_preserved"
assert_contains "$RUN_EVIDENCE/result.env" "keep_both_recovery=completed"
assert_contains "$RUN_EVIDENCE/result.env" "conflict_copy_remote=honey_mutated_preserved"
assert_contains "$RUN_EVIDENCE/result.env" "independent_sibling_push=completed"
assert_contains "$RUN_EVIDENCE/result.env" "independent_sibling_remote=honey_mutated_preserved"
assert_contains "$RUN_NEO/Projects/shared/conflict-notes.md" "version: neo-conflict"
assert_contains "$SSH_LOG" "honey-run-test"
assert_contains "$SSH_LOG" "prepare"
assert_contains "$SSH_LOG" "push-conflict"
assert_contains "$SSH_LOG" "push-sibling"
assert_contains "$SSH_LOG" "recover-keep-both"
assert_contains "$SCP_LOG" "honey-conflict-run.sh"
assert_contains "$SCP_LOG" "device-registry.json"
assert_contains "$SCP_LOG" "neo-conflict-content.txt"
assert_contains "$SCP_LOG" "honey-sibling-content.txt"
assert_contains "$TCFS_LOG" "push"
assert_contains "$TCFS_LOG" "pull"

RUN2_HOME="$TMPDIR/home-daemon-run"
RUN2_EVIDENCE="$TMPDIR/run-daemon-evidence"
RUN2_NEO="$RUN2_HOME/TCFS Pilot/run"
mkdir -p "$RUN2_HOME"

PATH="$FAKE_BIN:$PATH" \
HOME="$RUN2_HOME" \
AWS_ACCESS_KEY_ID=test \
AWS_SECRET_ACCESS_KEY=test \
TCFS_FAKE_TCFS_LOG="$TCFS_LOG" \
TCFS_FAKE_SSH_LOG="$SSH_LOG" \
TCFS_FAKE_SCP_LOG="$SCP_LOG" \
TCFS_FAKE_REMOTE_CONTENT="$RUN2_EVIDENCE/neo-conflict-content.txt" \
TCFS_FAKE_CONFLICT_COPY_CONTENT="$RUN2_EVIDENCE/honey-conflict-content.txt" \
TCFS_FAKE_SIBLING_CONTENT="$RUN2_EVIDENCE/honey-sibling-content.txt" \
bash "$SCRIPT" \
  --remote seaweedfs://example.invalid/tcfs/neo-honey-conflict-daemon-run-test \
  --neo-root "$RUN2_NEO" \
  --evidence-dir "$RUN2_EVIDENCE" \
  --tcfs-bin "$FAKE_BIN/fake-tcfs" \
  --honey-host honey-run-test \
  --honey-root /tmp/tcfs-conflict-run-test/root \
  --honey-remote-dir /tmp/tcfs-conflict-run-test \
  --push \
  --run-honey \
  --honey-resolve-keep-both \
  --honey-tcfsd-bin /tmp/fake-tcfsd \
  >"$TMPDIR/run-daemon.out"

assert_contains "$RUN2_EVIDENCE/honey-daemon-resolve-keep-both.log" "daemon_resolve_keep_both=completed"
assert_contains "$RUN2_EVIDENCE/honey-daemon-resolve-keep-both.log" "daemon_auth_bypass_required=1"
assert_contains "$RUN2_EVIDENCE/remote-original-after-daemon-resolve.content" "version: neo-conflict"
assert_contains "$RUN2_EVIDENCE/remote-daemon-conflict-copy.content" "version: honey-conflict"
assert_contains "$RUN2_EVIDENCE/result.env" "status=0"
assert_contains "$RUN2_EVIDENCE/result.env" "proof=cross-host-conflict-daemon-keep-both-current-behavior"
assert_contains "$RUN2_EVIDENCE/result.env" "daemon_resolve_keep_both=completed"
assert_contains "$RUN2_EVIDENCE/result.env" "daemon_auth_bypass_required=1"
assert_contains "$RUN2_EVIDENCE/result.env" "conflict_copy_path=Projects/shared/conflict-notes.conflict-00000000-0000-4000-8000-0000000000b2.md"
assert_contains "$SSH_LOG" "resolve-keep-both"

HOME_BAD="$TMPDIR/home-bad"
mkdir -p "$HOME_BAD"

assert_fails_contains \
  "refusing to use HOME as neo root" \
  env HOME="$HOME_BAD" bash "$SCRIPT" \
    --remote seaweedfs://example.invalid/tcfs/neo-honey-conflict-test \
    --neo-root "$HOME_BAD" \
    --evidence-dir "$TMPDIR/bad-home"

assert_fails_contains \
  "--run-honey requires --push" \
  env HOME="$HOME_BAD" bash "$SCRIPT" \
    --remote seaweedfs://example.invalid/tcfs/neo-honey-conflict-test \
    --neo-root "$HOME_BAD/TCFS Pilot/run" \
    --evidence-dir "$TMPDIR/bad-honey" \
    --run-honey

assert_fails_contains \
  "--honey-root contains unsafe shell characters" \
  env HOME="$HOME_BAD" bash "$SCRIPT" \
    --remote seaweedfs://example.invalid/tcfs/neo-honey-conflict-test \
    --neo-root "$HOME_BAD/TCFS Pilot/run" \
    --honey-root '/tmp/tcfs;bad' \
    --evidence-dir "$TMPDIR/bad-root"

assert_fails_contains \
  "--honey-remote-dir contains unsafe shell characters" \
  env HOME="$HOME_BAD" bash "$SCRIPT" \
    --remote seaweedfs://example.invalid/tcfs/neo-honey-conflict-test \
    --neo-root "$HOME_BAD/TCFS Pilot/run" \
    --honey-remote-dir '/tmp/tcfs;bad' \
    --evidence-dir "$TMPDIR/bad-remote-dir"

assert_fails_contains \
  "--honey-recover-keep-both and --honey-resolve-keep-both are mutually exclusive" \
  env HOME="$HOME_BAD" bash "$SCRIPT" \
    --remote seaweedfs://example.invalid/tcfs/neo-honey-conflict-test \
    --neo-root "$HOME_BAD/TCFS Pilot/run" \
    --honey-recover-keep-both \
    --honey-resolve-keep-both \
    --evidence-dir "$TMPDIR/bad-resolve-mutex"

printf 'neo honey conflict demo tests passed\n'
