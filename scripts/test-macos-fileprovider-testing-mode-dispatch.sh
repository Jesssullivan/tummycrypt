#!/usr/bin/env bash
#
# Regression tests for the hosted FileProvider testing-mode dispatch helper.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="${REPO_ROOT}/scripts/macos-fileprovider-testing-mode-dispatch.sh"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-testing-mode-dispatch-test.XXXXXX")"
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

assert_not_contains() {
  local file="$1"
  local unexpected="$2"

  if grep -Fq -- "$unexpected" "$file"; then
    printf 'did not expect to find %s in %s\n' "$unexpected" "$file" >&2
    printf '%s\n' '--- output ---' >&2
    cat "$file" >&2
    exit 1
  fi
}

assert_fails_contains() {
  local expected="$1"
  shift

  local out="${TMPDIR}/failure.out"
  local err="${TMPDIR}/failure.err"

  if "$@" >"$out" 2>"$err"; then
    printf 'expected command to fail: %s\n' "$*" >&2
    exit 1
  fi

  cat "$out" "$err" >"${TMPDIR}/failure.combined"
  assert_contains "${TMPDIR}/failure.combined" "$expected"
}

bash -n "$SCRIPT"

DRY_RUN_OUT="${TMPDIR}/dry-run.out"
bash "$SCRIPT" \
  --dry-run \
  --tag v9.9.9 \
  --repo owner/repo \
  --ref main \
  >"$DRY_RUN_OUT"
assert_contains "$DRY_RUN_OUT" "gh workflow run \"macos-fileprovider-testing-mode-pkg.yml\" --repo \"owner/repo\" --ref \"main\" -f tag=\"v9.9.9\""
assert_contains "$DRY_RUN_OUT" "-f package_artifact_run_id=\"<testing-mode-package-run-id>\""
assert_contains "$DRY_RUN_OUT" "-f fileprovider_testing_mode=true"

assert_fails_contains \
  "tag must start with 'v'" \
  bash "$SCRIPT" --dry-run --tag 9.9.9

FAKE_BIN="${TMPDIR}/fake-bin"
mkdir -p "$FAKE_BIN"
cat >"$FAKE_BIN/gh" <<'EOF'
#!/usr/bin/env bash
printf 'gh' >>"$TCFS_FAKE_GH_LOG"
printf ' %q' "$@" >>"$TCFS_FAKE_GH_LOG"
printf '\n' >>"$TCFS_FAKE_GH_LOG"

case "${1:-} ${2:-}" in
  "secret list")
    printf 'TCFS_HOST_TESTING_MODE_PROVISIONING_PROFILE_BASE64\n'
    ;;
  "workflow view")
    ;;
  "workflow run")
    ;;
  "run list")
    case "$*" in
      *macos-fileprovider-testing-mode-pkg.yml*)
        printf '123456\n'
        ;;
      *macos-postinstall-smoke.yml*)
        printf '654321\n'
        ;;
      *)
        exit 1
        ;;
    esac
    ;;
  "run watch")
    ;;
  *)
    exit 1
    ;;
esac
EOF
chmod +x "$FAKE_BIN/gh"

FAKE_LOG="${TMPDIR}/gh.log"
PATH="$FAKE_BIN:$PATH" \
TCFS_FAKE_GH_LOG="$FAKE_LOG" \
bash "$SCRIPT" \
  --tag v1.2.3 \
  --repo owner/repo \
  --ref main \
  --no-watch \
  >"${TMPDIR}/no-watch.out" \
  2>"${TMPDIR}/no-watch.err"

assert_contains "$FAKE_LOG" "gh secret list --repo owner/repo --json name --jq"
assert_contains "$FAKE_LOG" "gh workflow run macos-fileprovider-testing-mode-pkg.yml --repo owner/repo --ref main -f tag=v1.2.3"
assert_contains "$FAKE_LOG" "gh run list --repo owner/repo --workflow macos-fileprovider-testing-mode-pkg.yml"
assert_not_contains "$FAKE_LOG" "macos-postinstall-smoke.yml --repo owner/repo --ref main"
assert_contains "${TMPDIR}/no-watch.err" "Package run dispatched. After it succeeds, rerun with --package-run-id 123456"

FAKE_LOG="${TMPDIR}/gh-existing.log"
PATH="$FAKE_BIN:$PATH" \
TCFS_FAKE_GH_LOG="$FAKE_LOG" \
bash "$SCRIPT" \
  --tag v1.2.3 \
  --repo owner/repo \
  --ref main \
  --package-run-id 123456 \
  --no-watch \
  >"${TMPDIR}/existing.out" \
  2>"${TMPDIR}/existing.err"

assert_not_contains "$FAKE_LOG" "gh secret list"
assert_not_contains "$FAKE_LOG" "macos-fileprovider-testing-mode-pkg.yml --repo owner/repo --ref main"
assert_contains "$FAKE_LOG" "gh workflow run macos-postinstall-smoke.yml --repo owner/repo --ref main -f tag=v1.2.3 -f package_artifact_run_id=123456"
assert_contains "${TMPDIR}/existing.err" "Post-install smoke dispatched. Watch with: gh run watch 654321 --repo owner/repo --exit-status"

echo "macOS FileProvider testing-mode dispatch tests passed"
