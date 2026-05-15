#!/usr/bin/env bash
#
# Regression tests for git-repo-restore-proof.sh.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$REPO_ROOT/scripts/git-repo-restore-proof.sh"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-git-repo-restore-proof-test.XXXXXX")"
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

PACKET="$TMPDIR/git-repo-canary-fixture"
SHADOW="$TMPDIR/shadow tree"
RESTORE="$TMPDIR/restored tree"
STATE="$TMPDIR/restore-state.json"
CONFIG="$PACKET/state/tcfs-git-repo-canary.toml"
FAKE_TCFS="$TMPDIR/fake-tcfs"
mkdir -p "$PACKET/state" "$SHADOW/src" "$SHADOW/empty-dir"

cat >"$SHADOW/README.md" <<'EOF'
# restore fixture
EOF
cat >"$SHADOW/src/main.rs" <<'EOF'
fn main() {}
EOF
ln -s README.md "$SHADOW/readme-link"

cat >"$PACKET/git-repo-canary-policy.env" <<EOF
run_id=git-repo-canary-fixture
shadow=$SHADOW
remote=seaweedfs://example.invalid/tcfs/git-repo-canary-fixture
EOF
cat >"$PACKET/run-metadata.env" <<EOF
remote_prefix=git-repo-canary-fixture
tcfs_command=$FAKE_TCFS
EOF
cat >"$CONFIG" <<'EOF'
[storage]
endpoint = "http://example.invalid"
bucket = "tcfs"
remote_prefix = "git-repo-canary-fixture"

[sync]
sync_root = "/tmp/unused"
state_db = "/tmp/unused/state.db"
sync_git_dirs = true
git_sync_mode = "raw"
sync_hidden_dirs = true
sync_symlinks = true
sync_empty_dirs = true

[crypto]
enabled = false
EOF

cat >"$FAKE_TCFS" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "--version" ]]; then
  printf 'tcfs fake restore\n'
  exit 0
fi

restore_path=""
execute=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    reconcile)
      shift
      ;;
    --path)
      restore_path="$2"
      shift 2
      ;;
    --execute)
      execute=1
      shift
      ;;
    *)
      if [[ $# -ge 2 && "$2" != --* ]]; then
        shift 2
      else
        shift
      fi
      ;;
  esac
done

printf 'Plan: 0 push, 3 pull, 0 delete-local, 0 delete-remote, 0 conflict, 0 up-to-date\n'
if [[ "$execute" == "1" ]]; then
  mkdir -p "$restore_path"
  (
    cd "$FAKE_RESTORE_SOURCE"
    find . -type f -print | while IFS= read -r path; do
      rel="${path#./}"
      mkdir -p "$restore_path/$(dirname "$rel")"
      cp -p "$path" "$restore_path/$rel"
    done
    find . -type l -print | while IFS= read -r path; do
      rel="${path#./}"
      mkdir -p "$restore_path/$(dirname "$rel")"
      ln -s "$(readlink "$path")" "$restore_path/$rel"
    done
  )
  printf 'Done: 0 pushed, 3 pulled, 0 deleted, 0 conflicts, 0 errors\n'
fi
EOF
chmod +x "$FAKE_TCFS"

FAKE_RESTORE_SOURCE="$SHADOW" bash "$SCRIPT" \
  --evidence-dir "$PACKET" \
  --restore-root "$RESTORE" \
  --config "$CONFIG" \
  --state "$STATE" \
  >"$TMPDIR/positive.out"

assert_contains "$TMPDIR/positive.out" "restore proof status: passed"
assert_contains "$PACKET/restore-proof/restore-proof.env" "status=passed"
assert_contains "$PACKET/restore-proof/restore-proof.env" "proof=fresh-tree-restore-files-and-symlinks-empty-dirs-gap"
assert_contains "$PACKET/restore-proof/restore-proof.env" "regular_files_match=1"
assert_contains "$PACKET/restore-proof/restore-proof.env" "symlink_targets_match=1"
assert_contains "$PACKET/restore-proof/restore-proof.env" "state_manifest_status=unavailable"
assert_contains "$PACKET/restore-proof/restore-proof.env" "empty_dirs_match=0"
assert_contains "$PACKET/restore-proof/restore-proof.env" "shadow_empty_dir_count=1"
assert_contains "$PACKET/restore-proof/restore-proof.env" "restored_empty_dir_count=0"
assert_contains "$PACKET/restore-proof/README.md" "TCFS Git Repo Fresh-Tree Restore Proof"
test -f "$PACKET/restore-proof/reconcile-dry-run.log"
test -f "$PACKET/restore-proof/reconcile-execute.log"
test -L "$RESTORE/readme-link"

CUSTOM_RESTORE="$TMPDIR/restored tree custom"
CUSTOM_RESTORE_DIR="$PACKET/restore-proof-custom"
FAKE_RESTORE_SOURCE="$SHADOW" bash "$SCRIPT" \
  --evidence-dir "$PACKET" \
  --restore-root "$CUSTOM_RESTORE" \
  --restore-dir "$CUSTOM_RESTORE_DIR" \
  --config "$CONFIG" \
  --state "$TMPDIR/restore-state-custom.json" \
  >"$TMPDIR/custom-dir.out"

assert_contains "$TMPDIR/custom-dir.out" "restore proof status: passed"
assert_contains "$CUSTOM_RESTORE_DIR/restore-proof.env" "status=passed"
test -f "$CUSTOM_RESTORE_DIR/reconcile-execute.log"

REQUIRE_RESTORE="$TMPDIR/restored tree require"
assert_fails_contains \
  "restore proof status: failed" \
  env FAKE_RESTORE_SOURCE="$SHADOW" bash "$SCRIPT" \
    --evidence-dir "$PACKET" \
    --restore-root "$REQUIRE_RESTORE" \
    --config "$CONFIG" \
    --state "$TMPDIR/restore-state-require.json" \
    --require-empty-dirs

BROKEN_RESTORE="$TMPDIR/restored tree broken"
BROKEN_SOURCE="$TMPDIR/broken source"
mkdir -p "$BROKEN_SOURCE/src"
cp -a "$SHADOW/README.md" "$BROKEN_SOURCE/README.md"
printf 'changed\n' >"$BROKEN_SOURCE/src/main.rs"
ln -s README.md "$BROKEN_SOURCE/readme-link"
assert_fails_contains \
  "regular file hash manifest mismatch" \
  env FAKE_RESTORE_SOURCE="$BROKEN_SOURCE" bash "$SCRIPT" \
    --evidence-dir "$PACKET" \
    --restore-root "$BROKEN_RESTORE" \
    --config "$CONFIG" \
    --state "$TMPDIR/restore-state-broken.json"

NONEMPTY_RESTORE="$TMPDIR/nonempty"
mkdir -p "$NONEMPTY_RESTORE"
touch "$NONEMPTY_RESTORE/existing"
assert_fails_contains \
  "restore root must be empty" \
  env FAKE_RESTORE_SOURCE="$SHADOW" bash "$SCRIPT" \
    --evidence-dir "$PACKET" \
    --config "$CONFIG" \
    --restore-root "$NONEMPTY_RESTORE"

printf 'git repo restore proof tests passed\n'
