#!/usr/bin/env bash
#
# Regression tests for the lab FileProvider testing-mode dispatch helper.
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
assert_contains "$DRY_RUN_OUT" "gh workflow run \"macos-fileprovider-testing-mode-pkg.yml\" --repo \"owner/repo\" --ref \"main\""
assert_contains "$DRY_RUN_OUT" "gh api --paginate \"repos/owner/repo/actions/runners\""
assert_contains "$DRY_RUN_OUT" "-f tag=\"v9.9.9\""
assert_contains "$DRY_RUN_OUT" "-f runner_label=\"petting-zoo-mini\""
assert_contains "$DRY_RUN_OUT" "grep -Fx \"tcfs-9.9.9-macos-aarch64.tar.gz\""
assert_contains "$DRY_RUN_OUT" "gh api \"repos/owner/repo/actions/runs/<testing-mode-package-run-id>/artifacts\""
assert_contains "$DRY_RUN_OUT" "grep -Fx \"dist-testing-mode-pkg\""
assert_contains "$DRY_RUN_OUT" "-f package_artifact_run_id=\"<testing-mode-package-run-id>\""
assert_contains "$DRY_RUN_OUT" "-f fileprovider_testing_mode=true"
assert_contains "$DRY_RUN_OUT" "gh run watch \"<postinstall-smoke-run-id>\""
assert_not_contains "$DRY_RUN_OUT" "gh secret list"

DRY_RUN_NO_WATCH_OUT="${TMPDIR}/dry-run-no-watch.out"
bash "$SCRIPT" \
  --dry-run \
  --tag v9.9.9 \
  --repo owner/repo \
  --ref main \
  --no-watch \
  >"$DRY_RUN_NO_WATCH_OUT"
assert_not_contains "$DRY_RUN_NO_WATCH_OUT" "gh secret list"
assert_contains "$DRY_RUN_NO_WATCH_OUT" "gh api --paginate \"repos/owner/repo/actions/runners\""
assert_contains "$DRY_RUN_NO_WATCH_OUT" "gh release view"
assert_contains "$DRY_RUN_NO_WATCH_OUT" "gh workflow run \"macos-fileprovider-testing-mode-pkg.yml\""
assert_not_contains "$DRY_RUN_NO_WATCH_OUT" "actions/runs/<testing-mode-package-run-id>/artifacts"
assert_not_contains "$DRY_RUN_NO_WATCH_OUT" "macos-postinstall-smoke.yml"
assert_not_contains "$DRY_RUN_NO_WATCH_OUT" "gh run watch"
assert_contains "$DRY_RUN_NO_WATCH_OUT" "rerun with --package-run-id <testing-mode-package-run-id>"

DRY_RUN_SKIP_SECRET_OUT="${TMPDIR}/dry-run-skip-secret.out"
bash "$SCRIPT" \
  --dry-run \
  --tag v9.9.9 \
  --repo owner/repo \
  --ref main \
  --skip-secret-check \
  >"$DRY_RUN_SKIP_SECRET_OUT"
assert_not_contains "$DRY_RUN_SKIP_SECRET_OUT" "gh secret list"
assert_contains "$DRY_RUN_SKIP_SECRET_OUT" "gh release view"
assert_contains "$DRY_RUN_SKIP_SECRET_OUT" "macos-fileprovider-testing-mode-pkg.yml"

DRY_RUN_EXISTING_OUT="${TMPDIR}/dry-run-existing.out"
bash "$SCRIPT" \
  --dry-run \
  --tag v9.9.9 \
  --repo owner/repo \
  --ref main \
  --package-run-id 123456 \
  >"$DRY_RUN_EXISTING_OUT"
assert_not_contains "$DRY_RUN_EXISTING_OUT" "gh secret list"
assert_not_contains "$DRY_RUN_EXISTING_OUT" "gh release view"
assert_not_contains "$DRY_RUN_EXISTING_OUT" "macos-fileprovider-testing-mode-pkg.yml"
assert_contains "$DRY_RUN_EXISTING_OUT" "gh api \"repos/owner/repo/actions/runs/123456/artifacts\""
assert_contains "$DRY_RUN_EXISTING_OUT" "grep -Fx \"dist-testing-mode-pkg\""
assert_contains "$DRY_RUN_EXISTING_OUT" "-f package_artifact_run_id=\"123456\""
assert_contains "$DRY_RUN_EXISTING_OUT" "-f fileprovider_testing_mode=true"
assert_contains "$DRY_RUN_EXISTING_OUT" "-f runner_label=\"petting-zoo-mini\""
assert_contains "$DRY_RUN_EXISTING_OUT" "gh run watch \"<postinstall-smoke-run-id>\""

DRY_RUN_EXISTING_NO_WATCH_OUT="${TMPDIR}/dry-run-existing-no-watch.out"
bash "$SCRIPT" \
  --dry-run \
  --tag v9.9.9 \
  --repo owner/repo \
  --ref main \
  --package-run-id 123456 \
  --no-watch \
  >"$DRY_RUN_EXISTING_NO_WATCH_OUT"
assert_not_contains "$DRY_RUN_EXISTING_NO_WATCH_OUT" "gh secret list"
assert_not_contains "$DRY_RUN_EXISTING_NO_WATCH_OUT" "gh release view"
assert_not_contains "$DRY_RUN_EXISTING_NO_WATCH_OUT" "macos-fileprovider-testing-mode-pkg.yml"
assert_contains "$DRY_RUN_EXISTING_NO_WATCH_OUT" "gh api \"repos/owner/repo/actions/runs/123456/artifacts\""
assert_contains "$DRY_RUN_EXISTING_NO_WATCH_OUT" "grep -Fx \"dist-testing-mode-pkg\""
assert_contains "$DRY_RUN_EXISTING_NO_WATCH_OUT" "-f package_artifact_run_id=\"123456\""
assert_contains "$DRY_RUN_EXISTING_NO_WATCH_OUT" "Post-install smoke dispatched"

assert_fails_contains \
  "tag must start with 'v'" \
  bash "$SCRIPT" --dry-run --tag 9.9.9

assert_fails_contains \
  "FileProvider testing-mode requires a registered self-hosted Mac runner label" \
  bash "$SCRIPT" --dry-run --tag v9.9.9 --runner-label macos-15

FAKE_BIN="${TMPDIR}/fake-bin"
mkdir -p "$FAKE_BIN"
cat >"$FAKE_BIN/gh" <<'EOF'
#!/usr/bin/env bash
printf 'gh' >>"$TCFS_FAKE_GH_LOG"
printf ' %q' "$@" >>"$TCFS_FAKE_GH_LOG"
printf '\n' >>"$TCFS_FAKE_GH_LOG"

case "${1:-} ${2:-}" in
  "api --paginate")
    if [[ "${TCFS_FAKE_NO_RUNNER:-0}" == "1" ]]; then
      exit 0
    fi
    printf 'petting-zoo-mini\tmacOS\tonline\tself-hosted,macOS,ARM64,petting-zoo-mini\n'
    ;;
  "workflow view")
    ;;
  "workflow run")
    ;;
  "release view")
    if [[ "${TCFS_FAKE_MISSING_RELEASE_ASSET:-0}" == "1" ]]; then
      exit 0
    fi
    printf 'tcfs-1.2.3-macos-aarch64.tar.gz\n'
    ;;
  "run list")
    case "$*" in
      *macos-fileprovider-testing-mode-pkg.yml*)
        package_counter="$TCFS_FAKE_STATE/package-counter"
        package_count="$(cat "$package_counter" 2>/dev/null || printf '0')"
        package_count="$((package_count + 1))"
        printf '%s\n' "$package_count" >"$package_counter"
        if [[ "$package_count" == "1" ]]; then
          exit 0
        fi
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
  "api repos/owner/repo/actions/runs/123456/artifacts")
    if [[ "${TCFS_FAKE_MISSING_ARTIFACT:-0}" == "1" ]]; then
      exit 0
    fi
    printf 'dist-testing-mode-pkg\n'
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
TCFS_FAKE_STATE="$TMPDIR" \
TCFS_GH_RUN_ID_POLL_SECONDS=0 \
bash "$SCRIPT" \
  --tag v1.2.3 \
  --repo owner/repo \
  --ref main \
  --no-watch \
  >"${TMPDIR}/no-watch.out" \
  2>"${TMPDIR}/no-watch.err"

assert_not_contains "$FAKE_LOG" "gh secret list"
assert_contains "$FAKE_LOG" "gh release view v1.2.3 --repo owner/repo --json"
assert_contains "$FAKE_LOG" "gh api --paginate repos/owner/repo/actions/runners --jq"
assert_contains "$FAKE_LOG" "gh workflow run macos-fileprovider-testing-mode-pkg.yml --repo owner/repo --ref main -f tag=v1.2.3 -f runner_label=petting-zoo-mini"
assert_contains "$FAKE_LOG" "gh run list --repo owner/repo --workflow macos-fileprovider-testing-mode-pkg.yml"
assert_contains "${TMPDIR}/no-watch.err" "Waiting for macos-fileprovider-testing-mode-pkg.yml run to appear (1/10)"
assert_not_contains "$FAKE_LOG" "macos-postinstall-smoke.yml --repo owner/repo --ref main"
assert_contains "${TMPDIR}/no-watch.err" "Package run dispatched. After it succeeds, rerun with --package-run-id 123456"

FAKE_LOG="${TMPDIR}/gh-no-runner.log"
assert_fails_contains \
  "GitHub sees no self-hosted runners for owner/repo" \
  env PATH="$FAKE_BIN:$PATH" \
    TCFS_FAKE_GH_LOG="$FAKE_LOG" \
    TCFS_FAKE_STATE="$TMPDIR" \
    TCFS_FAKE_NO_RUNNER=1 \
    TCFS_GH_RUN_ID_POLL_SECONDS=0 \
    bash "$SCRIPT" \
      --tag v1.2.3 \
      --repo owner/repo \
      --ref main \
      --no-watch

FAKE_LOG="${TMPDIR}/gh-no-runner-skip.log"
PATH="$FAKE_BIN:$PATH" \
TCFS_FAKE_GH_LOG="$FAKE_LOG" \
TCFS_FAKE_STATE="$TMPDIR" \
TCFS_FAKE_NO_RUNNER=1 \
TCFS_GH_RUN_ID_POLL_SECONDS=0 \
bash "$SCRIPT" \
  --tag v1.2.3 \
  --repo owner/repo \
  --ref main \
  --no-watch \
  --skip-runner-check \
  >"${TMPDIR}/no-runner-skip.out" \
  2>"${TMPDIR}/no-runner-skip.err"
assert_contains "${TMPDIR}/no-runner-skip.err" "Skipping runner visibility check for petting-zoo-mini"
assert_contains "$FAKE_LOG" "gh workflow run macos-fileprovider-testing-mode-pkg.yml --repo owner/repo --ref main"

FAKE_LOG="${TMPDIR}/gh-missing-release-asset.log"
assert_fails_contains \
  "release v1.2.3 does not expose required asset tcfs-1.2.3-macos-aarch64.tar.gz" \
  env PATH="$FAKE_BIN:$PATH" \
    TCFS_FAKE_GH_LOG="$FAKE_LOG" \
    TCFS_FAKE_STATE="$TMPDIR" \
    TCFS_FAKE_MISSING_RELEASE_ASSET=1 \
    TCFS_GH_RUN_ID_POLL_SECONDS=0 \
    bash "$SCRIPT" \
      --tag v1.2.3 \
      --repo owner/repo \
      --ref main \
      --no-watch

FAKE_LOG="${TMPDIR}/gh-existing.log"
PATH="$FAKE_BIN:$PATH" \
TCFS_FAKE_GH_LOG="$FAKE_LOG" \
TCFS_FAKE_STATE="$TMPDIR" \
TCFS_GH_RUN_ID_POLL_SECONDS=0 \
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
assert_not_contains "$FAKE_LOG" "gh release view"
assert_contains "$FAKE_LOG" "gh api repos/owner/repo/actions/runs/123456/artifacts --jq"
assert_contains "$FAKE_LOG" "gh workflow run macos-postinstall-smoke.yml --repo owner/repo --ref main -f tag=v1.2.3 -f package_artifact_run_id=123456"
assert_contains "$FAKE_LOG" "-f runner_label=petting-zoo-mini"
assert_contains "${TMPDIR}/existing.err" "Post-install smoke dispatched. Watch with: gh run watch 654321 --repo owner/repo --exit-status"

FAKE_LOG="${TMPDIR}/gh-missing-artifact.log"
assert_fails_contains \
  "run 123456 does not expose a non-expired dist-testing-mode-pkg artifact" \
  env PATH="$FAKE_BIN:$PATH" \
    TCFS_FAKE_GH_LOG="$FAKE_LOG" \
    TCFS_FAKE_STATE="$TMPDIR" \
    TCFS_FAKE_MISSING_ARTIFACT=1 \
    TCFS_GH_RUN_ID_POLL_SECONDS=0 \
    bash "$SCRIPT" \
      --tag v1.2.3 \
      --repo owner/repo \
      --ref main \
      --package-run-id 123456 \
      --no-watch

echo "macOS FileProvider testing-mode dispatch tests passed"
