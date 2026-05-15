#!/usr/bin/env bash
#
# Regression tests for git-repo-canary.sh.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$REPO_ROOT/scripts/git-repo-canary.sh"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-git-repo-canary-test.XXXXXX")"
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

HOME_OK="$TMPDIR/home"
SOURCE="$TMPDIR/oauth-mux"
SHADOW="$TMPDIR/shadow"
EVIDENCE="$TMPDIR/git-repo-canary-oauth-mux-fixed"
STATE="$TMPDIR/state"
mkdir -p "$HOME_OK" "$SOURCE"

git -C "$SOURCE" init -q
git -C "$SOURCE" config user.name "TCFS Test"
git -C "$SOURCE" config user.email "tcfs-test@example.invalid"
git -C "$SOURCE" config commit.gpgsign false
mkdir -p "$SOURCE/src"
cat >"$SOURCE/README.md" <<'EOF'
# oauth-mux fixture
EOF
cat >"$SOURCE/src/main.rs" <<'EOF'
fn main() {}
EOF
git -C "$SOURCE" add README.md src/main.rs
git -C "$SOURCE" commit -q -m "initial fixture"
SOURCE_CANON="$(cd "$SOURCE" && pwd -P)"

OUT="$TMPDIR/positive.out"
HOME="$HOME_OK" bash "$SCRIPT" \
  --source "$SOURCE" \
  --name oauth-mux \
  --shadow-root "$SHADOW" \
  --evidence-dir "$EVIDENCE" \
  --state-dir "$STATE" \
  --remote seaweedfs://example.invalid/tcfs/git-repo-canary-test \
  --honey-host honey-test \
  --honey-smoke-max-depth 4 \
  --honey-smoke-timeout-secs 60 \
  >"$OUT"

assert_contains "$OUT" "git repo canary evidence:"
assert_contains "$OUT" "parity gate: full-project-parity-not-claimed"
assert_contains "$EVIDENCE/git-repo-canary-policy.env" "canary_name=oauth-mux"
assert_contains "$EVIDENCE/git-repo-canary-policy.env" "run_id=git-repo-canary-oauth-mux-fixed"
assert_contains "$EVIDENCE/git-repo-canary-policy.env" "source=$SOURCE_CANON"
assert_contains "$EVIDENCE/git-repo-canary-policy.env" "dirty_status_count=0"
assert_contains "$EVIDENCE/git-repo-canary-policy.env" "shadow_first=1"
assert_contains "$EVIDENCE/git-repo-canary-policy.env" "live_repo_mutation=0"
assert_contains "$EVIDENCE/git-repo-canary-policy.env" "finder_claim=0"
assert_contains "$EVIDENCE/git-repo-canary-policy.env" "full_home_claim=0"
assert_contains "$EVIDENCE/git-repo-canary-summary.md" "TCFS Git Repo Canary Summary"
assert_contains "$EVIDENCE/git-repo-canary-summary.md" "This packet does not mutate the live source repo."
assert_contains "$EVIDENCE/README.md" "TCFS Git Repo Canary Evidence"
assert_contains "$EVIDENCE/README.md" "Canary: \`oauth-mux\`"
assert_contains "$EVIDENCE/README.md" "It does not mutate the live"
assert_contains "$EVIDENCE/README.md" "full-project-parity-not-claimed"
assert_contains "$EVIDENCE/run-metadata.env" "source=$SOURCE_CANON"
assert_contains "$EVIDENCE/run-metadata.env" "honey_smoke_max_depth=4"
assert_contains "$EVIDENCE/run-metadata.env" "honey_smoke_timeout_secs=60"
assert_contains "$EVIDENCE/result.env" "status=plan-only"
assert_contains "$EVIDENCE/tcfs-linux-xr-shadow.toml" "sync_git_dirs = true"
assert_contains "$EVIDENCE/tcfs-git-repo-canary.toml" "sync_git_dirs = true"
test -f "$SHADOW/README.md"
test -f "$STATE/tcfs-linux-xr-shadow.toml"
test -f "$STATE/tcfs-git-repo-canary.toml"
test -f "$EVIDENCE/honey-git-repo-canary-commands.txt"

FAKE_TCFS="$TMPDIR/fake-tcfs"
cat >"$FAKE_TCFS" <<'EOF'
#!/usr/bin/env bash
if [[ "${1:-}" == "--version" ]]; then
  printf 'tcfs test\n'
  exit 0
fi
printf 'fake push failed\n' >&2
exit 42
EOF
chmod +x "$FAKE_TCFS"

FAIL_EVIDENCE="$TMPDIR/push-failure-evidence"
FAIL_OUT="$TMPDIR/push-failure.out"
FAIL_ERR="$TMPDIR/push-failure.err"
set +e
HOME="$HOME_OK" bash "$SCRIPT" \
  --source "$SOURCE" \
  --name oauth-mux \
  --shadow-root "$TMPDIR/push-failure-shadow" \
  --evidence-dir "$FAIL_EVIDENCE" \
  --remote seaweedfs://example.invalid/tcfs/git-repo-canary-push-failure \
  --tcfs-bin "$FAKE_TCFS" \
  --push \
  --run-honey \
  --honey-start-mount \
  --run-linux-lifecycle \
  >"$FAIL_OUT" 2>"$FAIL_ERR"
fail_rc=$?
set -e
if [[ "$fail_rc" -eq 0 ]]; then
  printf 'expected push failure canary to fail\n' >&2
  exit 1
fi
cat "$FAIL_OUT" "$FAIL_ERR" >"$TMPDIR/push-failure.combined"
assert_contains "$TMPDIR/push-failure.combined" "push failed"
assert_contains "$FAIL_EVIDENCE/README.md" "TCFS Git Repo Canary Evidence"
assert_contains "$FAIL_EVIDENCE/README.md" "Canary: \`oauth-mux\`"
assert_contains "$FAIL_EVIDENCE/parity-gates.env" "reason=shadow push failed"
assert_contains "$FAIL_EVIDENCE/parity-gates.env" "push_skipped_symlink_count=0"
assert_contains "$FAIL_EVIDENCE/run-metadata.env" "tcfs_command=$FAKE_TCFS"
assert_contains "$FAIL_EVIDENCE/run-metadata.env" "tcfs_version_status=ok"
assert_contains "$FAIL_EVIDENCE/run-metadata.env" "tcfs_version=tcfs test"
assert_contains "$FAIL_EVIDENCE/run-metadata.env" "tcfs_sha256_status=ok"
assert_contains "$FAIL_EVIDENCE/run-metadata.env" "honey_status=skipped-push-failed"
assert_contains "$FAIL_EVIDENCE/run-metadata.env" "linux_lifecycle_status=skipped-push-failed"

cat >>"$SOURCE/README.md" <<'EOF'
dirty change
EOF

assert_fails_contains \
  "source has dirty status" \
  env HOME="$HOME_OK" bash "$SCRIPT" \
    --source "$SOURCE" \
    --shadow-root "$TMPDIR/dirty-shadow" \
    --evidence-dir "$TMPDIR/dirty-evidence" \
    --remote seaweedfs://example.invalid/tcfs/git-repo-canary-dirty

DIRTY_EVIDENCE="$TMPDIR/dirty-allowed-evidence"
HOME="$HOME_OK" bash "$SCRIPT" \
  --source "$SOURCE" \
  --name dirty-ok \
  --shadow-root "$TMPDIR/dirty-allowed-shadow" \
  --evidence-dir "$DIRTY_EVIDENCE" \
  --remote seaweedfs://example.invalid/tcfs/git-repo-canary-dirty-ok \
  --allow-dirty-source \
  >"$TMPDIR/dirty-allowed.out"
assert_contains "$DIRTY_EVIDENCE/git-repo-canary-policy.env" "allow_dirty_source=1"
assert_contains "$DIRTY_EVIDENCE/git-repo-canary-policy.env" "dirty_status_count=1"

NOT_GIT="$TMPDIR/not-git"
mkdir -p "$NOT_GIT"
assert_fails_contains \
  "source is not a git worktree" \
  env HOME="$HOME_OK" bash "$SCRIPT" \
    --source "$NOT_GIT" \
    --shadow-root "$TMPDIR/not-git-shadow" \
    --evidence-dir "$TMPDIR/not-git-evidence" \
    --remote seaweedfs://example.invalid/tcfs/git-repo-canary-not-git

printf 'git repo canary tests passed\n'
