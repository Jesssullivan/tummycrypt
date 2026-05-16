#!/usr/bin/env bash
#
# Regression tests for the remote macOS FileProvider .pkg notarization proof.
# The workflow must call Apple's notary service, staple the package, assess it
# with Gatekeeper, and run strict package smoke before exposing an artifact.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORKFLOW="${REPO_ROOT}/.github/workflows/macos-fileprovider-pkg-notarization-proof.yml"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-pkg-notarization-proof-test.XXXXXX")"
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

extract_step() {
  local step_name="$1"
  local output="$2"

  ruby -ryaml -e '
    workflow = YAML.load_file(ARGV[0])
    job = workflow.fetch("jobs").fetch("notarized-pkg-proof")
    step = job.fetch("steps").find { |candidate| candidate["name"] == ARGV[1] }
    raise "step not found: #{ARGV[1]}" unless step
    File.write(ARGV[2], step.fetch("run"))
  ' "$WORKFLOW" "$step_name" "$output"
}

check_workflow_shape() {
  assert_contains "$WORKFLOW" "workflow_dispatch:"
  assert_contains "$WORKFLOW" "contents: read"
  assert_contains "$WORKFLOW" "runs-on: \${{ github.event.inputs.runner_label }}"
  assert_contains "$WORKFLOW" "name: dist-notarized-pkg-proof"
  assert_contains "$WORKFLOW" "name: macos-fileprovider-pkg-notarization-proof-evidence"
  assert_contains "$WORKFLOW" "\${{ runner.temp }}/tcfs-macos-fileprovider-pkg-notarization-proof/"
  assert_contains "$WORKFLOW" "if: always()"
  assert_contains "$WORKFLOW" "retention-days: 14"

  assert_not_contains "$WORKFLOW" "contents: write"
  assert_not_contains "$WORKFLOW" "softprops/action-gh-release"
  assert_not_contains "$WORKFLOW" "gh release"
  assert_not_contains "$WORKFLOW" "create-release"
  assert_not_contains "$WORKFLOW" "update-homebrew"
  assert_not_contains "$WORKFLOW" "|| echo \"::warning::"
  assert_not_contains "$WORKFLOW" "non-fatal"

  ruby -ryaml -e '
    workflow = YAML.load_file(ARGV[0])
    job = workflow.fetch("jobs").fetch("notarized-pkg-proof")
    raise "proof job must stay canonical-only" unless job.fetch("if").include?("Jesssullivan/tummycrypt")
    raise "proof job timeout too small" unless job.fetch("timeout-minutes") >= 90
    job.fetch("steps").each_with_index do |step, index|
      label = "step #{index + 1} #{step["name"] || step["uses"] || "(unnamed)"}"
      raise "#{label}: has with but no uses" if step.key?("with") && !step.key?("uses")
      raise "#{label}: has both run and uses" if step.key?("run") && step.key?("uses")
      raise "#{label}: has neither run nor uses" if !step.key?("run") && !step.key?("uses")
    end
  ' "$WORKFLOW"
}

check_required_secret_gate() {
  local step="${TMPDIR}/validate-secrets.sh"
  extract_step "Validate inputs and Apple secrets" "$step"
  bash -n "$step"

  for secret in \
    APPLE_CERTIFICATE_BASE64 \
    APPLE_INSTALLER_CERTIFICATE_BASE64 \
    APPLE_ID \
    APPLE_TEAM_ID \
    APPLE_NOTARIZE_PASSWORD \
    TCFS_HOST_PROVISIONING_PROFILE_BASE64 \
    TCFS_EXTENSION_PROVISIONING_PROFILE_BASE64
  do
    assert_contains "$WORKFLOW" "$secret: \${{ secrets.${secret} }}"
    assert_contains "$step" "$secret"
  done
  assert_contains "$step" "Missing required Apple signing/notarization secrets"
  assert_contains "$step" "This proof must run on macOS"
  assert_contains "$step" "must run on an arm64 macOS runner"

  for optional_password_secret in \
    APPLE_CERTIFICATE_PASSWORD \
    APPLE_INSTALLER_CERTIFICATE_PASSWORD
  do
    assert_contains "$WORKFLOW" "$optional_password_secret: \${{ secrets.${optional_password_secret} }}"
  done
}

check_signing_and_build_steps() {
  local app_import="${TMPDIR}/import-app-cert.sh"
  local profile_import="${TMPDIR}/import-profiles.sh"
  local build_app="${TMPDIR}/build-fileprovider.sh"
  local installer_import="${TMPDIR}/import-installer-cert.sh"
  local build_pkg="${TMPDIR}/build-pkg.sh"

  extract_step "Import Developer ID Application certificate" "$app_import"
  extract_step "Import FileProvider provisioning profiles" "$profile_import"
  extract_step "Build FileProvider app" "$build_app"
  extract_step "Import Developer ID Installer certificate" "$installer_import"
  extract_step "Build signed package" "$build_pkg"

  bash -n "$app_import" "$profile_import" "$build_app" "$installer_import" "$build_pkg"

  assert_contains "$app_import" "Developer ID Application"
  assert_contains "$app_import" "No Developer ID Application identity found"
  assert_contains "$app_import" "security set-key-partition-list -S apple-tool:,apple:,codesign:"
  assert_contains "$profile_import" "scripts/macos-fileprovider-profile-inventory.sh"
  assert_contains "$profile_import" "--strict"
  assert_contains "$profile_import" "TCFS_REQUIRE_PRODUCTION_SIGNING=1"
  assert_contains "$build_app" "swift/fileprovider/build.sh"
  assert_contains "$build_app" "scripts/macos-fileprovider-preflight.sh"
  assert_contains "$build_app" "--signing-only"
  assert_contains "$build_app" "--require-production-signing"
  assert_contains "$installer_import" "Developer ID Installer"
  assert_contains "$installer_import" "No Developer ID Installer identity found"
  assert_contains "$installer_import" "security set-key-partition-list -S apple-tool:,apple:,productsign:"
  assert_contains "$build_pkg" "scripts/macos-build-pkg.sh"
  assert_contains "$build_pkg" "--sign \"\$PKG_SIGNING_IDENTITY\""
  assert_contains "$build_pkg" "pkgutil --check-signature"
}

check_notarization_is_real() {
  local step="${TMPDIR}/notarize-package.sh"
  extract_step "Notarize package" "$step"
  bash -n "$step"

  assert_contains "$step" "set -euo pipefail"
  assert_contains "$step" "xcrun notarytool submit \"\$PKG_PATH\""
  assert_contains "$step" "--apple-id \"\$APPLE_ID\""
  assert_contains "$step" "--team-id \"\$APPLE_TEAM_ID\""
  assert_contains "$step" "--password \"\$APPLE_NOTARIZE_PASSWORD\""
  assert_contains "$step" "--wait"
  assert_contains "$step" "--output-format json"
  assert_contains "$step" "submit_status=\$?"
  assert_contains "$step" "exit \"\$submit_status\""
  assert_contains "$step" "notarytool accepted package"
  assert_contains "$step" "expected Accepted"
  assert_contains "$step" "xcrun notarytool log \"\$NOTARY_ID\""
  assert_not_contains "$step" "xcrun notarytool submit \"\$PKG_PATH\" || true"
  assert_not_contains "$step" "|| echo \"::warning::"
  assert_not_contains "$step" "non-fatal"
}

check_staple_gatekeeper_and_strict_smoke() {
  local step="${TMPDIR}/staple-assess-smoke.sh"
  extract_step "Staple, assess, and strict-smoke package" "$step"
  bash -n "$step"

  assert_contains "$step" "set -euo pipefail"
  assert_contains "$step" "xcrun stapler staple \"\$PKG_PATH\""
  assert_contains "$step" "xcrun stapler validate -v \"\$PKG_PATH\""
  assert_contains "$step" "/usr/sbin/spctl --assess --type install --verbose=4 \"\$PKG_PATH\""
  assert_contains "$step" "scripts/macos-pkg-structure-smoke.sh"
  assert_contains "$step" "--require-signature"
  assert_contains "$step" "--require-gatekeeper-install"
  assert_contains "$step" "--require-stapled-ticket"
  assert_contains "$step" "pkgutil --check-signature"
}

check_workflow_shape
check_required_secret_gate
check_signing_and_build_steps
check_notarization_is_real
check_staple_gatekeeper_and_strict_smoke

printf 'macOS FileProvider package notarization proof workflow tests passed\n'
