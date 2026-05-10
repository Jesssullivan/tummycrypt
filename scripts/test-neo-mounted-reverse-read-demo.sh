#!/usr/bin/env bash
#
# Regression tests for neo-mounted-reverse-read-demo.sh.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$REPO_ROOT/scripts/neo-mounted-reverse-read-demo.sh"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-mounted-reverse-read-test.XXXXXX")"
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

write_mutated_mount_fixture() {
  local mount_root="$1"
  local fixture="$mount_root/Projects/shared/mounted-reverse-notes.md"

  mkdir -p "$(dirname "$fixture")"
  cat >"$fixture" <<'EOF'
# Mounted reverse TCFS note

version: honey-mutated
body: honey updated this while neo had only the physical .tc stub.
EOF
}

HOME_OK="$TMPDIR/home-ok"
EVIDENCE="$TMPDIR/evidence"
NEO_ROOT="$HOME_OK/TCFS Pilot/mounted-reverse-read"
NEO_MOUNT="$TMPDIR/neo-mounted"
OUT="$TMPDIR/positive.out"
mkdir -p "$HOME_OK"

HOME="$HOME_OK" bash "$SCRIPT" \
  --remote seaweedfs://example.invalid/tcfs/mounted-reverse-read-test \
  --neo-root "$NEO_ROOT" \
  --neo-mount-root "$NEO_MOUNT" \
  --evidence-dir "$EVIDENCE" \
  --honey-host honey-test \
  --honey-root /tmp/tcfs-mounted-reverse-test/root \
  --honey-tcfs-bin /tmp/tcfs-current \
  >"$OUT"

assert_contains "$OUT" "plan-only: M4 mounted reverse-read packet created"
assert_contains "$OUT" "mounted reverse-read evidence:"
assert_contains "$OUT" "honey mounted reverse commands:"
assert_contains "$EVIDENCE/run-metadata.env" "honey_host=honey-test"
assert_contains "$EVIDENCE/run-metadata.env" "honey_root=/tmp/tcfs-mounted-reverse-test/root"
assert_contains "$EVIDENCE/run-metadata.env" "neo_mount_root="
assert_contains "$EVIDENCE/run-metadata.env" "neo-mounted"
assert_contains "$EVIDENCE/run-metadata.env" "push=0"
assert_contains "$EVIDENCE/run-metadata.env" "run_honey=0"
assert_contains "$EVIDENCE/run-metadata.env" "neo_nfs=0"
assert_contains "$EVIDENCE/result.env" "status=plan-only"
assert_contains "$EVIDENCE/result.env" "proof=pending-mounted-reverse-read"
assert_contains "$EVIDENCE/README.md" "TCFS neo Mounted Reverse-Read Evidence"
assert_contains "$EVIDENCE/README.md" "physical neo sync root"
assert_contains "$EVIDENCE/tcfs-mounted-reverse-read.toml" "sync_empty_dirs = true"
assert_contains "$EVIDENCE/honey-mounted-reverse-commands.txt" "ssh honey-test"
assert_contains "$EVIDENCE/honey-mounted-reverse-run.sh" "push-initial"
assert_contains "$EVIDENCE/honey-mounted-reverse-run.sh" "push-mutated"
test ! -e "$HOME_OK/Documents"
test ! -e "$HOME_OK/git"

FAKE_BIN="$TMPDIR/fake-bin"
RUN_HOME="$TMPDIR/home-run"
RUN_EVIDENCE="$TMPDIR/run-evidence"
RUN_NEO="$RUN_HOME/TCFS Pilot/run"
RUN_MOUNT="$TMPDIR/run-mounted"
SSH_LOG="$TMPDIR/ssh.log"
SCP_LOG="$TMPDIR/scp.log"
TCFS_LOG="$TMPDIR/tcfs.log"
mkdir -p "$FAKE_BIN" "$RUN_HOME"
write_mutated_mount_fixture "$RUN_MOUNT"

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
  pull)
    manifest="$1"
    local_path="$2"
    mkdir -p "$(dirname "$local_path")"
    cp "$TCFS_FAKE_INITIAL_CONTENT" "$local_path"
    printf 'fake pull ok: %s -> %s\n' "$manifest" "$local_path"
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
origin seaweedfs://example.invalid/tcfs/Projects/shared/mounted-reverse-notes.md
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
  *push-initial*) printf 'honey mounted reverse initial push ok: Projects/shared/mounted-reverse-notes.md\n' ;;
  *push-mutated*) printf 'honey mounted reverse mutated push ok: Projects/shared/mounted-reverse-notes.md\n' ;;
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
TCFS_FAKE_INITIAL_CONTENT="$RUN_EVIDENCE/honey-initial-content.txt" \
bash "$SCRIPT" \
  --remote seaweedfs://example.invalid/tcfs/mounted-reverse-read-run-test \
  --neo-root "$RUN_NEO" \
  --neo-mount-root "$RUN_MOUNT" \
  --evidence-dir "$RUN_EVIDENCE" \
  --tcfs-bin "$FAKE_BIN/fake-tcfs" \
  --honey-host honey-run-test \
  --honey-root /tmp/tcfs-mounted-reverse-run-test/root \
  --honey-remote-dir /tmp/tcfs-mounted-reverse-run-test \
  --push \
  --run-honey \
  --neo-existing-mount \
  --neo-nfs \
  >"$TMPDIR/run.out"

assert_contains "$RUN_EVIDENCE/honey-initial-push.log" "honey mounted reverse initial push ok"
assert_contains "$RUN_EVIDENCE/neo-initial-pull.log" "fake pull ok"
assert_contains "$RUN_EVIDENCE/neo-unsync.out" "fake unsync ok"
assert_contains "$RUN_EVIDENCE/neo-sync-status-after-unsync.out" "sync state: not_synced"
assert_contains "$RUN_EVIDENCE/honey-mutated-push.log" "honey mounted reverse mutated push ok"
assert_contains "$RUN_EVIDENCE/neo-mounted-read.log" "lazy hydration mounted smoke passed"
assert_contains "$RUN_EVIDENCE/neo-mounted-read.log" "cat hydrate target: Projects/shared/mounted-reverse-notes.md"
assert_contains "$RUN_EVIDENCE/neo-physical-stub-after-mounted-read.env" "neo_physical_after_mounted_read=stub_present"
assert_contains "$RUN_EVIDENCE/result.env" "status=0"
assert_contains "$RUN_EVIDENCE/result.env" "proof=mounted-reverse-read-current-behavior"
assert_contains "$RUN_EVIDENCE/result.env" "neo_physical_after_mounted_read=stub_present"
assert_contains "$RUN_EVIDENCE/run-metadata.env" "neo_nfs=1"
test ! -e "$RUN_NEO/Projects/shared/mounted-reverse-notes.md"
test -f "$RUN_NEO/Projects/shared/mounted-reverse-notes.md.tc"
assert_contains "$SSH_LOG" "honey-run-test"
assert_contains "$SSH_LOG" "push-initial"
assert_contains "$SSH_LOG" "push-mutated"
assert_contains "$SCP_LOG" "honey-mounted-reverse-run.sh"
assert_contains "$TCFS_LOG" "pull"
assert_contains "$TCFS_LOG" "unsync"

HOME_BAD="$TMPDIR/home-bad"
mkdir -p "$HOME_BAD"

assert_fails_contains \
  "refusing to use HOME as neo root" \
  env HOME="$HOME_BAD" bash "$SCRIPT" \
    --remote seaweedfs://example.invalid/tcfs/mounted-reverse-read-test \
    --neo-root "$HOME_BAD" \
    --evidence-dir "$TMPDIR/bad-home"

assert_fails_contains \
  "refusing to use real Documents as neo root" \
  env HOME="$HOME_BAD" bash "$SCRIPT" \
    --remote seaweedfs://example.invalid/tcfs/mounted-reverse-read-test \
    --neo-root "$HOME_BAD/Documents" \
    --evidence-dir "$TMPDIR/bad-documents"

assert_fails_contains \
  "refusing to use real git as neo root" \
  env HOME="$HOME_BAD" bash "$SCRIPT" \
    --remote seaweedfs://example.invalid/tcfs/mounted-reverse-read-test \
    --neo-root "$HOME_BAD/git" \
    --evidence-dir "$TMPDIR/bad-git"

assert_fails_contains \
  "--run-honey requires --push" \
  env HOME="$HOME_BAD" bash "$SCRIPT" \
    --remote seaweedfs://example.invalid/tcfs/mounted-reverse-read-test \
    --neo-root "$HOME_BAD/TCFS Pilot/run" \
    --evidence-dir "$TMPDIR/bad-honey" \
    --run-honey

assert_fails_contains \
  "--push for M4 requires --run-honey" \
  env HOME="$HOME_BAD" AWS_ACCESS_KEY_ID=test AWS_SECRET_ACCESS_KEY=test bash "$SCRIPT" \
    --remote seaweedfs://example.invalid/tcfs/mounted-reverse-read-test \
    --neo-root "$HOME_BAD/TCFS Pilot/run" \
    --evidence-dir "$TMPDIR/bad-push" \
    --push

assert_fails_contains \
  "--honey-root contains unsafe shell characters" \
  env HOME="$HOME_BAD" bash "$SCRIPT" \
    --remote seaweedfs://example.invalid/tcfs/mounted-reverse-read-test \
    --neo-root "$HOME_BAD/TCFS Pilot/run" \
    --honey-root '/tmp/tcfs;bad' \
    --evidence-dir "$TMPDIR/bad-root"

assert_fails_contains \
  "--honey-remote-dir contains unsafe shell characters" \
  env HOME="$HOME_BAD" bash "$SCRIPT" \
    --remote seaweedfs://example.invalid/tcfs/mounted-reverse-read-test \
    --neo-root "$HOME_BAD/TCFS Pilot/run" \
    --honey-remote-dir '/tmp/tcfs;bad' \
    --evidence-dir "$TMPDIR/bad-remote-dir"

assert_fails_contains \
  "refusing to use HOME as neo mount root" \
  env HOME="$HOME_BAD" bash "$SCRIPT" \
    --remote seaweedfs://example.invalid/tcfs/mounted-reverse-read-test \
    --neo-root "$HOME_BAD/TCFS Pilot/run" \
    --neo-mount-root "$HOME_BAD" \
    --evidence-dir "$TMPDIR/bad-mount-home"

printf 'neo mounted reverse read demo tests passed\n'
