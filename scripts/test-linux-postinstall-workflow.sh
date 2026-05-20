#!/usr/bin/env bash
# shellcheck disable=SC2016 # Literal workflow expressions are what this test asserts.
#
# Static regression checks for the Linux post-install smoke workflow.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORKFLOW="${REPO_ROOT}/.github/workflows/linux-postinstall-smoke.yml"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-linux-postinstall-workflow-test.XXXXXX")"
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
    printf 'expected not to find %s in %s\n' "$unexpected" "$file" >&2
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
    steps = workflow.fetch("jobs").fetch("package-postinstall").fetch("steps")
    step = steps.find { |item| item["name"] == ARGV[1] }
    raise "missing workflow step #{ARGV[1]}" unless step
    puts step.fetch("run")
  ' "$WORKFLOW" "$step_name" >"$output"
}

check_secret_surface() {
  assert_contains "$WORKFLOW" 'TCFS_SMOKE_S3_REGION: ${{ secrets.TCFS_SMOKE_S3_REGION }}'
  assert_contains "$WORKFLOW" 'TCFS_SMOKE_S3_CA_CERT_PEM: ${{ secrets.TCFS_SMOKE_S3_CA_CERT_PEM }}'
}

check_live_config_ca_cert_shape() {
  local step="$TMPDIR/write-live-config.sh"
  extract_step_from_workflow "Write live config" "$step"

  assert_contains "$step" 'REGION="${TCFS_SMOKE_S3_REGION:-us-east-1}"'
  assert_contains "$step" 'CA_CERT_CONFIG_LINE="ca_cert_path = \"${CA_CERT_PATH}\""'
  assert_contains "$step" 'ca_cert_path_supported=true'
  assert_contains "$step" 'ca_cert_path_configured=$([[ -n "$CA_CERT_PATH" ]] && echo true || echo false)'
  assert_not_contains "$step" "printf 'ca_cert_path = \"%s\"\\n' \"\$CA_CERT_PATH\" >> \"\$CONFIG_PATH\""

  python3 - "$step" <<'PY'
import pathlib
import sys

run = pathlib.Path(sys.argv[1]).read_text()
storage = run.index("[storage]")
ca_line = run.index("${CA_CERT_CONFIG_LINE}", storage)
crypto = run.index("[crypto]", storage)
sync = run.index("[sync]", storage)
if not (storage < ca_line < crypto < sync):
    raise SystemExit("CA cert config line must be emitted inside [storage]")
PY
}

check_secret_surface
check_live_config_ca_cert_shape

printf 'Linux post-install smoke workflow tests passed\n'
