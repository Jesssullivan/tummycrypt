#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$REPO_ROOT/scripts/storage-posture-canary-dispatch.sh"
WORKFLOW="$REPO_ROOT/.github/workflows/storage-posture-canary.yml"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-storage-dispatch-test.XXXXXX")"
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
  if "$@" >"$out" 2>&1; then
    printf 'expected command to fail: %s\n' "$*" >&2
    exit 1
  fi
  assert_contains "$out" "$expected"
}

cat >"$TMPDIR/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

printf 'gh %s\n' "$*" >> "${GH_FAKE_LOG:?}"

if [[ "${1:-}" == "secret" && "${2:-}" == "list" ]]; then
  env_name=""
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --env)
        env_name="$2"
        shift 2
        ;;
      *)
        shift
        ;;
    esac
  done

  if [[ "$env_name" == "good-env" ]]; then
    cat <<'SECRETS'
TCFS_SMOKE_S3_ENDPOINT	2026-05-19T00:00:00Z
TCFS_SMOKE_S3_BUCKET	2026-05-19T00:00:00Z
TCFS_SMOKE_S3_ACCESS_KEY_ID	2026-05-19T00:00:00Z
TCFS_SMOKE_S3_SECRET_ACCESS_KEY	2026-05-19T00:00:00Z
SECRETS
  fi
  exit 0
fi

if [[ "${1:-}" == "api" ]]; then
  printf 'linux-1\tonline\tself-hosted,private-linux\n'
  exit 0
fi

if [[ "${1:-}" == "workflow" && "${2:-}" == "run" ]]; then
  printf 'dispatch-ok\n'
  exit 0
fi

printf 'unexpected gh invocation: %s\n' "$*" >&2
exit 99
EOF
chmod +x "$TMPDIR/gh"

export PATH="$TMPDIR:$PATH"
export GH_FAKE_LOG="$TMPDIR/gh.log"
: >"$GH_FAKE_LOG"

bash -n "$SCRIPT"
assert_contains "$WORKFLOW" "require_https=true requires scope_deny_prefix"
assert_contains "$WORKFLOW" "production posture packet proves scoped-credential denial"
assert_contains "$WORKFLOW" "require_https production posture runs must include scope-deny proof"
assert_contains "$WORKFLOW" "scope-deny probe must fail with PermissionDenied"

DRY_RUN="$TMPDIR/dry-run.out"
bash "$SCRIPT" \
  --dry-run \
  --repo owner/repo \
  --environment good-env \
  --scope-deny-prefix gha/storage-posture-denied/test \
  --remote-prefix gha/storage-posture/test \
  >"$DRY_RUN" 2>&1
assert_contains "$DRY_RUN" "gh workflow run storage-posture-canary.yml"
assert_contains "$DRY_RUN" "-R owner/repo"
assert_contains "$DRY_RUN" "-f runner_label=ubuntu-24.04"
assert_contains "$DRY_RUN" "-f smoke_environment=good-env"
assert_contains "$DRY_RUN" "-f scope_deny_prefix=gha/storage-posture-denied/test"
assert_contains "$DRY_RUN" "-f remote_prefix=gha/storage-posture/test"
assert_contains "$DRY_RUN" "-f require_https=true"

assert_fails_contains \
  "GitHub environment 'missing-env' is missing required secrets" \
  bash "$SCRIPT" \
    --dry-run \
    --repo owner/repo \
    --environment missing-env

PRIVATE_RUNNER="$TMPDIR/private-runner.out"
bash "$SCRIPT" \
  --dry-run \
  --repo owner/repo \
  --environment good-env \
  --runner-label private-linux \
  >"$PRIVATE_RUNNER" 2>&1
assert_contains "$PRIVATE_RUNNER" "runner label 'private-linux' is currently online on runner 'linux-1'"
assert_contains "$PRIVATE_RUNNER" "-f runner_label=private-linux"

DISPATCH="$TMPDIR/dispatch.out"
bash "$SCRIPT" \
  --repo owner/repo \
  --environment good-env \
  --scope-deny-prefix gha/storage-posture-denied/test \
  >"$DISPATCH" 2>&1
assert_contains "$DISPATCH" "dispatch-ok"
assert_contains "$DISPATCH" "gh run list -R owner/repo --workflow storage-posture-canary.yml"

printf 'storage posture canary dispatch tests passed\n'
