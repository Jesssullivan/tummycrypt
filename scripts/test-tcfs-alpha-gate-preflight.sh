#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$REPO_ROOT/scripts/tcfs-alpha-gate-preflight.sh"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-alpha-gate-preflight-test.XXXXXX")"
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

  case "$env_name" in
    storage-good)
      cat <<'SECRETS'
TCFS_SMOKE_S3_ENDPOINT	2026-05-20T00:00:00Z
TCFS_SMOKE_S3_BUCKET	2026-05-20T00:00:00Z
TCFS_SMOKE_S3_ACCESS_KEY_ID	2026-05-20T00:00:00Z
TCFS_SMOKE_S3_SECRET_ACCESS_KEY	2026-05-20T00:00:00Z
SECRETS
      ;;
    linux-good)
      cat <<'SECRETS'
TCFS_SMOKE_S3_ENDPOINT	2026-05-20T00:00:00Z
TCFS_SMOKE_S3_BUCKET	2026-05-20T00:00:00Z
TCFS_SMOKE_S3_ACCESS_KEY_ID	2026-05-20T00:00:00Z
TCFS_SMOKE_S3_SECRET_ACCESS_KEY	2026-05-20T00:00:00Z
TCFS_SMOKE_MASTER_KEY_B64	2026-05-20T00:00:00Z
SECRETS
      ;;
  esac
  exit 0
fi

if [[ "${1:-}" == "api" ]]; then
  printf 'linux-1\tonline\tself-hosted,linux-private\n'
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

READY="$TMPDIR/ready.out"
bash "$SCRIPT" \
  --repo owner/repo \
  --storage-environment storage-good \
  --linux-environment linux-good \
  --tag v1.2.3 \
  >"$READY"
assert_contains "$READY" "# TCFS Alpha Gate Preflight"
assert_contains "$READY" "- status: \`runnable\`"
assert_contains "$READY" "scripts/storage-posture-canary-dispatch.sh"
assert_contains "$READY" "gh workflow run linux-postinstall-smoke.yml"
assert_contains "$READY" "-f tag=v1.2.3"
assert_contains "$READY" "just neo-honey-smoke"

BLOCKED="$TMPDIR/blocked.out"
bash "$SCRIPT" \
  --repo owner/repo \
  --storage-environment storage-empty \
  --linux-environment linux-empty \
  >"$BLOCKED"
assert_contains "$BLOCKED" "missing_secrets"
assert_contains "$BLOCKED" "TCFS_SMOKE_MASTER_KEY_B64"

SELF_HOSTED="$TMPDIR/self-hosted.out"
bash "$SCRIPT" \
  --repo owner/repo \
  --storage-environment storage-good \
  --linux-environment linux-good \
  --linux-runner-label linux-private \
  >"$SELF_HOSTED"
assert_contains "$SELF_HOSTED" "runner: \`linux-private\` (online:linux-1)"

assert_fails_contains \
  "missing_secrets" \
  bash "$SCRIPT" \
    --strict \
    --repo owner/repo \
    --storage-environment storage-empty \
    --linux-environment linux-empty

printf 'tcfs alpha gate preflight tests passed\n'
