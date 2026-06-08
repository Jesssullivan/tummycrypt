#!/usr/bin/env bash
#
# Regression tests for git-roam-daily-driver-harness.sh.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$REPO_ROOT/scripts/git-roam-daily-driver-harness.sh"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-git-roam-test.XXXXXX")"
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

FIXTURE="$TMPDIR/repo"
AGENT="$TMPDIR/agent"
EVIDENCE="$TMPDIR/evidence"
mkdir -p "$FIXTURE/src" "$AGENT/project-one"

git -C "$FIXTURE" init -q
git -C "$FIXTURE" config user.name "TCFS Test"
git -C "$FIXTURE" config user.email "tcfs-test@example.invalid"
git -C "$FIXTURE" config commit.gpgsign false
printf '# fixture\n' >"$FIXTURE/README.md"
printf 'fn main() {}\n' >"$FIXTURE/src/main.rs"
git -C "$FIXTURE" add README.md src/main.rs
git -C "$FIXTURE" commit -q -m "initial fixture"

printf 'changed\n' >>"$FIXTURE/README.md"
printf 'new file\n' >"$FIXTURE/src/new.txt"
mkdir -p "$FIXTURE/node_modules/pkg"
printf 'generated\n' >"$FIXTURE/node_modules/pkg/generated.js"
printf 'secret\n' >"$FIXTURE/.env.local"
printf 'session line\n' >"$AGENT/project-one/session.jsonl"
printf 'db\n' >"$AGENT/project-one/state.sqlite"

OUT="$TMPDIR/out.txt"
bash "$SCRIPT" \
  --repo "$FIXTURE" \
  --agent-root "$AGENT" \
  --name ci-templates \
  --origin-host neo \
  --continuation-host honey \
  --third-host bumble \
  --remote-prefix git/ci-templates \
  --evidence-dir "$EVIDENCE" \
  >"$OUT"

assert_contains "$OUT" "git roam daily-driver evidence:"
assert_contains "$EVIDENCE/source.env" "status=plan-only"
assert_contains "$EVIDENCE/source.env" "name=ci-templates"
assert_contains "$EVIDENCE/source.env" "origin_host=neo"
assert_contains "$EVIDENCE/source.env" "continuation_host=honey"
assert_contains "$EVIDENCE/source.env" "third_host=bumble"
assert_contains "$EVIDENCE/source.env" "remote_prefix=git/ci-templates"
assert_contains "$EVIDENCE/source.env" "tcfs_mutation=0"
assert_contains "$EVIDENCE/source.env" "daily_driver_claim=0"
assert_contains "$EVIDENCE/git-source.txt" "##"
assert_contains "$EVIDENCE/git-source.txt" "README.md"
assert_contains "$EVIDENCE/tree-source.sha256" "./README.md"
assert_contains "$EVIDENCE/tree-source.sha256" "./src/new.txt"
assert_contains "$EVIDENCE/agent-source.sha256" "./project-one/session.jsonl"
assert_contains "$EVIDENCE/policy-deny.txt" "secret-deny"
assert_contains "$EVIDENCE/policy-deny.txt" ".env.local"
assert_contains "$EVIDENCE/policy-deny.txt" "generated-deny"
assert_contains "$EVIDENCE/policy-deny.txt" "node_modules"
assert_contains "$EVIDENCE/policy-deny.txt" "live-db-deny"
assert_contains "$EVIDENCE/gates.env" "r1_single_origin_dirty_wip=pending-live"
assert_contains "$EVIDENCE/result.env" "daily_driver_git_claim=0"
assert_contains "$EVIDENCE/README.md" "Status: plan-only."

NOT_GIT="$TMPDIR/not-git"
mkdir -p "$NOT_GIT"
assert_fails_contains \
  "repo is not a git worktree" \
  bash "$SCRIPT" --repo "$NOT_GIT" --evidence-dir "$TMPDIR/not-git-evidence"

printf 'git roam daily-driver harness tests passed\n'
