#!/usr/bin/env bash
# shellcheck disable=SC2016 # Literal workflow expressions are what this test asserts.
#
# Static regression checks for the TIN-1546 large restore workflow.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORKFLOW="$REPO_ROOT/.github/workflows/storage-large-restore-canary.yml"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-storage-large-restore-workflow-test.XXXXXX")"
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

extract_step_from_workflow() {
  local step_name="$1"
  local output="$2"

  ruby -ryaml -e '
    workflow = YAML.load_file(ARGV[0])
    steps = workflow.fetch("jobs").fetch("large-restore").fetch("steps")
    step = steps.find { |candidate| candidate["name"] == ARGV[1] }
    raise "missing workflow step #{ARGV[1]}" unless step
    puts step.fetch("run", "")
  ' "$WORKFLOW" "$step_name" >"$output"
}

bash -n "$0"

assert_contains "$WORKFLOW" "name: Storage Large Restore Canary"
assert_contains "$WORKFLOW" "environment: \${{ github.event.inputs.smoke_environment }}"
assert_contains "$WORKFLOW" "TCFS_SMOKE_S3_ENDPOINT: \${{ secrets.TCFS_SMOKE_S3_ENDPOINT }}"
assert_contains "$WORKFLOW" "AWS_ACCESS_KEY_ID: \${{ secrets.TCFS_SMOKE_S3_ACCESS_KEY_ID }}"
assert_contains "$WORKFLOW" "gha/storage-posture/large/"
assert_contains "$WORKFLOW" "refusing to run restore against an untrusted prefix"
assert_contains "$WORKFLOW" "actions/upload-artifact@v4"
assert_contains "$WORKFLOW" 'TCFS_S3_REGION=$REGION'
assert_contains "$WORKFLOW" 'TCFS_STORAGE_S3_CA_CERT_PATH=$CA_CERT_PATH'
assert_contains "$WORKFLOW" "ca_cert_path_supported=true"

BUILD_STEP="$TMPDIR/build-step.sh"
extract_step_from_workflow "Build release tcfs" "$BUILD_STEP"
assert_contains "$BUILD_STEP" "cargo build --release -p tcfs-cli --bin tcfs"
assert_contains "$BUILD_STEP" "sha256sum target/release/tcfs"

SOURCE_STEP="$TMPDIR/source-step.sh"
extract_step_from_workflow "Generate synthetic git pack source" "$SOURCE_STEP"
assert_contains "$SOURCE_STEP" "dd if=/dev/urandom"
assert_contains "$SOURCE_STEP" 'git -C "$SOURCE_ROOT" gc --prune=now'
assert_contains "$SOURCE_STEP" "synthetic-source-pack-files.txt"

PUSH_STEP="$TMPDIR/push-step.sh"
extract_step_from_workflow "Push large canary" "$PUSH_STEP"
assert_contains "$PUSH_STEP" "scripts/home-canary-linux-xr-storage-posture.sh"
assert_contains "$PUSH_STEP" '--remote "$REMOTE"'
assert_contains "$PUSH_STEP" "--push"
assert_contains "$PUSH_STEP" "--socket-sample-interval-secs 5"

VALIDATE_PUSH_STEP="$TMPDIR/validate-push-step.sh"
extract_step_from_workflow "Validate large canary push evidence" "$VALIDATE_PUSH_STEP"
assert_contains "$VALIDATE_PUSH_STEP" "push-storage-summary.env"
assert_contains "$VALIDATE_PUSH_STEP" "push.log"
assert_contains "$VALIDATE_PUSH_STEP" "require_gt_zero upload_rows"
assert_contains "$VALIDATE_PUSH_STEP" "require_gt_zero total_file_bytes"
assert_contains "$VALIDATE_PUSH_STEP" "require_gt_zero pack_rows"
assert_contains "$VALIDATE_PUSH_STEP" "require_gt_zero pack_file_bytes"
assert_contains "$VALIDATE_PUSH_STEP" "PermissionDenied|AccessDenied|InvalidAccessKeyId|SignatureDoesNotMatch|NoSuchBucket|upload failed permanently|failed to write remote|Error:"
assert_contains "$VALIDATE_PUSH_STEP" "push completed with transient storage noise"
assert_contains "$VALIDATE_PUSH_STEP" "continuing to restore so TIN-1546 can classify recovery"

RESTORE_STEP="$TMPDIR/restore-step.sh"
extract_step_from_workflow "Restore large canary" "$RESTORE_STEP"
assert_contains "$RESTORE_STEP" "RESTORE_REQUIRE_HEADROOM=1"
assert_contains "$RESTORE_STEP" 'RESTORE_HEADROOM_MARGIN_BYTES="$MARGIN_BYTES"'
assert_contains "$RESTORE_STEP" "REQUIRE_EMPTY_DIRS=1"
assert_contains "$RESTORE_STEP" "scripts/git-repo-restore-proof.sh"
assert_contains "$RESTORE_STEP" '--restore-root "$RESTORE_ROOT"'

printf 'storage large restore canary workflow tests passed\n'
