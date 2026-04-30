#!/usr/bin/env bash
#
# Regression tests for the release workflow's macOS FileProvider packaging
# steps. This keeps CI-only YAML heredocs covered by the same local lazy gate.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORKFLOW="${REPO_ROOT}/.github/workflows/release.yml"
POSTINSTALL_WORKFLOW="${REPO_ROOT}/.github/workflows/macos-postinstall-smoke.yml"
PKG_POSTINSTALL="${REPO_ROOT}/scripts/macos-pkg-postinstall.sh"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-release-workflow-test.XXXXXX")"
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

  local out="${TMPDIR}/failure.out"
  local err="${TMPDIR}/failure.err"

  if "$@" >"$out" 2>"$err"; then
    printf 'expected command to fail: %s\n' "$*" >&2
    exit 1
  fi

  cat "$out" "$err" >"${TMPDIR}/failure.combined"
  assert_contains "${TMPDIR}/failure.combined" "$expected"
}

extract_step() {
  local job="$1"
  local step_name="$2"
  local output="$3"

  extract_step_from_workflow "$WORKFLOW" "$job" "$step_name" "$output"
}

extract_step_from_workflow() {
  local workflow="$1"
  local job="$2"
  local step_name="$3"
  local output="$4"

  ruby -ryaml -e '
    workflow = YAML.load_file(ARGV[0])
    job = workflow.fetch("jobs").fetch(ARGV[1])
    step = job.fetch("steps").find { |candidate| candidate["name"] == ARGV[2] }
    raise "step not found: #{ARGV[1]} / #{ARGV[2]}" unless step
    File.write(ARGV[3], step.fetch("run"))
  ' "$workflow" "$job" "$step_name" "$output"
}

check_workflow_step_shape() {
  ruby -ryaml -e '
    errors = []

    ARGV.each do |workflow_path|
      workflow = YAML.load_file(workflow_path)
      workflow.fetch("jobs").each do |job_name, job|
        Array(job["steps"]).each_with_index do |step, index|
          label = "#{workflow_path}: #{job_name} step #{index + 1} #{step["name"] || step["uses"] || "(unnamed)"}"
          errors << "#{label}: has with but no uses" if step.key?("with") && !step.key?("uses")
          errors << "#{label}: has both run and uses" if step.key?("run") && step.key?("uses")
          errors << "#{label}: has neither run nor uses" if !step.key?("run") && !step.key?("uses")
        end
      end
    end

    unless errors.empty?
      warn errors.join("\n")
      exit 1
    end
  ' "$WORKFLOW" "$POSTINSTALL_WORKFLOW"
}

check_postinstall_workflow_checkout_uses_current_harness() {
  ruby -ryaml -e '
    workflow = YAML.load_file(ARGV[0])
    steps = workflow.fetch("jobs").fetch("pkg-postinstall").fetch("steps")
    checkout = steps.find { |step| step["uses"] == "actions/checkout@v5" }
    raise "checkout step not found" unless checkout
    raise "postinstall checkout must keep the current harness ref" if checkout.key?("with") && checkout["with"].key?("ref")
  ' "$POSTINSTALL_WORKFLOW"
}

check_postinstall_workflow_environment_and_secrets() {
  ruby -ryaml -e '
    workflow = YAML.load_file(ARGV[0])
    job = workflow.fetch("jobs").fetch("pkg-postinstall")
    expected_env = "tcfs-macos-smoke"
    actual_env = job.fetch("environment")
    raise "postinstall environment mismatch: #{actual_env.inspect}" unless actual_env == expected_env

    env = job.fetch("env")
    secret = ->(name) { "#{36.chr}{{ secrets.#{name} }}" }
    expected = {
      "TCFS_SMOKE_S3_ENDPOINT" => secret.call("TCFS_SMOKE_S3_ENDPOINT"),
      "TCFS_SMOKE_S3_BUCKET" => secret.call("TCFS_SMOKE_S3_BUCKET"),
      "TCFS_SMOKE_S3_ACCESS_KEY_ID" => secret.call("TCFS_SMOKE_S3_ACCESS_KEY_ID"),
      "TCFS_SMOKE_S3_SECRET_ACCESS_KEY" => secret.call("TCFS_SMOKE_S3_SECRET_ACCESS_KEY"),
      "TCFS_SMOKE_MASTER_KEY_B64" => secret.call("TCFS_SMOKE_MASTER_KEY_B64"),
      "TCFS_S3_ACCESS" => secret.call("TCFS_SMOKE_S3_ACCESS_KEY_ID"),
      "TCFS_S3_SECRET" => secret.call("TCFS_SMOKE_S3_SECRET_ACCESS_KEY"),
      "AWS_ACCESS_KEY_ID" => secret.call("TCFS_SMOKE_S3_ACCESS_KEY_ID"),
      "AWS_SECRET_ACCESS_KEY" => secret.call("TCFS_SMOKE_S3_SECRET_ACCESS_KEY"),
    }

    expected.each do |name, value|
      actual = env.fetch(name) { raise "missing env: #{name}" }
      raise "env #{name} mismatch: #{actual.inspect}" unless actual == value
    end
  ' "$POSTINSTALL_WORKFLOW"
}

write_profile() {
  local path="$1"
  local name="$2"
  local uuid="$3"
  local team="$4"
  local bundle_id="$5"
  local keychain_suffix="$6"

  cat >"$path" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Name</key>
  <string>${name}</string>
  <key>UUID</key>
  <string>${uuid}</string>
  <key>Entitlements</key>
  <dict>
    <key>application-identifier</key>
    <string>${team}.${bundle_id}</string>
    <key>com.apple.security.application-groups</key>
    <array>
      <string>group.io.tinyland.tcfs</string>
    </array>
    <key>keychain-access-groups</key>
    <array>
      <string>${team}.${keychain_suffix}</string>
    </array>
  </dict>
</dict>
</plist>
EOF
}

base64_file() {
  base64 <"$1"
}

FAKE_BIN="${TMPDIR}/fake-bin"
mkdir -p "$FAKE_BIN"

check_workflow_step_shape
check_postinstall_workflow_checkout_uses_current_harness
check_postinstall_workflow_environment_and_secrets

cat >"$FAKE_BIN/uname" <<'EOF'
#!/usr/bin/env bash
printf 'Darwin\n'
EOF
cat >"$FAKE_BIN/security" <<'EOF'
#!/usr/bin/env bash
if [[ "${1:-}" == "cms" && "${2:-}" == "-D" ]]; then
  shift 2
  while [[ $# -gt 0 ]]; do
    case "$1" in
      -i)
        cat "$2"
        exit 0
        ;;
      *)
        shift
        ;;
    esac
  done
fi
exit 1
EOF
cat >"$FAKE_BIN/pluginkit" <<'EOF'
#!/usr/bin/env bash
printf 'pluginkit' >>"$TCFS_FAKE_POSTINSTALL_LOG"
printf ' %q' "$@" >>"$TCFS_FAKE_POSTINSTALL_LOG"
printf '\n' >>"$TCFS_FAKE_POSTINSTALL_LOG"
EOF
cat >"$FAKE_BIN/stat" <<'EOF'
#!/usr/bin/env bash
if [[ "${1:-}" == "-f" && "${2:-}" == "%Su" && "${3:-}" == "/dev/console" ]]; then
  printf '%s\n' "${TCFS_FAKE_CONSOLE_USER:-jess}"
  exit 0
fi
printf 'unexpected stat invocation:' >&2
printf ' %q' "$@" >&2
printf '\n' >&2
exit 1
EOF
cat >"$FAKE_BIN/id" <<'EOF'
#!/usr/bin/env bash
if [[ "${1:-}" == "-u" && "${2:-}" == "${TCFS_FAKE_CONSOLE_USER:-jess}" ]]; then
  printf '%s\n' "${TCFS_FAKE_CONSOLE_UID:-501}"
  exit 0
fi
printf 'unexpected id invocation:' >&2
printf ' %q' "$@" >&2
printf '\n' >&2
exit 1
EOF
cat >"$FAKE_BIN/launchctl" <<'EOF'
#!/usr/bin/env bash
printf 'launchctl' >>"$TCFS_FAKE_POSTINSTALL_LOG"
printf ' %q' "$@" >>"$TCFS_FAKE_POSTINSTALL_LOG"
printf '\n' >>"$TCFS_FAKE_POSTINSTALL_LOG"

if [[ "${1:-}" == "asuser" ]]; then
  shift 2
  "$@"
fi
EOF
cat >"$FAKE_BIN/sudo" <<'EOF'
#!/usr/bin/env bash
if [[ "${1:-}" == "-u" ]]; then
  shift 2
fi
"$@"
EOF
cat >"$FAKE_BIN/chown" <<'EOF'
#!/usr/bin/env bash
printf 'chown' >>"$TCFS_FAKE_POSTINSTALL_LOG"
printf ' %q' "$@" >>"$TCFS_FAKE_POSTINSTALL_LOG"
printf '\n' >>"$TCFS_FAKE_POSTINSTALL_LOG"
EOF
chmod +x "$FAKE_BIN"/*

IMPORT_STEP="${TMPDIR}/import-fileprovider-profiles.sh"
extract_step "build-fileprovider" "Import FileProvider provisioning profiles" "$IMPORT_STEP"
bash -n "$IMPORT_STEP"
assert_contains "$IMPORT_STEP" "set -euo pipefail"
assert_contains "$IMPORT_STEP" "mkdir -p \"\$RUNNER_TEMP\""
assert_contains "$IMPORT_STEP" "scripts/macos-fileprovider-profile-inventory.sh"
assert_contains "$IMPORT_STEP" "TCFS_REQUIRE_PRODUCTION_SIGNING=1"

RAW_HOST_PROFILE="${TMPDIR}/raw-host.provisionprofile"
RAW_EXTENSION_PROFILE="${TMPDIR}/raw-extension.provisionprofile"
write_profile \
  "$RAW_HOST_PROFILE" \
  "TCFS Host" \
  "HOST-UUID" \
  "QP994XQKNH" \
  "io.tinyland.tcfs" \
  "*"
write_profile \
  "$RAW_EXTENSION_PROFILE" \
  "TCFS FileProvider Extension" \
  "EXT-UUID" \
  "QP994XQKNH" \
  "io.tinyland.tcfs.fileprovider" \
  "*"

IMPORT_RUNNER_TEMP="${TMPDIR}/runner"
IMPORT_ENV="${TMPDIR}/github-env"
IMPORT_OUT="${TMPDIR}/import.out"
PATH="$FAKE_BIN:$PATH" \
RUNNER_TEMP="$IMPORT_RUNNER_TEMP" \
GITHUB_ENV="$IMPORT_ENV" \
TCFS_HOST_PROVISIONING_PROFILE_BASE64="$(base64_file "$RAW_HOST_PROFILE")" \
TCFS_EXTENSION_PROVISIONING_PROFILE_BASE64="$(base64_file "$RAW_EXTENSION_PROFILE")" \
bash -e "$IMPORT_STEP" >"$IMPORT_OUT"

assert_contains "$IMPORT_OUT" "profiles scanned: 2"
assert_contains "$IMPORT_OUT" "compatible pair: found"
assert_contains "$IMPORT_OUT" "host candidates: 1"
assert_contains "$IMPORT_OUT" "extension candidates: 1"
assert_contains "$IMPORT_ENV" "TCFS_HOST_PROVISIONING_PROFILE=${IMPORT_RUNNER_TEMP}/tcfs-host-developer-id.provisionprofile"
assert_contains "$IMPORT_ENV" "TCFS_EXTENSION_PROVISIONING_PROFILE=${IMPORT_RUNNER_TEMP}/tcfs-fileprovider-developer-id.provisionprofile"
assert_contains "$IMPORT_ENV" "TCFS_REQUIRE_PRODUCTION_SIGNING=1"

assert_fails_contains \
  "::error::TCFS_EXTENSION_PROVISIONING_PROFILE_BASE64 is required" \
  env PATH="$FAKE_BIN:$PATH" \
    RUNNER_TEMP="${TMPDIR}/missing-extension-runner" \
    GITHUB_ENV="${TMPDIR}/missing-extension-env" \
    TCFS_HOST_PROVISIONING_PROFILE_BASE64="$(base64_file "$RAW_HOST_PROFILE")" \
    TCFS_EXTENSION_PROVISIONING_PROFILE_BASE64="" \
    bash -e "$IMPORT_STEP"

BUILD_PKG_STEP="${TMPDIR}/build-pkg.sh"
extract_step "build-pkg" "Build .pkg" "$BUILD_PKG_STEP"
assert_contains "$BUILD_PKG_STEP" "scripts/macos-build-pkg.sh"
assert_contains "$BUILD_PKG_STEP" "--cli-tar \"cli-dist/tcfs-\${VERSION}-macos-aarch64.tar.gz\""
assert_contains "$BUILD_PKG_STEP" "--fileprovider-zip \"\$FP_ZIP\""
assert_contains "$BUILD_PKG_STEP" "--output \"tcfs-\${VERSION}-macos-aarch64.pkg\""
assert_contains "$BUILD_PKG_STEP" "--sign \"\${PKG_SIGNING_IDENTITY:-}\""

VERIFY_RELEASE_PKG_STEP="${TMPDIR}/verify-release-package-structure.sh"
extract_step_from_workflow \
  "$POSTINSTALL_WORKFLOW" \
  "pkg-postinstall" \
  "Verify release package structure" \
  "$VERIFY_RELEASE_PKG_STEP"
bash -n "$VERIFY_RELEASE_PKG_STEP"
assert_contains "$VERIFY_RELEASE_PKG_STEP" "scripts/macos-pkg-structure-smoke.sh"
assert_contains "$VERIFY_RELEASE_PKG_STEP" "--pkg \"\$RUNNER_TEMP/tcfs-\${VERSION}-macos-aarch64.pkg\""
assert_contains "$VERIFY_RELEASE_PKG_STEP" "--require-signature"
assert_contains "$VERIFY_RELEASE_PKG_STEP" "require_current_postinstall"
assert_contains "$VERIFY_RELEASE_PKG_STEP" "--allow-postinstall-mismatch"
assert_contains "$VERIFY_RELEASE_PKG_STEP" "--expected-postinstall scripts/macos-pkg-postinstall.sh"

INSTALL_BINARY_SMOKE_STEP="${TMPDIR}/prove-installed-binary-smoke.sh"
extract_step_from_workflow \
  "$POSTINSTALL_WORKFLOW" \
  "pkg-postinstall" \
  "Prove installed-binary smoke" \
  "$INSTALL_BINARY_SMOKE_STEP"
bash -n "$INSTALL_BINARY_SMOKE_STEP"
assert_contains "$INSTALL_BINARY_SMOKE_STEP" "-u TCFS_S3_ACCESS"
assert_contains "$INSTALL_BINARY_SMOKE_STEP" "-u TCFS_S3_SECRET"
assert_contains "$INSTALL_BINARY_SMOKE_STEP" "-u AWS_ACCESS_KEY_ID"
assert_contains "$INSTALL_BINARY_SMOKE_STEP" "-u AWS_SECRET_ACCESS_KEY"
assert_contains "$INSTALL_BINARY_SMOKE_STEP" "scripts/install-smoke.sh --expected-version \"\${VERSION}\""

VALIDATE_STORAGE_STEP="${TMPDIR}/validate-release-inputs-and-storage-secrets.sh"
extract_step_from_workflow \
  "$POSTINSTALL_WORKFLOW" \
  "pkg-postinstall" \
  "Validate release inputs and storage secrets" \
  "$VALIDATE_STORAGE_STEP"
assert_contains "$VALIDATE_STORAGE_STEP" "TCFS_SMOKE_S3_ENDPOINT"
assert_contains "$VALIDATE_STORAGE_STEP" "TCFS_SMOKE_S3_BUCKET"
assert_contains "$VALIDATE_STORAGE_STEP" "TCFS_SMOKE_S3_ACCESS_KEY_ID"
assert_contains "$VALIDATE_STORAGE_STEP" "TCFS_SMOKE_S3_SECRET_ACCESS_KEY"
assert_contains "$VALIDATE_STORAGE_STEP" "TCFS_SMOKE_MASTER_KEY_B64"
assert_contains "$VALIDATE_STORAGE_STEP" "Missing required tcfs-macos-smoke environment secrets"
assert_contains "$VALIDATE_STORAGE_STEP" "parsed.scheme != \"https\""

INSTALL_MASTER_KEY_STEP="${TMPDIR}/install-e2ee-master-key.sh"
extract_step_from_workflow \
  "$POSTINSTALL_WORKFLOW" \
  "pkg-postinstall" \
  "Install E2EE master key" \
  "$INSTALL_MASTER_KEY_STEP"
bash -n "$INSTALL_MASTER_KEY_STEP"
assert_contains "$INSTALL_MASTER_KEY_STEP" "TCFS_SMOKE_MASTER_KEY_B64"
assert_contains "$INSTALL_MASTER_KEY_STEP" "base64.b64decode(encoded, validate=True)"
assert_contains "$INSTALL_MASTER_KEY_STEP" "if len(key) != 32:"
assert_contains "$INSTALL_MASTER_KEY_STEP" "chmod 600 \"\$MASTER_KEY_PATH\""

WRITE_LIVE_CONFIG_STEP="${TMPDIR}/write-live-config.sh"
extract_step_from_workflow \
  "$POSTINSTALL_WORKFLOW" \
  "pkg-postinstall" \
  "Write live config" \
  "$WRITE_LIVE_CONFIG_STEP"
bash -n "$WRITE_LIVE_CONFIG_STEP"
assert_contains "$WRITE_LIVE_CONFIG_STEP" "endpoint = \"\${TCFS_SMOKE_S3_ENDPOINT}\""
assert_contains "$WRITE_LIVE_CONFIG_STEP" "bucket = \"\${TCFS_SMOKE_S3_BUCKET}\""
assert_contains "$WRITE_LIVE_CONFIG_STEP" "enforce_tls = true"
assert_contains "$WRITE_LIVE_CONFIG_STEP" "[crypto]"
assert_contains "$WRITE_LIVE_CONFIG_STEP" "enabled = true"
assert_contains "$WRITE_LIVE_CONFIG_STEP" "master_key_file = \"\${MASTER_KEY_PATH}\""

SEED_REMOTE_FIXTURE_STEP="${TMPDIR}/seed-remote-fixture.sh"
extract_step_from_workflow \
  "$POSTINSTALL_WORKFLOW" \
  "pkg-postinstall" \
  "Seed remote fixture" \
  "$SEED_REMOTE_FIXTURE_STEP"
bash -n "$SEED_REMOTE_FIXTURE_STEP"
assert_contains "$SEED_REMOTE_FIXTURE_STEP" "> \"\$EXPECTED_CONTENT_FILE\""
assert_contains "$SEED_REMOTE_FIXTURE_STEP" "cp \"\$EXPECTED_CONTENT_FILE\" \"\$FIXTURE_PATH\""
assert_contains "$SEED_REMOTE_FIXTURE_STEP" "tcfs --config \"\$CONFIG_PATH\" push \"\$FIXTURE_PATH\""

VERIFY_E2EE_STEP="${TMPDIR}/verify-remote-fixture-requires-e2ee-key.sh"
extract_step_from_workflow \
  "$POSTINSTALL_WORKFLOW" \
  "pkg-postinstall" \
  "Verify remote fixture requires E2EE key" \
  "$VERIFY_E2EE_STEP"
bash -n "$VERIFY_E2EE_STEP"
assert_contains "$VERIFY_E2EE_STEP" "NO_CRYPTO_CONFIG_PATH"
assert_contains "$VERIFY_E2EE_STEP" "if tcfs --config \"\$NO_CRYPTO_CONFIG_PATH\" pull \"\$FIXTURE_PATH\""
assert_contains "$VERIFY_E2EE_STEP" "Encrypted smoke fixture was readable without the E2EE master key"
assert_contains "$VERIFY_E2EE_STEP" "tcfs --config \"\$CONFIG_PATH\" pull \"\$FIXTURE_PATH\""
assert_contains "$VERIFY_E2EE_STEP" "cmp -s \"\$EXPECTED_CONTENT_FILE\" \"\$RUNNER_TEMP/e2ee-pull-check\""

POSTINSTALL_HARNESS_STEP="${TMPDIR}/run-macos-postinstall-harness.sh"
extract_step_from_workflow \
  "$POSTINSTALL_WORKFLOW" \
  "pkg-postinstall" \
  "Run macOS post-install harness" \
  "$POSTINSTALL_HARNESS_STEP"
bash -n "$POSTINSTALL_HARNESS_STEP"
assert_contains "$POSTINSTALL_HARNESS_STEP" "--expected-content-file \"\$EXPECTED_CONTENT_FILE\""
assert_contains "$POSTINSTALL_HARNESS_STEP" "--require-keychain-config"

bash -n "$PKG_POSTINSTALL"

assert_contains "$PKG_POSTINSTALL" "FP_APPEX=\"\${APP_PATH}/Contents/Extensions/TCFSFileProvider.appex\""
assert_contains "$PKG_POSTINSTALL" "\"\$LAUNCHCTL_BIN\" asuser \"\$CONSOLE_UID\""
assert_contains "$PKG_POSTINSTALL" "\"\$PLUGINKIT_BIN\" -a \"\$FP_APPEX\""
assert_contains "$PKG_POSTINSTALL" "PLIST_DIR=\"\${TCFS_POSTINSTALL_LAUNCHAGENTS_DIR:-/Library/LaunchAgents}\""
assert_contains "$PKG_POSTINSTALL" "exec /usr/local/bin/tcfsd --config \"\$HOME/.config/tcfs/config.toml\" --mode daemon"

POSTINSTALL_APP="${TMPDIR}/Applications/TCFSProvider.app"
POSTINSTALL_APPEX="${POSTINSTALL_APP}/Contents/Extensions/TCFSFileProvider.appex"
POSTINSTALL_LAUNCHAGENTS="${TMPDIR}/LaunchAgents"
POSTINSTALL_LOG="${TMPDIR}/postinstall.log"
mkdir -p "$POSTINSTALL_APPEX"
TCFS_POSTINSTALL_APP_PATH="$POSTINSTALL_APP" \
TCFS_POSTINSTALL_LAUNCHAGENTS_DIR="$POSTINSTALL_LAUNCHAGENTS" \
TCFS_POSTINSTALL_PLUGINKIT="$FAKE_BIN/pluginkit" \
TCFS_POSTINSTALL_LAUNCHCTL="$FAKE_BIN/launchctl" \
TCFS_POSTINSTALL_SUDO="$FAKE_BIN/sudo" \
TCFS_POSTINSTALL_STAT="$FAKE_BIN/stat" \
TCFS_POSTINSTALL_ID="$FAKE_BIN/id" \
TCFS_POSTINSTALL_CHOWN="$FAKE_BIN/chown" \
TCFS_FAKE_POSTINSTALL_LOG="$POSTINSTALL_LOG" \
bash "$PKG_POSTINSTALL"

PLIST_PATH="${POSTINSTALL_LAUNCHAGENTS}/io.tinyland.tcfsd.plist"
[[ -f "$PLIST_PATH" ]] || {
  printf 'expected postinstall to write %s\n' "$PLIST_PATH" >&2
  exit 1
}
assert_contains "$PLIST_PATH" "io.tinyland.tcfsd"
assert_contains "$PLIST_PATH" "exec /usr/local/bin/tcfsd --config \"\$HOME/.config/tcfs/config.toml\" --mode daemon"
assert_contains "$POSTINSTALL_LOG" "pluginkit -a $POSTINSTALL_APPEX"
assert_contains "$POSTINSTALL_LOG" "launchctl asuser 501"
assert_contains "$POSTINSTALL_LOG" "launchctl bootstrap gui/501 $PLIST_PATH"
assert_contains "$POSTINSTALL_LOG" "launchctl enable gui/501/io.tinyland.tcfsd"

NO_SESSION_LAUNCHAGENTS="${TMPDIR}/no-session-launchagents"
NO_SESSION_LOG="${TMPDIR}/no-session-postinstall.log"
TCFS_POSTINSTALL_APP_PATH="${TMPDIR}/missing-app/TCFSProvider.app" \
TCFS_POSTINSTALL_LAUNCHAGENTS_DIR="$NO_SESSION_LAUNCHAGENTS" \
TCFS_POSTINSTALL_PLUGINKIT="$FAKE_BIN/pluginkit" \
TCFS_POSTINSTALL_LAUNCHCTL="$FAKE_BIN/launchctl" \
TCFS_POSTINSTALL_SUDO="$FAKE_BIN/sudo" \
TCFS_POSTINSTALL_STAT="$FAKE_BIN/stat" \
TCFS_POSTINSTALL_ID="$FAKE_BIN/id" \
TCFS_POSTINSTALL_CHOWN="$FAKE_BIN/chown" \
TCFS_FAKE_CONSOLE_USER=root \
TCFS_FAKE_POSTINSTALL_LOG="$NO_SESSION_LOG" \
bash "$PKG_POSTINSTALL"
[[ -f "${NO_SESSION_LAUNCHAGENTS}/io.tinyland.tcfsd.plist" ]] || {
  printf 'expected postinstall without app/session to still write LaunchAgent\n' >&2
  exit 1
}
if [[ -f "$NO_SESSION_LOG" ]] && grep -Fq "pluginkit" "$NO_SESSION_LOG"; then
  printf 'postinstall attempted pluginkit without installed app\n' >&2
  cat "$NO_SESSION_LOG" >&2
  exit 1
fi

if command -v shellcheck >/dev/null 2>&1; then
  shellcheck -s bash "$IMPORT_STEP"
  shellcheck "$PKG_POSTINSTALL"
fi

printf 'release workflow FileProvider packaging tests passed\n'
