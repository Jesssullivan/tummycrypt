#!/usr/bin/env bash
#
# Dispatch the non-production FileProvider testing-mode package workflow, then
# feed its package artifact into the hosted macOS post-install smoke.
#
set -euo pipefail

REPO="${TCFS_GITHUB_REPO:-Jesssullivan/tummycrypt}"
REF="${TCFS_GITHUB_REF:-main}"
TAG="${TAG:-v0.12.6}"
ARTIFACT_NAME="${ARTIFACT_NAME:-dist-testing-mode-pkg}"
PACKAGE_WORKFLOW="macos-fileprovider-testing-mode-pkg.yml"
SMOKE_WORKFLOW="macos-postinstall-smoke.yml"
TESTING_MODE_SECRET="TCFS_HOST_TESTING_MODE_PROVISIONING_PROFILE_BASE64"
RUN_ID_POLL_ATTEMPTS="${TCFS_GH_RUN_ID_POLL_ATTEMPTS:-10}"
RUN_ID_POLL_SECONDS="${TCFS_GH_RUN_ID_POLL_SECONDS:-2}"
DRY_RUN=0
WATCH=1
SKIP_SECRET_CHECK=0
PACKAGE_RUN_ID=""

usage() {
  cat <<'USAGE'
Usage: scripts/macos-fileprovider-testing-mode-dispatch.sh [options]

Options:
  --tag <tag>             Release tag whose CLI tarball supplies tcfs/tcfsd (default: v0.12.6)
  --repo <owner/name>     GitHub repository (default: Jesssullivan/tummycrypt)
  --ref <ref>             Workflow ref to dispatch (default: main)
  --artifact-name <name>  Package artifact name (default: dist-testing-mode-pkg)
  --package-run-id <id>   Skip package workflow dispatch and smoke an existing package run
  --dry-run               Print the commands without calling gh
  --no-watch              Do not wait for workflow completion
  --skip-secret-check     Do not verify TCFS_HOST_TESTING_MODE_PROVISIONING_PROFILE_BASE64
  -h, --help              Show this help
USAGE
}

log() {
  printf '%s\n' "$*" >&2
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

require_value() {
  local flag="$1"
  local value="${2:-}"

  if [[ -z "$value" ]]; then
    die "$flag requires a value"
  fi
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --tag)
      require_value "$1" "${2:-}"
      TAG="$2"
      shift 2
      ;;
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
    --artifact-name)
      require_value "$1" "${2:-}"
      ARTIFACT_NAME="$2"
      shift 2
      ;;
    --package-run-id)
      require_value "$1" "${2:-}"
      PACKAGE_RUN_ID="$2"
      shift 2
      ;;
    --dry-run)
      DRY_RUN=1
      shift
      ;;
    --no-watch)
      WATCH=0
      shift
      ;;
    --skip-secret-check)
      SKIP_SECRET_CHECK=1
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      die "unknown argument: $1"
      ;;
  esac
done

if [[ "$TAG" != v* ]]; then
  die "tag must start with 'v' (got '$TAG')"
fi

print_dry_run() {
  local package_run_id="$PACKAGE_RUN_ID"

  if [[ -z "$package_run_id" ]]; then
    package_run_id="<testing-mode-package-run-id>"

    if [[ "$SKIP_SECRET_CHECK" != "1" ]]; then
      cat <<EOF
gh secret list --repo "$REPO" --json name --jq '.[].name' | grep -Fx "$TESTING_MODE_SECRET"
EOF
    fi

    cat <<EOF
gh release view "$TAG" --repo "$REPO" --json isDraft,assets --jq '. as \$release | select(\$release.isDraft == false) | .assets[].name' | grep -Fx "tcfs-${TAG#v}-macos-aarch64.tar.gz"
gh workflow run "$PACKAGE_WORKFLOW" --repo "$REPO" --ref "$REF" -f tag="$TAG"
EOF

    if [[ "$WATCH" != "1" ]]; then
      cat <<EOF
# Package run dispatched. After it succeeds, rerun with --package-run-id $package_run_id
EOF
      return 0
    fi

    cat <<EOF
gh run watch "$package_run_id" --repo "$REPO" --exit-status
EOF
  fi

  cat <<EOF
gh api "repos/$REPO/actions/runs/$package_run_id/artifacts" --jq '.artifacts[] | select(.expired == false) | .name' | grep -Fx "$ARTIFACT_NAME"
gh workflow run "$SMOKE_WORKFLOW" --repo "$REPO" --ref "$REF" \\
  -f tag="$TAG" \\
  -f package_artifact_run_id="$package_run_id" \\
  -f package_artifact_name="$ARTIFACT_NAME" \\
  -f fileprovider_testing_mode=true
EOF

  if [[ "$WATCH" != "1" ]]; then
    cat <<EOF
# Post-install smoke dispatched. Watch with: gh run watch "<postinstall-smoke-run-id>" --repo "$REPO" --exit-status
EOF
    return 0
  fi

  cat <<EOF
gh run watch "<postinstall-smoke-run-id>" --repo "$REPO" --exit-status
EOF
}

if [[ "$DRY_RUN" == "1" ]]; then
  print_dry_run
  exit 0
fi

command -v gh >/dev/null 2>&1 || die "gh is required"

if [[ "$SKIP_SECRET_CHECK" != "1" && -z "$PACKAGE_RUN_ID" ]]; then
  if ! gh secret list --repo "$REPO" --json name --jq '.[].name' \
    | grep -Fxq "$TESTING_MODE_SECRET"; then
    die "missing repository secret $TESTING_MODE_SECRET"
  fi
fi

gh workflow view "$PACKAGE_WORKFLOW" --repo "$REPO" >/dev/null
gh workflow view "$SMOKE_WORKFLOW" --repo "$REPO" >/dev/null

verify_release_cli_asset() {
  local version="${TAG#v}"
  local asset="tcfs-${version}-macos-aarch64.tar.gz"

  # shellcheck disable=SC2016 # Keep the jq expression literal.
  if ! gh release view "$TAG" \
    --repo "$REPO" \
    --json isDraft,assets \
    --jq '. as $release | select($release.isDraft == false) | .assets[].name' \
    | grep -Fxq "$asset"; then
    die "release $TAG does not expose required asset $asset"
  fi
}

latest_dispatch_run_id() {
  local workflow="$1"
  local created_after="$2"

  gh run list \
    --repo "$REPO" \
    --workflow "$workflow" \
    --event workflow_dispatch \
    --branch "$REF" \
    --created ">=$created_after" \
    --limit 1 \
    --json databaseId \
    --jq '.[0].databaseId // empty'
}

pause_between_run_id_polls() {
  if [[ "$RUN_ID_POLL_SECONDS" == "0" ]]; then
    return 0
  fi

  sleep "$RUN_ID_POLL_SECONDS" &
  wait "$!"
}

wait_for_dispatch_run_id() {
  local workflow="$1"
  local created_after="$2"
  local attempt
  local run_id

  for ((attempt = 1; attempt <= RUN_ID_POLL_ATTEMPTS; attempt += 1)); do
    run_id="$(latest_dispatch_run_id "$workflow" "$created_after")"
    if [[ -n "$run_id" ]]; then
      printf '%s\n' "$run_id"
      return 0
    fi

    if (( attempt < RUN_ID_POLL_ATTEMPTS )); then
      log "Waiting for $workflow run to appear ($attempt/$RUN_ID_POLL_ATTEMPTS)"
      pause_between_run_id_polls
    fi
  done

  return 1
}

verify_package_artifact() {
  local run_id="$1"

  if ! gh api "repos/$REPO/actions/runs/$run_id/artifacts" \
    --jq '.artifacts[] | select(.expired == false) | .name' \
    | grep -Fxq "$ARTIFACT_NAME"; then
    die "run $run_id does not expose a non-expired $ARTIFACT_NAME artifact"
  fi
}

dispatch_and_capture_run_id() {
  local workflow="$1"
  shift

  local created_after
  created_after="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"

  log "Dispatching $workflow on $REF for $TAG"
  gh workflow run "$workflow" --repo "$REPO" --ref "$REF" "$@" >&2

  local run_id
  if ! run_id="$(wait_for_dispatch_run_id "$workflow" "$created_after")"; then
    die "dispatched $workflow but could not locate its run id after $RUN_ID_POLL_ATTEMPTS attempts; inspect with gh run list --repo $REPO --workflow $workflow --event workflow_dispatch"
  fi

  printf '%s\n' "$run_id"
}

if [[ -z "$PACKAGE_RUN_ID" ]]; then
  verify_release_cli_asset
  PACKAGE_RUN_ID="$(dispatch_and_capture_run_id "$PACKAGE_WORKFLOW" -f tag="$TAG")"
  log "Testing-mode package run: $PACKAGE_RUN_ID"

  if [[ "$WATCH" == "1" ]]; then
    gh run watch "$PACKAGE_RUN_ID" --repo "$REPO" --exit-status
  else
    log "Package run dispatched. After it succeeds, rerun with --package-run-id $PACKAGE_RUN_ID"
    exit 0
  fi
else
  log "Using existing testing-mode package run: $PACKAGE_RUN_ID"
fi

verify_package_artifact "$PACKAGE_RUN_ID"

SMOKE_RUN_ID="$(dispatch_and_capture_run_id \
  "$SMOKE_WORKFLOW" \
  -f tag="$TAG" \
  -f package_artifact_run_id="$PACKAGE_RUN_ID" \
  -f package_artifact_name="$ARTIFACT_NAME" \
  -f fileprovider_testing_mode=true)"
log "Post-install smoke run: $SMOKE_RUN_ID"

if [[ "$WATCH" == "1" ]]; then
  gh run watch "$SMOKE_RUN_ID" --repo "$REPO" --exit-status
else
  log "Post-install smoke dispatched. Watch with: gh run watch $SMOKE_RUN_ID --repo $REPO --exit-status"
fi
