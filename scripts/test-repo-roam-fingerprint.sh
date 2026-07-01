#!/usr/bin/env bash
#
# Regression tests for repo-roam-fingerprint.sh. Disposable /tmp repos only;
# never touches a real worktree.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$REPO_ROOT/scripts/repo-roam-fingerprint.sh"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-repo-roam-fp-test.XXXXXX")"
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

# --- self-test mode round-trips (deterministic + drift detection) ------------
SELFTEST_OUT="$TMPDIR/selftest.out"
bash "$SCRIPT" self-test "$TMPDIR/selftest-base" >"$SELFTEST_OUT" 2>&1
assert_contains "$SELFTEST_OUT" "self-test PASSED"
assert_contains "$SELFTEST_OUT" "dev-env-zero-diff=pass"

# --- seed-canary builds the expected dirty/in-progress state -----------------
REPO="$TMPDIR/canary"
bash "$SCRIPT" seed-canary "$REPO" >"$TMPDIR/seed.out"
assert_contains "$TMPDIR/seed.out" "seeded throwaway canary repo"
test -d "$REPO/.git"
test -L "$REPO/README.link"
test -x "$REPO/run.sh"
# feature branch checked out, stash present.
[[ "$(git -C "$REPO" rev-parse --abbrev-ref HEAD)" == "feature/in-progress" ]]
[[ "$(git -C "$REPO" stash list | wc -l | tr -d ' ')" == "1" ]]

# --- capture writes the full fingerprint surface -----------------------------
FP_A="$TMPDIR/fp-a"
bash "$SCRIPT" capture "$REPO" "$FP_A" >"$TMPDIR/capture-a.out"
assert_contains "$TMPDIR/capture-a.out" "captured fingerprint of"
for f in fingerprint.env status.txt head.env refs.txt branches.txt \
         diff-cached.sha256 diff-worktree.sha256 index-blobs.txt \
         untracked.txt stash-list.txt stash.env reflog.txt fsck.txt \
         fsck.env working-manifest.tsv; do
  test -f "$FP_A/$f" || { printf 'missing capture file: %s\n' "$f" >&2; exit 1; }
done
assert_contains "$FP_A/fingerprint.env" "fsck=clean"
assert_contains "$FP_A/fingerprint.env" "branch=feature/in-progress"
assert_contains "$FP_A/fingerprint.env" "stash_entries=1"
# symlink + exec mode recorded in the working manifest.
assert_contains "$FP_A/working-manifest.tsv" "README.link"
assert_contains "$FP_A/working-manifest.tsv" "symlink:README.md"
assert_contains "$FP_A/working-manifest.tsv" "run.sh"
# untracked file present.
assert_contains "$FP_A/untracked.txt" "NOTES.txt"

# --- deny-set posture: planted secrets are recorded DENIED, never hashed -----
secret_paths=(
  ".env"
  ".env.local"
  "service.env"
  ".credentials.json"
  "auth.json"
  ".netrc"
  ".pgpass"
  "logs_2.sqlite"
  "logs_2.sqlite3"
  "logs_2.sqlite-wal"
  "logs_2.sqlite-shm"
  "opencode.db"
  "opencode.db-wal"
  "opencode.db-shm"
  ".ssh/id_ed25519"
  ".gnupg/private-keys-v1.d/key"
  "sops-nix/secrets/example"
  "nested/secrets/token"
)
for secret_path in "${secret_paths[@]}"; do
  mkdir -p "$REPO/$(dirname "$secret_path")"
  printf 'SECRET=should-not-appear:%s\n' "$secret_path" >"$REPO/$secret_path"
  git -C "$REPO" add -f "$secret_path"
done
FP_DENY="$TMPDIR/fp-deny"
bash "$SCRIPT" capture "$REPO" "$FP_DENY" >/dev/null
for secret_path in "${secret_paths[@]}"; do
  assert_contains "$FP_DENY/working-manifest.tsv" "$secret_path"
  if ! awk -F '\t' -v path="$secret_path" '$1 == path && $2 == "DENIED" { found=1 } END { exit found ? 0 : 1 }' \
      "$FP_DENY/working-manifest.tsv"; then
    printf 'expected %s to be recorded as DENIED\n' "$secret_path" >&2
    cat "$FP_DENY/working-manifest.tsv" >&2
    exit 1
  fi
done
if grep -F 'should-not-appear' "$FP_DENY"/* >/dev/null 2>&1; then
  printf 'secret content leaked into fingerprint evidence\n' >&2
  exit 1
fi
# reset the planted secrets out of the way for the unchanged-compare below.
git -C "$REPO" rm -qr --cached -- "${secret_paths[@]}"
for secret_path in "${secret_paths[@]}"; do
  rm -f "$REPO/$secret_path"
done

# --- compare: identical re-capture passes ------------------------------------
FP_B="$TMPDIR/fp-b"
bash "$SCRIPT" capture "$REPO" "$FP_B" >/dev/null
CMP_OUT="$TMPDIR/compare.out"
bash "$SCRIPT" compare "$FP_A" "$FP_B" >"$CMP_OUT"
assert_contains "$CMP_OUT" "dev-env-zero-diff=pass"

# --- compare: a real change fails with a diff --------------------------------
printf 'drift\n' >>"$REPO/NOTES.txt"
FP_C="$TMPDIR/fp-c"
bash "$SCRIPT" capture "$REPO" "$FP_C" >/dev/null
assert_fails_contains \
  "dev-env-zero-diff" \
  bash "$SCRIPT" compare "$FP_A" "$FP_C"

# --- safety: seed-canary refuses HOME ----------------------------------------
assert_fails_contains \
  "refusing to seed canary at \$HOME" \
  env HOME="$TMPDIR/fakehome" bash "$SCRIPT" seed-canary "$TMPDIR/fakehome"

# --- safety: seed-canary refuses ~/git/<repo> --------------------------------
mkdir -p "$TMPDIR/fakehome2/git"
assert_fails_contains \
  "refusing to seed canary under ~/git" \
  env HOME="$TMPDIR/fakehome2" bash "$SCRIPT" seed-canary "$TMPDIR/fakehome2/git/somerepo"

# --- capture refuses a non-git dir -------------------------------------------
mkdir -p "$TMPDIR/not-git"
assert_fails_contains \
  "not a git worktree" \
  bash "$SCRIPT" capture "$TMPDIR/not-git" "$TMPDIR/fp-not-git"

printf 'repo-roam-fingerprint tests passed\n'
