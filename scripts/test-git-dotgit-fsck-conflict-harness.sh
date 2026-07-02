#!/usr/bin/env bash
#
# Regression tests for git-dotgit-fsck-conflict-harness.sh.
#
# These run the harness in its default local-fixture mode (no remote, no daemon,
# no fleet mutation) and assert the FACET 6 invariants:
#   - the throwaway canary repo is fsck-clean to start,
#   - the full-tree (.git-as-files) mirror stays fsck-clean and exact,
#   - the mid-write gate evidence records the index.lock skip contract,
#   - the conflict scenario produces corruption-risk evidence,
#   - safety guards reject real source trees and non-disposable remotes.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$REPO_ROOT/scripts/git-dotgit-fsck-conflict-harness.sh"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-dotgit-fsck-test.XXXXXX")"
trap 'rm -rf "$TMPDIR"' EXIT

fail() { printf 'FAIL: %s\n' "$1" >&2; exit 1; }

assert_contains() {
  local file="$1" expected="$2"
  grep -Fq -- "$expected" "$file" || {
    printf 'expected %s in %s\n--- output ---\n' "$expected" "$file" >&2
    cat "$file" >&2; exit 1; }
}

# bash -n / shellcheck parse sanity (mirrors the lazy:check lane).
bash -n "$SCRIPT" || fail "harness has a bash syntax error"
bash -n "${BASH_SOURCE[0]}" || fail "test wrapper has a bash syntax error"

# 1. Default local run succeeds and is fsck-clean end to end.
EVID="$TMPDIR/evid1"
"$SCRIPT" --work-dir "$TMPDIR/run1" --evidence-dir "$EVID" >"$TMPDIR/run1.out" 2>&1 \
  || { cat "$TMPDIR/run1.out" >&2; fail "default harness run exited non-zero"; }
[ -f "$EVID/baseline-fsck.txt" ] || fail "missing baseline-fsck.txt"
[ -f "$EVID/mirror-fsck.txt" ] || fail "missing mirror-fsck.txt"
[ -f "$EVID/conflict-scenario.txt" ] || fail "missing conflict-scenario.txt"
[ -f "$EVID/midwrite-gate.txt" ] || fail "missing midwrite-gate.txt"
assert_contains "$EVID/midwrite-gate.txt" "index.lock"
assert_contains "$TMPDIR/run1.out" "clean flip-flop invariant holds"

# 1b. The interleaved per-file .git conflict must surface a half-applied ref:
#     git fsck must report an invalid/dangling pointer. This is the FACET 6
#     corruption-risk row — the harness exists to keep this hazard visible.
assert_contains "$EVID/conflict-fsck.txt" "invalid sha1 pointer"
assert_contains "$EVID/conflict-scenario.txt" "CORRUPTION-RISK CONFIRMED"

# 2. The mirror must be byte-exact on HEAD and status (no half-applied refs).
diff -q "$EVID/baseline-head.txt" "$EVID/mirror-head.txt" >/dev/null \
  || fail "mirror HEAD diverged from baseline"
diff -q "$EVID/baseline-status.txt" "$EVID/mirror-status.txt" >/dev/null \
  || fail "mirror status diverged from baseline"

# 3. Safety guard: refuse a remote that does not look disposable.
if "$SCRIPT" --work-dir "$TMPDIR/run2" --run-push \
    --remote "seaweedfs://prod-host:8333/tcfs/fleet-root" \
    >"$TMPDIR/run2.out" 2>&1; then
  fail "harness accepted a non-disposable remote"
fi
assert_contains "$TMPDIR/run2.out" "refusing remote"

# 4. Safety guard: --run-push without --remote must fail closed.
if "$SCRIPT" --work-dir "$TMPDIR/run3" --run-push >"$TMPDIR/run3.out" 2>&1; then
  fail "harness accepted --run-push without --remote"
fi
assert_contains "$TMPDIR/run3.out" "requires --remote"

printf 'all git-dotgit-fsck-conflict-harness tests passed\n'
