#!/usr/bin/env bash
# shellcheck disable=SC2016 # Markdown backticks are literal in printf strings.
set -euo pipefail

REPO="Jesssullivan/tummycrypt"
STORAGE_ENVIRONMENT="tcfs-storage-prod-smoke"
LINUX_ENVIRONMENT="tcfs-linux-smoke"
STORAGE_RUNNER_LABEL="ubuntu-24.04"
LINUX_RUNNER_LABEL="ubuntu-24.04"
TAG=""
SCOPE_DENY_PREFIX="gha/storage-posture-denied/$(date -u +%Y%m%dT%H%M%SZ)"
REMOTE_PREFIX=""
LINUX_REMOTE_PREFIX=""
STRICT=0

usage() {
  cat <<'EOF'
Usage: scripts/tcfs-alpha-gate-preflight.sh [options]

Classify whether the TCFS alpha productionization gates are runnable from the
current GitHub/runner state. This is intentionally read-only: it checks
environment secret names and runner labels, then prints the next dispatch
commands or the exact blocker.

Options:
  --repo OWNER/REPO                 GitHub repo (default: Jesssullivan/tummycrypt)
  --storage-environment NAME        GitHub environment for TIN-1546 storage canary
                                    (default: tcfs-storage-prod-smoke)
  --linux-environment NAME          GitHub environment for TIN-1422 Linux smoke
                                    (default: tcfs-linux-smoke)
  --storage-runner-label LABEL      Runner label for storage posture canary
                                    (default: ubuntu-24.04)
  --linux-runner-label LABEL        Runner label for Linux postinstall smoke
                                    (default: ubuntu-24.04)
  --tag TAG                         Release tag for Linux package smoke
                                    (default: newest GitHub Release tag)
  --scope-deny-prefix PREFIX        Outside-policy prefix that storage canary
                                    must reject
  --remote-prefix PREFIX            Optional positive storage canary prefix
  --linux-remote-prefix PREFIX      Optional Linux smoke prefix override
  --strict                          Exit non-zero when any alpha gate is blocked
  -h, --help                        Show this help

Required storage secrets:
  TCFS_SMOKE_S3_ENDPOINT
  TCFS_SMOKE_S3_BUCKET
  TCFS_SMOKE_S3_ACCESS_KEY_ID
  TCFS_SMOKE_S3_SECRET_ACCESS_KEY

Required Linux smoke secrets:
  TCFS_SMOKE_S3_ENDPOINT
  TCFS_SMOKE_S3_BUCKET
  TCFS_SMOKE_S3_ACCESS_KEY_ID
  TCFS_SMOKE_S3_SECRET_ACCESS_KEY
  TCFS_SMOKE_MASTER_KEY_B64
EOF
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

require_value() {
  local flag="$1"
  local value="${2:-}"
  [[ -n "$value" ]] || die "$flag requires a value"
}

is_hosted_runner_label() {
  case "$1" in
    ubuntu-*|macos-*|windows-*) return 0 ;;
    *) return 1 ;;
  esac
}

quote_cmd() {
  local quoted=()
  local arg
  for arg in "$@"; do
    quoted+=("$(printf '%q' "$arg")")
  done
  printf '%s\n' "${quoted[*]}"
}

secret_names_for_environment() {
  local environment="$1"
  gh secret list -R "$REPO" --env "$environment" 2>/dev/null | awk '{print $1}'
}

missing_secret_names() {
  local environment="$1"
  shift
  local secret_names
  secret_names="$(secret_names_for_environment "$environment" || true)"

  local missing=()
  local name
  for name in "$@"; do
    if ! grep -qx "$name" <<<"$secret_names"; then
      missing+=("$name")
    fi
  done

  if (( ${#missing[@]} > 0 )); then
    printf '%s\n' "${missing[@]}"
  fi
}

self_hosted_runner_match() {
  local label="$1"
  gh api "repos/$REPO/actions/runners" --paginate \
    --jq '.runners[] | [.name, .status, ([.labels[].name] | join(","))] | @tsv' \
    | awk -F '\t' -v label="$label" '
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
}

latest_release_tag() {
  gh api "repos/$REPO/releases?per_page=1" --jq '.[0].tag_name // empty'
}

runner_status_line() {
  local label="$1"
  if is_hosted_runner_label "$label"; then
    printf 'hosted:%s\n' "$label"
    return
  fi

  local match
  match="$(self_hosted_runner_match "$label")"
  if [[ -n "$match" ]]; then
    printf 'online:%s\n' "$match"
  else
    printf 'missing:%s\n' "$label"
  fi
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo)
      require_value "$1" "${2:-}"
      REPO="$2"
      shift 2
      ;;
    --storage-environment)
      require_value "$1" "${2:-}"
      STORAGE_ENVIRONMENT="$2"
      shift 2
      ;;
    --linux-environment)
      require_value "$1" "${2:-}"
      LINUX_ENVIRONMENT="$2"
      shift 2
      ;;
    --storage-runner-label)
      require_value "$1" "${2:-}"
      STORAGE_RUNNER_LABEL="$2"
      shift 2
      ;;
    --linux-runner-label)
      require_value "$1" "${2:-}"
      LINUX_RUNNER_LABEL="$2"
      shift 2
      ;;
    --tag)
      require_value "$1" "${2:-}"
      TAG="$2"
      shift 2
      ;;
    --scope-deny-prefix)
      require_value "$1" "${2:-}"
      SCOPE_DENY_PREFIX="${2#/}"
      SCOPE_DENY_PREFIX="${SCOPE_DENY_PREFIX%/}"
      shift 2
      ;;
    --remote-prefix)
      require_value "$1" "${2:-}"
      REMOTE_PREFIX="${2#/}"
      REMOTE_PREFIX="${REMOTE_PREFIX%/}"
      shift 2
      ;;
    --linux-remote-prefix)
      require_value "$1" "${2:-}"
      LINUX_REMOTE_PREFIX="${2#/}"
      LINUX_REMOTE_PREFIX="${LINUX_REMOTE_PREFIX%/}"
      shift 2
      ;;
    --strict)
      STRICT=1
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

[[ -n "$SCOPE_DENY_PREFIX" ]] || die "--scope-deny-prefix must not be empty"

if ! command -v gh >/dev/null 2>&1; then
  die "gh CLI is required"
fi

if [[ -z "$TAG" ]]; then
  TAG="$(latest_release_tag || true)"
  [[ -n "$TAG" ]] || die "could not resolve the newest GitHub Release tag for $REPO; pass --tag explicitly"
fi
[[ "$TAG" == v* ]] || die "--tag must start with v"

storage_required=(
  TCFS_SMOKE_S3_ENDPOINT
  TCFS_SMOKE_S3_BUCKET
  TCFS_SMOKE_S3_ACCESS_KEY_ID
  TCFS_SMOKE_S3_SECRET_ACCESS_KEY
)
linux_required=(
  TCFS_SMOKE_S3_ENDPOINT
  TCFS_SMOKE_S3_BUCKET
  TCFS_SMOKE_S3_ACCESS_KEY_ID
  TCFS_SMOKE_S3_SECRET_ACCESS_KEY
  TCFS_SMOKE_MASTER_KEY_B64
)

storage_missing=()
linux_missing=()
while IFS= read -r line; do
  [[ -n "$line" ]] && storage_missing+=("$line")
done < <(missing_secret_names "$STORAGE_ENVIRONMENT" "${storage_required[@]}")
while IFS= read -r line; do
  [[ -n "$line" ]] && linux_missing+=("$line")
done < <(missing_secret_names "$LINUX_ENVIRONMENT" "${linux_required[@]}")
storage_runner_status="$(runner_status_line "$STORAGE_RUNNER_LABEL")"
linux_runner_status="$(runner_status_line "$LINUX_RUNNER_LABEL")"

blocked=0
run_date="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

printf '# TCFS Alpha Gate Preflight\n\n'
printf -- '- generated_at: `%s`\n' "$run_date"
printf -- '- repo: `%s`\n' "$REPO"
printf -- '- tag: `%s`\n\n' "$TAG"

printf '## TIN-1546 Storage Posture\n\n'
printf -- '- environment: `%s`\n' "$STORAGE_ENVIRONMENT"
printf -- '- runner: `%s` (%s)\n' "$STORAGE_RUNNER_LABEL" "$storage_runner_status"
if [[ "$storage_runner_status" == missing:* ]]; then
  printf -- '- status: `blocked`\n'
  printf -- '- blocker: no online self-hosted runner advertises `%s`\n' "$STORAGE_RUNNER_LABEL"
  blocked=1
elif (( ${#storage_missing[@]} > 0 )); then
  printf -- '- status: `blocked`\n'
  printf -- '- missing_secrets: `%s`\n' "${storage_missing[*]}"
  blocked=1
else
  printf -- '- status: `runnable`\n'
  storage_cmd=(
    scripts/storage-posture-canary-dispatch.sh
    --repo "$REPO"
    --environment "$STORAGE_ENVIRONMENT"
    --runner-label "$STORAGE_RUNNER_LABEL"
    --scope-deny-prefix "$SCOPE_DENY_PREFIX"
  )
  if [[ -n "$REMOTE_PREFIX" ]]; then
    storage_cmd+=(--remote-prefix "$REMOTE_PREFIX")
  fi
  printf -- '- dispatch:\n\n'
  printf '```bash\n%s\n```\n' "$(quote_cmd "${storage_cmd[@]}")"
fi

printf '\n## TIN-1540 / TIN-1422 Linux Package Smoke\n\n'
printf -- '- environment: `%s`\n' "$LINUX_ENVIRONMENT"
printf -- '- runner: `%s` (%s)\n' "$LINUX_RUNNER_LABEL" "$linux_runner_status"
if [[ "$linux_runner_status" == missing:* ]]; then
  printf -- '- status: `blocked`\n'
  printf -- '- blocker: no online self-hosted runner advertises `%s`\n' "$LINUX_RUNNER_LABEL"
  blocked=1
elif (( ${#linux_missing[@]} > 0 )); then
  printf -- '- status: `blocked`\n'
  printf -- '- missing_secrets: `%s`\n' "${linux_missing[*]}"
  blocked=1
else
  printf -- '- status: `runnable`\n'
  linux_remote_prefix="$LINUX_REMOTE_PREFIX"
  if [[ -z "$linux_remote_prefix" && "$LINUX_ENVIRONMENT" == "$STORAGE_ENVIRONMENT" ]]; then
    linux_remote_prefix="gha/storage-posture/linux-postinstall/${TAG}/$(date -u +%Y%m%dT%H%M%SZ)"
  fi
  linux_cmd=(
    gh workflow run linux-postinstall-smoke.yml
    -R "$REPO"
    --ref main
    -f "tag=$TAG"
    -f "runner_label=$LINUX_RUNNER_LABEL"
    -f "smoke_environment=$LINUX_ENVIRONMENT"
    -f "exercise_evict_rehydrate=true"
    -f "exercise_mutation=true"
  )
  if [[ -n "$linux_remote_prefix" ]]; then
    linux_cmd+=(-f "remote_prefix=$linux_remote_prefix")
  fi
  printf -- '- dispatch:\n\n'
  printf '```bash\n%s\n```\n' "$(quote_cmd "${linux_cmd[@]}")"
fi

printf '\n## TIN-132 Neo/Honey Fleet\n\n'
printf -- '- status: `operator-run-required`\n'
printf -- '- command:\n\n'
printf '```bash\njust neo-honey-smoke\n```\n'
printf '\n'
printf 'Run this from the operator environment with `TCFS_E2E_LIVE=1`, live S3 credentials, and live NATS URL set. CI Live Storage remains regression coverage, not a replacement for the named neo/honey transcript.\n'

printf '\n## Claim Boundary\n\n'
printf 'Do not close `TIN-1546`, `TIN-1540`, `TIN-1422`, `TIN-131/#280`, or `TIN-132` from this preflight alone. Close only from the workflow/operator artifacts named above.\n'

if (( STRICT == 1 && blocked == 1 )); then
  exit 1
fi
