#!/usr/bin/env bash
#
# Regression tests for tcfs-symlink-package-probe.sh.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$REPO_ROOT/scripts/tcfs-symlink-package-probe.sh"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-symlink-package-probe-test.XXXXXX")"
trap 'rm -rf "$TMPDIR"' EXIT

assert_contains() {
  local file="$1"
  local expected="$2"

  if ! grep -Fq -- "$expected" "$file"; then
    printf 'expected to find %s in %s\n' "$expected" "$file" >&2
    printf '%s\n' '--- file ---' >&2
    cat "$file" >&2
    exit 1
  fi
}

make_fake_tcfs() {
  local path="$1"
  local version="$2"
  local mode="$3"

  cat >"$path" <<EOF
#!/usr/bin/env bash
set -euo pipefail
if [[ "\${1:-}" == "--version" ]]; then
  printf '%s\n' "$version"
  exit 0
fi
case "$mode" in
  preserved)
    printf 'uploaded symlink path=/tmp/source/link.txt target=target.txt\n' >&2
    printf 'Push complete:\n  uploaded: 2 files\n'
    ;;
  skipped)
    printf 'skipping symlink (follow_symlinks=false) path=/tmp/source/link.txt target=target.txt\n' >&2
    printf 'Push complete:\n  uploaded: 1 files\n'
    ;;
  failed)
    printf 'simulated push failure\n' >&2
    exit 42
    ;;
  *)
    printf 'unknown fake mode\n' >&2
    exit 2
    ;;
esac
EOF
  chmod +x "$path"
}

PRESERVED="$TMPDIR/tcfs-preserved"
SKIPPED="$TMPDIR/tcfs-skipped"
FAILED="$TMPDIR/tcfs-failed"
make_fake_tcfs "$PRESERVED" "tcfs preserved-test" preserved
make_fake_tcfs "$SKIPPED" "tcfs skipped-test" skipped
make_fake_tcfs "$FAILED" "tcfs failed-test" failed

EVIDENCE="$TMPDIR/evidence"
OUT="$TMPDIR/out.txt"
bash "$SCRIPT" \
  --endpoint http://example.invalid:8333 \
  --bucket tcfs-test \
  --prefix-base package-probe-test \
  --evidence-dir "$EVIDENCE" \
  --candidate preserved="$PRESERVED" \
  --candidate skipped="$SKIPPED" \
  --candidate failed="$FAILED" \
  >"$OUT"

assert_contains "$OUT" "overall status: blocked"
assert_contains "$EVIDENCE/result.env" "candidate_count=3"
assert_contains "$EVIDENCE/result.env" "candidate_1_symlink_result=preserved"
assert_contains "$EVIDENCE/result.env" "candidate_2_symlink_result=skipped"
assert_contains "$EVIDENCE/result.env" "candidate_3_symlink_result=push_failed"
assert_contains "$EVIDENCE/result.env" "overall_status=blocked"
assert_contains "$EVIDENCE/README.md" "TCFS Symlink Package Probe"
assert_contains "$EVIDENCE/README.md" "\`preserved\`: \`preserved\`"
assert_contains "$EVIDENCE/preserved.toml" "sync_symlinks = true"
assert_contains "$EVIDENCE/fixture.tsv" $'link.txt\ttarget.txt'

STRICT_OUT="$TMPDIR/strict.out"
STRICT_ERR="$TMPDIR/strict.err"
if bash "$SCRIPT" \
  --strict \
  --endpoint http://example.invalid:8333 \
  --bucket tcfs-test \
  --prefix-base package-probe-strict \
  --evidence-dir "$TMPDIR/strict-evidence" \
  --candidate skipped="$SKIPPED" \
  >"$STRICT_OUT" 2>"$STRICT_ERR"; then
  printf 'expected strict probe to fail when symlink is skipped\n' >&2
  exit 1
fi
assert_contains "$TMPDIR/strict-evidence/result.env" "overall_status=blocked"

PASS_EVIDENCE="$TMPDIR/pass-evidence"
bash "$SCRIPT" \
  --strict \
  --endpoint http://example.invalid:8333 \
  --bucket tcfs-test \
  --prefix-base package-probe-pass \
  --evidence-dir "$PASS_EVIDENCE" \
  --candidate preserved="$PRESERVED" \
  >"$TMPDIR/pass.out"
assert_contains "$PASS_EVIDENCE/result.env" "overall_status=passed"

printf 'tcfs symlink package probe tests passed\n'
