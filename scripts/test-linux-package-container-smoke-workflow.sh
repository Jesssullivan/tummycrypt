#!/usr/bin/env bash
# shellcheck disable=SC2016 # Literal workflow expressions are what this test asserts.
#
# Static regression checks for the Linux package container smoke workflow.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORKFLOW="${REPO_ROOT}/.github/workflows/linux-package-container-smoke.yml"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-linux-package-container-workflow-test.XXXXXX")"
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
    steps = workflow.fetch("jobs").fetch("package-container-smoke").fetch("steps")
    step = steps.find { |item| item["name"] == ARGV[1] }
    raise "missing workflow step #{ARGV[1]}" unless step
    puts step.fetch("run")
  ' "$WORKFLOW" "$step_name" >"$output"
}

check_matrix_surfaces() {
  assert_contains "$WORKFLOW" "pull_request:"
  assert_contains "$WORKFLOW" ".github/workflows/linux-package-container-smoke.yml"
  assert_contains "$WORKFLOW" "Debian 13 amd64 .deb install smoke"
  assert_contains "$WORKFLOW" "image: debian:13"
  assert_contains "$WORKFLOW" "package_family: deb"
  assert_contains "$WORKFLOW" "smoke_mode: fresh-install"
  assert_contains "$WORKFLOW" "skip_cli: \"false\""
  assert_contains "$WORKFLOW" "Debian 13 amd64 .deb upgrade smoke"
  assert_contains "$WORKFLOW" "Ubuntu 24.04 amd64 .deb upgrade smoke"
  assert_contains "$WORKFLOW" "smoke_mode: upgrade"

  assert_contains "$WORKFLOW" "Fedora 42 x86_64 daemon-only .rpm install smoke"
  assert_contains "$WORKFLOW" "Fedora 42 x86_64 daemon-only .rpm upgrade smoke"
  assert_contains "$WORKFLOW" "image: fedora:42"
  assert_contains "$WORKFLOW" "package_family: rpm"
  assert_contains "$WORKFLOW" "skip_cli: \"true\""
}

check_release_asset_selection() {
  local step="$TMPDIR/download-and-verify.sh"
  extract_step_from_workflow "Download and verify public package assets" "$step"

  assert_contains "$step" 'VERSION="${TAG#v}"'
  assert_contains "$step" 'PREVIOUS_VERSION="${PREVIOUS_TAG#v}"'
  assert_contains "$step" '"tcfs-${version}-amd64.deb"'
  assert_contains "$step" '"tcfsd-${version}-amd64.deb"'
  assert_contains "$step" '"tcfsd-${version}-x86_64.rpm"'
  assert_contains "$step" 'download_release_assets "$TAG" "$VERSION" current'
  assert_contains "$step" 'download_release_assets "$PREVIOUS_TAG" "$PREVIOUS_VERSION" previous'
  assert_contains "$step" 'releases/download/${tag}'
  assert_contains "$step" 'SHA256SUMS.txt'
  assert_contains "$step" 'sha256sum -c SHA256SUMS-selected.txt'
}

check_container_smoke() {
  local step="$TMPDIR/container-smoke.sh"
  extract_step_from_workflow "Run package install smoke in container" "$step"

  assert_contains "$step" 'install_packages_from()'
  assert_contains "$step" 'run_install_smoke()'
  assert_contains "$step" 'apt-get install -y --no-install-recommends "$package_dir"/tcfs-*.deb "$package_dir"/tcfsd-*.deb'
  assert_contains "$step" 'dnf upgrade -y "$package_dir"/tcfsd-*.rpm'
  assert_contains "$step" 'install_packages_from /packages/previous install'
  assert_contains "$step" 'run_install_smoke "$PREVIOUS_BINARY_EXPECTED_VERSION" /logs/install-smoke-before-upgrade.log'
  assert_contains "$step" 'install_packages_from /packages/current upgrade'
  assert_contains "$step" 'bash /work/scripts/install-smoke.sh'
  assert_contains "$step" '--skip-cli'
  assert_contains "$step" 'docker run --rm -i'
  assert_contains "$step" '-e PREVIOUS_BINARY_EXPECTED_VERSION="$PREVIOUS_BINARY_EXPECTED_VERSION"'
  assert_contains "$step" '-e SMOKE_MODE="$SMOKE_MODE"'
  assert_contains "$step" '--platform "${{ matrix.platform }}"'
  assert_contains "$step" '2>&1 | tee "$LOG_DIR/container-smoke.log"'
  assert_contains "$step" 'status="${PIPESTATUS[0]}"'
}

check_artifact_retention() {
  assert_contains "$WORKFLOW" "github.event.inputs.tag || 'v0.12.13-rc4'"
  assert_contains "$WORKFLOW" "github.event.inputs.binary_expected_version || '0.12.13'"
  assert_contains "$WORKFLOW" "github.event.inputs.previous_tag || 'v0.12.12'"
  assert_contains "$WORKFLOW" "github.event.inputs.previous_binary_expected_version || '0.12.12'"
  assert_contains "$WORKFLOW" "github.event.inputs.tag || github.ref"
  assert_contains "$WORKFLOW" "linux-package-container-smoke-\${{ github.event.inputs.tag || 'v0.12.13-rc4' }}-\${{ matrix.artifact_suffix }}"
  assert_contains "$WORKFLOW" "retention-days: 14"
}

check_matrix_surfaces
check_release_asset_selection
check_container_smoke
check_artifact_retention

printf 'Linux package container smoke workflow tests passed\n'
