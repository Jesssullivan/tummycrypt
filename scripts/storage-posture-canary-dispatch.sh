#!/usr/bin/env bash
set -euo pipefail

REPO="Jesssullivan/tummycrypt"
REF="main"
RUNNER_LABEL="ubuntu-24.04"
SMOKE_ENVIRONMENT="tcfs-storage-prod-smoke"
REMOTE_PREFIX=""
SCOPE_DENY_PREFIX="gha/storage-posture-denied/$(date -u +%Y%m%dT%H%M%SZ)"
TIMEOUT_SECS="10"
REQUIRE_HTTPS="true"
DRY_RUN=0
SKIP_SECRET_CHECK=0

usage() {
  cat <<'EOF'
Usage: scripts/storage-posture-canary-dispatch.sh [options]

Dispatch the TIN-1546 production storage posture canary with the expected
HTTPS + scoped-credential denial inputs.

Options:
  --repo OWNER/REPO             GitHub repo (default: Jesssullivan/tummycrypt)
  --ref REF                     Git ref to dispatch (default: main)
  --runner-label LABEL          Runner label (default: ubuntu-24.04)
  --environment NAME            GitHub environment (default: tcfs-storage-prod-smoke)
  --remote-prefix PREFIX        Optional positive canary prefix; otherwise workflow default is used
  --scope-deny-prefix PREFIX    Outside-policy prefix that must be denied
  --timeout-secs SECONDS        Per-operation canary timeout (default: 10)
  --require-https true|false    Require HTTPS in workflow input (default: true)
  --skip-secret-check           Do not verify required environment secret names before dispatch
  --dry-run                     Print the gh command without dispatching
  -h, --help                    Show this help

Required environment secrets:
  TCFS_SMOKE_S3_ENDPOINT
  TCFS_SMOKE_S3_BUCKET
  TCFS_SMOKE_S3_ACCESS_KEY_ID
  TCFS_SMOKE_S3_SECRET_ACCESS_KEY

Optional environment secrets:
  TCFS_SMOKE_S3_REGION
  TCFS_SMOKE_S3_CA_CERT_PEM
EOF
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

log() {
  printf '%s\n' "$*" >&2
}

quote_cmd() {
  local quoted=()
  local arg
  for arg in "$@"; do
    quoted+=("$(printf '%q' "$arg")")
  done
  printf '%s\n' "${quoted[*]}"
}

require_value() {
  local flag="$1"
  local value="${2:-}"
  [[ -n "$value" ]] || die "$flag requires a value"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo)
      require_value "$1" "${2:-}"
      REPO="$2"
      shift 2
      ;;
    --ref)
      require_value "$1" "${2:-}"
      REF="$2"
      shift 2
      ;;
    --runner-label)
      require_value "$1" "${2:-}"
      RUNNER_LABEL="$2"
      shift 2
      ;;
    --environment)
      require_value "$1" "${2:-}"
      SMOKE_ENVIRONMENT="$2"
      shift 2
      ;;
    --remote-prefix)
      require_value "$1" "${2:-}"
      REMOTE_PREFIX="${2#/}"
      REMOTE_PREFIX="${REMOTE_PREFIX%/}"
      shift 2
      ;;
    --scope-deny-prefix)
      require_value "$1" "${2:-}"
      SCOPE_DENY_PREFIX="${2#/}"
      SCOPE_DENY_PREFIX="${SCOPE_DENY_PREFIX%/}"
      shift 2
      ;;
    --timeout-secs)
      require_value "$1" "${2:-}"
      TIMEOUT_SECS="$2"
      shift 2
      ;;
    --require-https)
      require_value "$1" "${2:-}"
      REQUIRE_HTTPS="$2"
      shift 2
      ;;
    --skip-secret-check)
      SKIP_SECRET_CHECK=1
      shift
      ;;
    --dry-run)
      DRY_RUN=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      die "unknown argument: $1"
      ;;
  esac
done

[[ "$TIMEOUT_SECS" =~ ^[1-9][0-9]*$ ]] || die "--timeout-secs must be a positive integer"
case "$REQUIRE_HTTPS" in
  true|false) ;;
  *) die "--require-https must be true or false" ;;
esac
[[ -n "$SCOPE_DENY_PREFIX" ]] || die "--scope-deny-prefix must not be empty for the production posture gate"

if ! command -v gh >/dev/null 2>&1; then
  die "gh CLI is required"
fi

if [[ "$SKIP_SECRET_CHECK" != "1" ]]; then
  secret_names="$(gh secret list -R "$REPO" --env "$SMOKE_ENVIRONMENT" 2>/dev/null | awk '{print $1}')"
  required=(
    TCFS_SMOKE_S3_ENDPOINT
    TCFS_SMOKE_S3_BUCKET
    TCFS_SMOKE_S3_ACCESS_KEY_ID
    TCFS_SMOKE_S3_SECRET_ACCESS_KEY
  )
  missing=()
  for required_name in "${required[@]}"; do
    if ! grep -qx "$required_name" <<<"$secret_names"; then
      missing+=("$required_name")
    fi
  done
  if (( ${#missing[@]} > 0 )); then
    die "GitHub environment '$SMOKE_ENVIRONMENT' is missing required secrets: ${missing[*]}"
  fi
fi

case "$RUNNER_LABEL" in
  ubuntu-*|macos-*|windows-*)
    ;;
  *)
    runner_match="$(
      gh api "repos/$REPO/actions/runners" --paginate \
        --jq '.runners[] | [.name, .status, ([.labels[].name] | join(","))] | @tsv' \
        | awk -F '\t' -v label="$RUNNER_LABEL" '
            $2 == "online" {
              split($3, labels, ",")
              for (i in labels) {
                if (labels[i] == label) {
                  print $1
                  exit
                }
              }
            }
          '
    )"
    [[ -n "$runner_match" ]] || die "no online self-hosted runner currently advertises label '$RUNNER_LABEL'"
    log "runner label '$RUNNER_LABEL' is currently online on runner '$runner_match'"
    ;;
esac

cmd=(
  gh workflow run storage-posture-canary.yml
  -R "$REPO"
  --ref "$REF"
  -f "runner_label=$RUNNER_LABEL"
  -f "smoke_environment=$SMOKE_ENVIRONMENT"
  -f "scope_deny_prefix=$SCOPE_DENY_PREFIX"
  -f "timeout_secs=$TIMEOUT_SECS"
  -f "require_https=$REQUIRE_HTTPS"
)

if [[ -n "$REMOTE_PREFIX" ]]; then
  cmd+=(-f "remote_prefix=$REMOTE_PREFIX")
fi

log "dispatching storage posture canary:"
log "  repo: $REPO"
log "  ref: $REF"
log "  runner: $RUNNER_LABEL"
log "  environment: $SMOKE_ENVIRONMENT"
log "  require_https: $REQUIRE_HTTPS"
log "  scope_deny_prefix: $SCOPE_DENY_PREFIX"
if [[ -n "$REMOTE_PREFIX" ]]; then
  log "  remote_prefix: $REMOTE_PREFIX"
else
  log "  remote_prefix: <workflow default>"
fi

if [[ "$DRY_RUN" == "1" ]]; then
  quote_cmd "${cmd[@]}"
  exit 0
fi

"${cmd[@]}"
log "dispatch requested; inspect the latest workflow_dispatch run with:"
log "  gh run list -R $REPO --workflow storage-posture-canary.yml --event workflow_dispatch --limit 5"
