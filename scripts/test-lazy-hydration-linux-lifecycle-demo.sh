#!/usr/bin/env bash
#
# Regression checks for the Linux lifecycle proof harness shape.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$REPO_ROOT/scripts/lazy-hydration-linux-lifecycle-demo.sh"
BASE_SCRIPT="$REPO_ROOT/scripts/lazy-hydration-linux-demo.sh"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-linux-lifecycle-test.XXXXXX")"
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

bash -n "$SCRIPT" "$BASE_SCRIPT"

HELP_OUT="$TMPDIR/help.out"
bash "$SCRIPT" --help >"$HELP_OUT"
assert_contains "$HELP_OUT" "write/edit through"
assert_contains "$HELP_OUT" "verify exact remote pullback"
assert_contains "$HELP_OUT" "recursive safe-unsync refusal/success"
assert_contains "$HELP_OUT" "--evidence-dir"

assert_contains "$BASE_SCRIPT" "[5/9] write and edit through mounted view"
assert_contains "$BASE_SCRIPT" "[8/9] prove recursive safe-unsync refuses dirty descendants"
assert_contains "$BASE_SCRIPT" "redacted-metadata.env"
assert_contains "$BASE_SCRIPT" "remote-prefix.txt"

printf 'Linux lifecycle proof helper tests passed\n'
