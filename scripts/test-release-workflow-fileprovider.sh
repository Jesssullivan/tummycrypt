#!/usr/bin/env bash
#
# Regression tests for the release workflow's macOS FileProvider packaging
# steps. This keeps CI-only YAML heredocs covered by the same local lazy gate.
#
set -Eeuo pipefail
trap 'printf "release workflow fixture failed at line %d\n" "$LINENO" >&2' ERR

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORKFLOW="${REPO_ROOT}/.github/workflows/release.yml"
POSTINSTALL_WORKFLOW="${REPO_ROOT}/.github/workflows/macos-postinstall-smoke.yml"
TESTING_MODE_PKG_WORKFLOW="${REPO_ROOT}/.github/workflows/macos-fileprovider-testing-mode-pkg.yml"
PKG_POSTINSTALL="${REPO_ROOT}/scripts/macos-pkg-postinstall.sh"
LAB_GATEKEEPER_OVERRIDE="${REPO_ROOT}/scripts/macos-fileprovider-lab-gatekeeper-override.sh"
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
  ' "$WORKFLOW" "$POSTINSTALL_WORKFLOW" "$TESTING_MODE_PKG_WORKFLOW"
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
  # shellcheck disable=SC2016 # Keep the GitHub expression literal intact for YAML comparison.
  ruby -ryaml -e '
    workflow = YAML.load_file(ARGV[0])
    job = workflow.fetch("jobs").fetch("pkg-postinstall")
    expected_runner = "${{ github.event.inputs.runner_label }}"
    actual_runner = job.fetch("runs-on")
    raise "postinstall runner mismatch: #{actual_runner.inspect}" unless actual_runner == expected_runner

    expected_env = "${{ github.event.inputs.smoke_environment }}"
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
      "TCFS_RUNNER_SUDO_PASSWORD" => secret.call("TCFS_RUNNER_SUDO_PASSWORD"),
    }

    expected.each do |name, value|
      actual = env.fetch(name) { raise "missing env: #{name}" }
      raise "env #{name} mismatch: #{actual.inspect}" unless actual == value
    end
  ' "$POSTINSTALL_WORKFLOW"
}

check_postinstall_workflow_artifact_download_uses_api_zip() {
  local download_step="${TMPDIR}/postinstall-download-package.sh"

  extract_step_from_workflow \
    "$POSTINSTALL_WORKFLOW" \
    "pkg-postinstall" \
    "Download package" \
    "$download_step"
  bash -n "$download_step"
  assert_contains "$download_step" "archive_download_url"
  assert_contains "$download_step" "python3 -m zipfile -e"
  assert_contains "$download_step" "Authorization: Bearer \$GH_TOKEN"
  assert_not_contains "$download_step" "gh run download"
}

check_release_action_token_override() {
  # shellcheck disable=SC2016 # Keep the GitHub expression literal intact for YAML comparison.
  ruby -ryaml -e '
    workflow = YAML.load_file(ARGV[0])
    steps = workflow.fetch("jobs").fetch("create-release").fetch("steps")
    step = steps.find { |candidate| candidate["name"] == "Create release" }
    raise "Create release step not found" unless step
    with = step.fetch("with")
    expected = "${{ secrets.GH_RELEASE_TOKEN || github.token }}"
    actual = with.fetch("token") { raise "Create release step missing token override" }
    raise "Create release token mismatch: #{actual.inspect}" unless actual == expected
  ' "$WORKFLOW"
}

check_release_gates_and_apple_signing_requirements() {
  ruby -ryaml -e '
    workflow = YAML.load_file(ARGV[0])
    jobs = workflow.fetch("jobs")

    ["build-pkg", "create-release", "update-homebrew"].each do |job_name|
      condition = jobs.fetch(job_name).fetch("if")
      raise "#{job_name} must require upstream success" unless condition.include?("success()")
      raise "#{job_name} must not use !cancelled() as a success gate" if condition.include?("!cancelled()")
    end

    produced = File.read(ARGV[0]).lines.take(24).join
    raise "release header must list FileProvider zip" unless produced.include?("macOS FileProvider app archive")
    raise "release header must list macOS pkg" unless produced.include?("macOS installer package")

    fileprovider_import = jobs.fetch("build-fileprovider").fetch("steps").find { |step| step["name"] == "Import signing certificate" }.fetch("run")
    raise "release FileProvider build must require APPLE_CERTIFICATE_BASE64" unless fileprovider_import.include?("APPLE_CERTIFICATE_BASE64 is required")
    raise "release FileProvider build must fail without Developer ID Application" unless fileprovider_import.include?("No Developer ID Application identity found")
    raise "release FileProvider build must not fall back to ad-hoc signing" if fileprovider_import.include?("using ad-hoc")

    installer_import = jobs.fetch("build-pkg").fetch("steps").find { |step| step["name"] == "Import installer certificate" }.fetch("run")
    raise "release pkg build must require APPLE_INSTALLER_CERTIFICATE_BASE64" unless installer_import.include?("APPLE_INSTALLER_CERTIFICATE_BASE64 is required")
    raise "release pkg build must fail without Developer ID Installer" unless installer_import.include?("No Developer ID Installer identity found")
    raise "release pkg build must not build unsigned packages" if installer_import.include?("building unsigned")
  ' "$WORKFLOW"
}

check_release_apple_failure_paths() {
  local gate_root="${TMPDIR}/release-apple-gates"
  local gate_tools="${gate_root}/tools"
  mkdir -p "$gate_tools"

  # Run the real YAML step bodies. Only GitHub expressions are rendered with
  # synthetic values; commands and their failure handling are not rewritten.
  extract_step "build-binaries" "Import signing certificate (macOS)" "$gate_root/import.sh"
  extract_step "build-binaries" "Sign CLI binaries (macOS)" "$gate_root/sign.sh"
  extract_step "build-binaries" "Notarize CLI binaries (macOS)" "$gate_root/cli.sh"
  extract_step "build-fileprovider" "Notarize FileProvider" "$gate_root/fileprovider.sh"
  extract_step "build-binaries" "Package and notarize daemon app (macOS)" "$gate_root/daemon.sh"
  ruby -e '
    substitutions = {
      "${{ needs.plan.outputs.version }}" => "9.8.7",
      "${{ matrix.name }}" => "macos-aarch64",
      "${{ matrix.target }}" => "aarch64-apple-darwin",
      "${{ secrets.APPLE_DEVELOPER_ID_CA_G2 }}" => "",
      "${{ github.sha }}" => "ffffffffffffffffffffffffffffffffffffffff"
    }
    ARGV.each do |path|
      body = File.read(path)
      substitutions.each { |from, to| body = body.gsub(from, to) }
      raise "unrendered GitHub expression" if body.include?("${{")
      File.write(path, body)
    end
  ' "$gate_root/import.sh" "$gate_root/sign.sh" "$gate_root/cli.sh" "$gate_root/fileprovider.sh" "$gate_root/daemon.sh"

  cat >"$gate_tools/security" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'security-%s\n' "$1" >> "$TCFS_GATE_LOG"
case "$1" in
  find-identity)
    if [[ "$TCFS_GATE_MODE" == "no-identity" ]]; then
      printf '0 valid identities found\n'
    else
      printf '1) 1111111111111111111111111111111111111111 "Developer ID Application: Fixture (TESTTEAMID)"\n'
      if [[ "$TCFS_GATE_MODE" == ambiguous-identity ]]; then
        printf '2) 2222222222222222222222222222222222222222 "Developer ID Application: Fixture (TESTTEAMID)"\n'
      fi
    fi
    ;;
  create-keychain|set-keychain-settings|unlock-keychain|import|set-key-partition-list|list-keychains) ;;
  *) exit 71 ;;
esac
EOF
  cat >"$gate_tools/openssl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ "$*" == "rand -hex 16" ]]
printf 'fixture-keychain-password\n'
EOF
  cat >"$gate_tools/shasum" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ "$#" == 3 && "$1 $2" == '-a 256' ]]
# macOS supplies shasum; the Linux fixture hashes only its actual owned input.
python3 - "$3" <<'PY'
import hashlib, os, pathlib, sys
path = pathlib.Path(sys.argv[1]).resolve()
assert str(path).startswith(os.environ['TCFS_GATE_ROOT'] + '/')
assert path.read_bytes() == b'native fixture input\n'
print(hashlib.sha256(path.read_bytes()).hexdigest() + '  ' + sys.argv[1])
PY
EOF
  cat >"$gate_tools/ditto" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'archive\n' >> "$TCFS_GATE_LOG"
destination="${!#}"
[[ "$destination" == /* ]] || destination="$PWD/$destination"
[[ "$destination" == "$TCFS_GATE_ROOT/"* && "$destination" != *'/../'* ]]
printf 'owned synthetic archive\n' > "$destination"
EOF
  cat >"$gate_tools/codesign" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ "$#" == 4 && "$1 $2 $3" == "--verify --deep --strict" ]]
[[ "$4" == build/TCFSProvider.app || "$4" == "$TCFS_GATE_ROOT/"*/TCFSDaemon.app ]]
printf 'signature-verify\n' >> "$TCFS_GATE_LOG"
[[ "$TCFS_GATE_MODE" != signature-failed ]]
EOF
  cat >"$gate_tools/xcrun" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
case "$1 $2" in
  "notarytool submit")
    [[ "$*" == *'--wait --timeout 15m --output-format json' ]]
    printf 'notary-submit\n' >> "$TCFS_GATE_LOG"
    case "$TCFS_GATE_MODE" in
      failed) printf 'synthetic notary failure\n' >&2; exit 19 ;;
      invalid) printf '{"status":"Invalid"}\n' ;;
      pending) printf '{"status":"In Progress"}\n' ;;
      missing-status) printf '{"message":"status absent"}\n' ;;
      missing-id) printf '{"status":"Accepted"}\n' ;;
      invalid-id) printf '{"status":"Accepted","id":"wrong"}\n' ;;
      malformed) printf 'not json\n' ;;
      wrong-shape) printf '[{"status":"Accepted"}]\n' ;;
      oversized) python3 -c 'print(" " * 65537 + "{\"status\":\"Accepted\"}")' ;;
      *) printf '{"status":"Accepted","id":"11111111-2222-4333-8444-555555555555"}\n' ;;
    esac
    ;;
  "stapler staple") printf 'staple\n' >> "$TCFS_GATE_LOG"; [[ "$TCFS_GATE_MODE" != staple-failed ]] ;;
  "stapler validate") printf 'staple-validate\n' >> "$TCFS_GATE_LOG"; [[ "$TCFS_GATE_MODE" != staple-validation-failed ]] ;;
  *) exit 72 ;;
esac
EOF
  cat >"$gate_tools/spctl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ "$#" == 5 && "$1 $2 $3 $4" == "--assess --type exec --verbose=2" ]]
[[ "$5" == build/TCFSProvider.app || "$5" == "$TCFS_GATE_ROOT/"*/TCFSDaemon.app ]]
printf 'gatekeeper-assess\n' >> "$TCFS_GATE_LOG"
[[ "$TCFS_GATE_MODE" != assessment-failed ]]
EOF
  chmod +x "$gate_tools"/*

  local kind mode modes expectation case_root identity certificate apple_id result
  local cases=0
  for kind in import sign cli fileprovider daemon; do
    case "$kind" in
      import) modes="missing-certificate no-identity accepted" ;;
      sign) modes="missing-identity ad-hoc" ;;
      cli) modes="missing-identity ad-hoc missing-credential failed invalid pending missing-status missing-id invalid-id malformed wrong-shape oversized accepted" ;;
      *) modes="missing-identity ad-hoc missing-credential signature-failed failed invalid pending missing-status missing-id invalid-id malformed wrong-shape oversized staple-failed staple-validation-failed assessment-failed accepted" ;;
    esac
    if [[ "$kind" == daemon ]]; then
      modes="$modes no-identity ambiguous-identity packager-failed"
    fi
    for mode in $modes; do
      cases=$((cases + 1))
      case_root="$gate_root/$kind-$mode"
      mkdir -p "$case_root/work/build/TCFSProvider.app" "$case_root/runner"
      mkdir -p "$case_root/work/tcfs-9.8.7-macos-aarch64"
      mkdir -p "$case_root/work/swift/fileprovider" "$case_root/work/swift/daemon"
      mkdir -p "$case_root/work/target/aarch64-apple-darwin/release"
      cp "$REPO_ROOT/swift/fileprovider/notarize.sh" "$case_root/work/swift/fileprovider/notarize.sh"
      printf 'native fixture input\n' > "$case_root/work/target/aarch64-apple-darwin/release/tcfsd"
      printf 'owned keychain fixture\n' > "$case_root/runner/cli-signing.keychain-db"
      # Existing check_daemon_packaging_contract exercises the actual8arg packager.
      # This fake checks this workflow's exact integration without signing anything.
      cat > "$case_root/work/swift/daemon/build.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
python3 - "$@" <<'PY'
import hashlib, os, pathlib, sys
assert len(sys.argv) == 9
binary, output, certificate, team, keychain, digest, revision, version = sys.argv[1:]
assert certificate == '1' * 40 and team == 'TESTTEAMID'
assert pathlib.Path(keychain).read_bytes() == b'owned keychain fixture\n'
assert pathlib.Path(binary).read_bytes() == b'native fixture input\n'
assert hashlib.sha256(pathlib.Path(binary).read_bytes()).hexdigest() == digest
assert revision == 'f' * 40 and version == '9.8.7'
parent = pathlib.Path(output)
assert str(parent).startswith(os.environ['TCFS_GATE_ROOT'] + '/')
with open(os.environ['TCFS_GATE_LOG'], 'a') as log:
    log.write('package-daemon\n')
if os.environ['TCFS_GATE_MODE'] == 'packager-failed':
    raise SystemExit(23)
(parent / 'TCFSDaemon.app').mkdir(parents=True)
PY
EOF
      : > "$case_root/log"
      : > "$case_root/github-env"
      identity="Developer ID Application: Fixture (TESTTEAMID)"
      certificate="Zml4dHVyZQ=="
      apple_id="fixture@example.invalid"
      [[ "$mode" != "missing-identity" ]] || identity=""
      [[ "$mode" != "ad-hoc" ]] || identity="-"
      [[ "$mode" != "missing-certificate" ]] || certificate=""
      [[ "$mode" != "missing-credential" ]] || apple_id=""
      expectation="failure"
      [[ "$mode" != "accepted" ]] || expectation="success"
      if (
        cd "$case_root/work"
        env -i PATH="$gate_tools:$PATH" \
          RUNNER_TEMP="$case_root/runner" GITHUB_ENV="$case_root/github-env" \
          TCFS_GATE_ROOT="$gate_root" TCFS_GATE_LOG="$case_root/log" TCFS_GATE_MODE="$mode" \
          APPLE_CERTIFICATE_BASE64="$certificate" APPLE_CERTIFICATE_PASSWORD="fixture" \
          APPLE_ID="$apple_id" APPLE_TEAM_ID="TESTTEAMID" APPLE_NOTARIZE_PASSWORD="fixture" \
          CLI_SIGNING_IDENTITY="$identity" SIGNING_IDENTITY="$identity" \
          CLI_KEYCHAIN_PATH="$case_root/runner/cli-signing.keychain-db" \
          TCFS_RELEASE_DITTO="$gate_tools/ditto" TCFS_RELEASE_CODESIGN="$gate_tools/codesign" \
          TCFS_RELEASE_SPCTL="$gate_tools/spctl" \
          bash --noprofile --norc -e -o pipefail "$gate_root/$kind.sh"
      ) > "$case_root/stdout" 2> "$case_root/stderr"; then
        result="success"
      else
        result="failure"
      fi
      if [[ "$result" != "$expectation" ]]; then
        printf 'release gate %s/%s: expected %s, got %s\n' "$kind" "$mode" "$expectation" "$result" >&2
        cat "$case_root/stdout" "$case_root/stderr" >&2
        return 1
      fi
      if [[ "$kind" == daemon && "$mode" != missing-identity && "$mode" != ad-hoc && "$mode" != missing-credential && "$mode" != no-identity && "$mode" != ambiguous-identity ]]; then
        if ! grep -Fq 'package-daemon' "$case_root/log"; then
          cat "$case_root/stdout" "$case_root/stderr" >&2
          echo 'Daemon workflow did not reach the expected owned packager.' >&2
          return 1
        fi
      fi
      if [[ "$mode" == signature-failed ]]; then
        assert_contains "$case_root/log" 'signature-verify'
      fi
      if [[ "$kind" == "import" ]]; then
        if [[ "$mode" == "missing-certificate" ]]; then
          [[ ! -s "$case_root/log" && ! -s "$case_root/github-env" ]]
        elif [[ "$mode" == "no-identity" ]]; then
          assert_contains "$case_root/github-env" "CLI_KEYCHAIN_PATH=$case_root/runner/cli-signing.keychain-db"
          assert_not_contains "$case_root/github-env" "CLI_SIGNING_IDENTITY="
        else
          assert_contains "$case_root/github-env" "CLI_SIGNING_IDENTITY=$identity"
        fi
      elif [[ "$kind" == "sign" || "$mode" == "missing-identity" || "$mode" == "ad-hoc" || "$mode" == "missing-credential" ]]; then
        [[ ! -s "$case_root/log" ]]
      elif [[ "$mode" == "accepted" ]]; then
        if [[ "$kind" == fileprovider ]]; then
          printf 'signature-verify\narchive\nnotary-submit\nstaple\nstaple-validate\ngatekeeper-assess\n' > "$case_root/expected-log"
        elif [[ "$kind" == daemon ]]; then
          printf 'security-find-identity\npackage-daemon\nsignature-verify\narchive\nnotary-submit\nstaple\nstaple-validate\ngatekeeper-assess\narchive\n' > "$case_root/expected-log"
          [[ -s "$case_root/work/TCFSDaemon-9.8.7-macos-aarch64.zip" ]]
        else
          printf 'archive\nnotary-submit\n' > "$case_root/expected-log"
          [[ ! -e "$case_root/work/tcfs-9.8.7-macos-aarch64-notarize.zip" ]]
        fi
        cmp "$case_root/expected-log" "$case_root/log"
      elif [[ "$mode" == no-identity || "$mode" == ambiguous-identity || "$mode" == packager-failed || "$mode" == signature-failed ]]; then
        assert_not_contains "$case_root/log" 'notary-submit'
      elif [[ "$mode" != staple-failed && "$mode" != staple-validation-failed && "$mode" != assessment-failed ]]; then
        assert_contains "$case_root/log" "notary-submit"
        assert_not_contains "$case_root/log" "staple"
        assert_not_contains "$case_root/log" "gatekeeper-assess"
      fi
      # No failed app/notary/signature/assessment path produces final evidence or daemon ZIP.
      local evidence
      case "$kind" in
        cli) evidence="$case_root/work/tcfs-9.8.7-macos-aarch64.notarization.json" ;;
        fileprovider) evidence="$case_root/work/build/TCFSProvider-9.8.7-macos-aarch64.notarization.json" ;;
        daemon) evidence="$case_root/work/TCFSDaemon-9.8.7-macos-aarch64.notarization.json" ;;
        *) evidence="" ;;
      esac
      if [[ -n "$evidence" ]]; then
        if [[ "$mode" == accepted ]]; then
          python3 - "$evidence" <<'PY'
import json, pathlib, sys
raw = pathlib.Path(sys.argv[1]).read_bytes()
assert len(raw) <= 65536
assert json.loads(raw) == {'status': 'Accepted', 'id': '11111111-2222-4333-8444-555555555555'}
PY
        else
          [[ ! -e "$evidence" ]]
          [[ "$kind" != daemon || ! -e "$case_root/work/TCFSDaemon-9.8.7-macos-aarch64.zip" ]]
        fi
      fi
    done
  done
  printf 'release Apple failure-path execution cases: %s passed\n' "$cases"
}

check_release_evidence_publication() {
  local gate_root="${TMPDIR}/release-evidence-publication"
  mkdir -p "$gate_root"
  extract_step "create-release" "Flatten artifacts and generate checksums" "$gate_root/publish.sh"
  ruby -e '
    path = ARGV.fetch(0)
    text = File.read(path).gsub("${{ needs.plan.outputs.version }}", "__TCFS_FIXTURE_VERSION__")
    raise "unrendered GitHub expression" if text.include?("${{")
    File.write(path, text)
  ' "$gate_root/publish.sh"
  # Execute the actual publication gate with owned archives and exact raw Apple JSON.
  # No signing, notary service, network, package installer or live executable is used.
  python3 - "$gate_root" <<'PY'
import hashlib, json, os, pathlib, subprocess, sys
root = pathlib.Path(sys.argv[1])
pairs = [(f'{kind}-9.8.7-{platform}', suffix)
         for platform in ('macos-aarch64', 'macos-x86_64')
         for kind, suffix in [('tcfs', '.tar.gz'), ('TCFSDaemon', '.zip')]]
pairs.append(('TCFSProvider-9.8.7-macos-aarch64', '.zip'))
evidence_bytes = b'{"status":"Accepted","id":"11111111-2222-4333-8444-555555555555"}\n'
names = [stem + suffix for stem, suffix in pairs] + [stem + '.notarization.json' for stem, _ in pairs]
cases = ['accepted', 'prerelease', 'prerelease-wrong-evidence-pair', 'prerelease-missing-daemon', 'invalid-version'] + ['missing-' + name for name in names]
cases += ['invalid', 'pending', 'malformed', 'oversized', 'wrong-shape', 'missing-id', 'invalid-id',
          'empty-archive', 'duplicate', 'unexpected-evidence', 'symlink']
for index, mode in enumerate(cases):
    version = '9.8.7-rc.1' if mode.startswith('prerelease') else ('9.8' if mode == 'invalid-version' else '9.8.7')
    pairs = [(f'{kind}-{version}-{platform}', suffix)
             for platform in ('macos-aarch64', 'macos-x86_64')
             for kind, suffix in [('tcfs', '.tar.gz'), ('TCFSDaemon', '.zip')]]
    pairs.append((f'TCFSProvider-{version}-macos-aarch64', '.zip'))
    names = [stem + suffix for stem, suffix in pairs] + [stem + '.notarization.json' for stem, _ in pairs]
    case = root / str(index)
    source = case / 'dist' / 'owned'
    source.mkdir(parents=True)
    for stem, suffix in pairs:
        (source / (stem + suffix)).write_bytes(b'owned final artifact ' + stem.encode())
        (source / (stem + '.notarization.json')).write_bytes(evidence_bytes)
    selected = source / (pairs[0][0] + '.notarization.json')
    if mode == 'prerelease-wrong-evidence-pair':
        selected.rename(source / selected.name.replace('-rc.1', ''))
    elif mode == 'prerelease-missing-daemon':
        (source / (pairs[1][0] + '.zip')).unlink()
    if mode.startswith('missing-') and mode != 'missing-id':
        (source / mode.removeprefix('missing-')).unlink()
    elif mode == 'invalid': selected.write_text('{"status":"Invalid"}')
    elif mode == 'pending': selected.write_text('{"status":"In Progress"}')
    elif mode == 'malformed': selected.write_text('not JSON')
    elif mode == 'oversized': selected.write_bytes(b' ' * 65537 + evidence_bytes)
    elif mode == 'wrong-shape': selected.write_text('[{"status":"Accepted"}]')
    elif mode == 'missing-id': selected.write_text('{"status":"Accepted"}')
    elif mode == 'invalid-id': selected.write_text('{"status":"Accepted","id":"invalid"}')
    elif mode == 'empty-archive': (source / names[0]).write_bytes(b'')
    elif mode == 'duplicate':
        other = case / 'dist' / 'duplicate'
        other.mkdir()
        (other / names[0]).write_bytes(b'conflicting duplicate')
    elif mode == 'unexpected-evidence': (source / 'unexpected.notarization.json').write_bytes(evidence_bytes)
    elif mode == 'symlink':
        selected.unlink()
        selected.symlink_to(source / (pairs[1][0] + '.notarization.json'))
    template = (root / 'publish.sh').read_text()
    assert template.count('__TCFS_FIXTURE_VERSION__') == 1
    (case / 'publish.sh').write_text(template.replace('__TCFS_FIXTURE_VERSION__', version))
    result = subprocess.run(['bash', '--noprofile', '--norc', '-e', '-o', 'pipefail', str(case / 'publish.sh')],
                            cwd=case, env={'PATH': os.environ['PATH']}, capture_output=True, timeout=10)
    (case / 'stdout').write_bytes(result.stdout)
    (case / 'stderr').write_bytes(result.stderr)
    if mode in ('accepted', 'prerelease'):
        assert result.returncode == 0, (mode, result.stderr.decode())
        published = case / 'release'
        assert {p.name for p in published.iterdir()} == set(names) | {'SHA256SUMS.txt'}
        checksums = {}
        for line in (published / 'SHA256SUMS.txt').read_text().splitlines():
            digest, name = line.split('  ', 1)
            checksums[name] = digest
        assert set(checksums) == set(names)
        for name in names:
            raw = (published / name).read_bytes()
            assert raw == (source / name).read_bytes(), name
            assert checksums[name] == hashlib.sha256(raw).hexdigest(), name
    else:
        assert result.returncode != 0, mode
        assert not (case / 'release').exists(), mode
print('release evidence publication/refusal cases:', len(cases), 'passed')
PY
  ruby -ryaml -e '
    jobs = YAML.load_file(ARGV[0]).fetch("jobs")
    binaries = jobs.fetch("build-binaries").fetch("steps")
    daemon = binaries.find { |step| step["name"] == "Package and notarize daemon app (macOS)" }
    raise "daemon must retain existing Darwin signing condition" unless daemon.fetch("if") == "matrix.sign"
    raise "daemon must call8arg packager" unless daemon.fetch("run").include?("bash swift/daemon/build.sh")
    raise "daemon identity cannot be replaced by automatic selection" unless daemon.fetch("run").include?("name == sys.argv[2]")
    upload = binaries.find { |step| step["name"] == "Upload artifacts" }.fetch("with")
    raise "CLI/daemon evidence missing from artifact upload" unless upload.fetch("path").include?("*.notarization.json")
    provider = jobs.fetch("build-fileprovider").fetch("steps").find { |step| step["name"] == "Upload artifact" }.fetch("with")
    raise "FileProvider evidence missing from artifact upload" unless provider.fetch("path").include?("*.notarization.json")
    release = jobs.fetch("create-release")
    raise "publication must require successful producer jobs" unless release.fetch("if").include?("success()")
    steps = release.fetch("steps")
    flat = steps.index { |step| step["name"] == "Flatten artifacts and generate checksums" }
    sign = steps.index { |step| step["name"] == "Sign checksums with Cosign (keyless)" }
    publish = steps.index { |step| step["name"] == "Create release" }
    raise "validation must precede Cosign and publication" unless flat < sign && sign < publish
    run = steps.fetch(sign).fetch("run")
    raise "existing signed checksum format changed" unless run.include?("--output-signature SHA256SUMS.txt.sig") && run.include?("--output-certificate SHA256SUMS.txt.pem")
    raise "publication must include evidence/checksums" unless steps.fetch(publish).fetch("with").fetch("files") == "release/*"
  ' "$WORKFLOW"
}

check_daemon_packaging_contract() {
  # Exercise the actual packager with owned bytes and a fake codesign tool.
  # These cases prove assembly/refusal behavior, never an Apple signature.
  python3 - "$REPO_ROOT" "$TMPDIR/daemon-packaging" <<'PY'
import hashlib
import json
import os
import plistlib
import shutil
import subprocess
import sys
from pathlib import Path

repo = Path(sys.argv[1])
root = Path(sys.argv[2])
root.mkdir(mode=0o700)
certificate = "A" * 40
team = "TESTTEAMID"
revision = "b" * 40
version = "9.8.7"
input_bytes = b"owned synthetic daemon input; not a Mach-O executable\n"
input_hash = hashlib.sha256(input_bytes).hexdigest()
fake = root / "codesign"
fake.write_text("#!" + sys.executable + "\n" + r"""
import json, os, sys
from pathlib import Path
args = sys.argv[1:]
mode = os.environ['TCFS_DAEMON_FIXTURE_MODE']
with open(os.environ['TCFS_DAEMON_FIXTURE_LOG'], 'a') as log:
    log.write(json.dumps(args) + '\n')
app = Path(args[-1])
assert app.is_dir() and app.name == 'TCFSDaemon.app'
if args[0] == '--force':
    expected = ['--force', '--sign', 'A' * 40, '--keychain',
                os.environ['TCFS_DAEMON_FIXTURE_KEYCHAIN'], '--identifier',
                'io.tinyland.tcfsd', '--options', 'runtime', '--timestamp',
                '--entitlements', str(app.parent / 'tcfsd.entitlements'), str(app)]
    assert args == expected, args
    if mode == 'sign-failed':
        raise SystemExit(31)
elif args[0] == '--verify':
    assert args[:3] == ['--verify', '--strict', '--test-requirement'] and len(args) == 5
    expected = ('identifier "io.tinyland.tcfsd" and anchor apple generic '
                'and certificate 1[field.1.2.840.113635.100.6.2.6] exists '
                'and certificate leaf[field.1.2.840.113635.100.6.1.13] exists '
                'and certificate leaf[subject.OU] = "TESTTEAMID" '
                'and certificate leaf = H"' + 'A' * 40 + '"')
    assert args[3] == expected, args[3]
    if mode == 'verify-failed':
        raise SystemExit(32)
else:
    assert args[:2] == ['--display', '--verbose=4'] and len(args) == 3
    identifier = 'io.tinyland.other' if mode == 'wrong-identifier' else 'io.tinyland.tcfsd'
    team = 'OTHERTEAM1' if mode == 'wrong-team' else 'TESTTEAMID'
    flags = '10002' if mode == 'adhoc-result' else ('0' if mode == 'no-runtime' else '10000')
    lines = ['Identifier=' + identifier, 'TeamIdentifier=' + team,
             'CodeDirectory v=20500 size=123 flags=0x' + flags + ' hashes=1+1 location=embedded']
    if mode != 'no-timestamp':
        lines.append('Timestamp=Sep 8, 2026 at 12:00:00 AM')
    if mode == 'duplicate-timestamp':
        lines.append('Timestamp=Sep 8, 2026 at 12:00:00 AM')
    if mode == 'display-failed':
        raise SystemExit(33)
    print('\n'.join(lines), file=sys.stderr)
""")
fake.chmod(0o700)

modes = (
    'success', 'old-caller', 'auto', 'ad-hoc', 'zero-certificate', 'bad-team',
    'wrong-hash', 'zero-hash', 'bad-source', 'zero-source', 'bad-version',
    'missing-keychain', 'symlink-keychain', 'missing-input', 'symlink-input',
    'empty-input', 'existing-output', 'symlink-output', 'wrong-template-id',
    'wrong-template-version', 'sign-failed', 'verify-failed', 'display-failed',
    'wrong-identifier', 'wrong-team', 'adhoc-result', 'no-runtime',
    'no-timestamp', 'duplicate-timestamp', 'prerelease-rc', 'prerelease-beta',
    'bad-version-components', 'bad-version-empty-prerelease', 'bad-version-separator', 'bad-version-metadata',
)
late = {'sign-failed', 'verify-failed', 'display-failed', 'wrong-identifier',
        'wrong-team', 'adhoc-result', 'no-runtime', 'no-timestamp', 'duplicate-timestamp'}
success_modes = {'success', 'prerelease-rc', 'prerelease-beta'}
for mode in modes:
    case = root / mode
    case.mkdir()
    source = case / 'daemon'
    shutil.copytree(repo / 'swift/daemon', source)
    binary = case / 'tcfsd'
    binary.write_bytes(input_bytes)
    keychain = case / 'existing.keychain-db'
    keychain.write_bytes(b'owned synthetic keychain metadata fixture\n')
    keychain_hash = hashlib.sha256(keychain.read_bytes()).hexdigest()
    output = case / 'output'
    log = case / 'calls.jsonl'
    values = [str(binary), str(output), certificate, team, str(keychain), input_hash, revision, version]
    if mode in ('auto', 'ad-hoc', 'zero-certificate'):
        values[2] = {'auto': 'auto', 'ad-hoc': '-', 'zero-certificate': '0' * 40}[mode]
    elif mode == 'bad-team':
        values[3] = 'invalid'
    elif mode in ('wrong-hash', 'zero-hash'):
        values[5] = 'c' * 64 if mode == 'wrong-hash' else '0' * 64
    elif mode in ('bad-source', 'zero-source'):
        values[6] = 'HEAD' if mode == 'bad-source' else '0' * 40
    elif mode == 'bad-version':
        values[7] = '9.8.7;false'
    elif mode in ('prerelease-rc', 'prerelease-beta'):
        values[7] = {'prerelease-rc': '9.8.7-rc1', 'prerelease-beta': '9.8.7-beta.2'}[mode]
    elif mode.startswith('bad-version-'):
        values[7] = {'bad-version-components': '9.8', 'bad-version-empty-prerelease': '9.8.7-',
                     'bad-version-separator': '9.8.7-rc/1', 'bad-version-metadata': '9.8.7+build'}[mode]
    elif mode == 'missing-keychain':
        values[4] = str(case / 'missing.keychain-db')
    elif mode == 'symlink-keychain':
        link = case / 'keychain-link'; link.symlink_to(keychain); values[4] = str(link)
    elif mode == 'missing-input':
        values[0] = str(case / 'missing-input')
    elif mode == 'symlink-input':
        link = case / 'input-link'; link.symlink_to(binary); values[0] = str(link)
    elif mode == 'empty-input':
        binary.write_bytes(b'')
    elif mode == 'existing-output':
        output.mkdir(); (output / 'marker').write_bytes(b'preserve\n')
    elif mode == 'symlink-output':
        other = case / 'other'; other.mkdir(); (other / 'marker').write_bytes(b'preserve\n'); output.symlink_to(other)
    elif mode in ('wrong-template-id', 'wrong-template-version'):
        info_path = source / 'resources/Info.plist'
        info = plistlib.loads(info_path.read_bytes())
        info['CFBundleIdentifier' if mode == 'wrong-template-id' else 'CFBundleVersion'] = 'incorrect'
        info_path.write_bytes(plistlib.dumps(info))
    elif mode == 'old-caller':
        values = values[:2] + ['auto']
    environment = {
        'PATH': os.environ['PATH'],
        'TCFS_DAEMON_CODESIGN': str(fake),
        'TCFS_DAEMON_FIXTURE_MODE': mode,
        'TCFS_DAEMON_FIXTURE_LOG': str(log),
        'TCFS_DAEMON_FIXTURE_KEYCHAIN': str(keychain),
    }
    result = subprocess.run(['bash', '--noprofile', '--norc', str(source / 'build.sh'), *values],
                            env=environment, capture_output=True, timeout=20, check=False)
    (case / 'stdout').write_bytes(result.stdout)
    (case / 'stderr').write_bytes(result.stderr)
    assert (result.returncode == 0) == (mode in success_modes), (mode, result.returncode, result.stderr)
    assert hashlib.sha256(keychain.read_bytes()).hexdigest() == keychain_hash, mode
    if mode != 'empty-input':
        assert binary.read_bytes() == input_bytes, mode
    calls = [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []
    if mode not in late and mode not in success_modes:
        assert not calls, (mode, calls)
        if mode in ('existing-output', 'symlink-output'):
            assert (output / 'marker').read_bytes() == b'preserve\n'
            assert sorted(path.name for path in output.iterdir()) == ['marker']
        else:
            assert not output.exists(), mode
    else:
        expected_count = 1 if mode == 'sign-failed' else (2 if mode == 'verify-failed' else 3)
        assert len(calls) == expected_count, (mode, calls)
        assert (output / 'TCFSDaemon.app/Contents/MacOS/tcfsd').read_bytes() == input_bytes
        assert ('Signed intermediate:' in result.stdout.decode()) == (mode in success_modes)
        info = plistlib.loads((output / 'TCFSDaemon.app/Contents/Info.plist').read_bytes())
        assert info['CFBundleIdentifier'] == 'io.tinyland.tcfsd'
        assert info['CFBundleVersion'] == info['CFBundleShortVersionString'] == values[7].partition('-')[0]
        assert info['TCFSReleaseVersion'] == values[7]
        assert info['TCFSSourceRevision'] == revision and info['TCFSInputSHA256'] == input_hash
        expected_entitlements = (repo / 'swift/daemon/resources/tcfsd.entitlements').read_bytes().replace(
            b'$(TeamIdentifierPrefix)', (team + '.').encode('ascii'))
        assert (output / 'tcfsd.entitlements').read_bytes() == expected_entitlements
print('daemon packaging assembly/refusal cases:', len(modes), 'passed (fake codesign only)')
PY
}

check_macos_fileprovider_principal_class() {
  local plist="$REPO_ROOT/swift/fileprovider/resources/Extension-Info.plist"
  local source="$REPO_ROOT/swift/fileprovider/Sources/Extension/FileProviderExtension.swift"

  assert_contains "$plist" "<string>TCFSFileProvider.TCFSFileProviderExtension</string>"
  if grep -Fq "@objc(TCFSFileProviderExtension)" "$source"; then
    printf 'macOS FileProvider principal class should use the Swift module name, not a custom @objc runtime name\n' >&2
    exit 1
  fi
}

check_testing_mode_is_explicit_opt_in() {
  local build_script="$REPO_ROOT/swift/fileprovider/build.sh"
  local host_app="$REPO_ROOT/swift/fileprovider/Sources/HostApp/HostApp.swift"

  assert_contains "$build_script" "TCFS_FILEPROVIDER_TESTING_MODE_ENTITLEMENT"
  assert_contains "$build_script" "com.apple.developer.fileprovider.testing-mode"
  assert_contains "$build_script" "com.apple.security.temporary-exception.files.home-relative-path.read-write"
  assert_contains "$build_script" "/Library/Application Support/io.tinyland.tcfs/"
  assert_contains "$host_app" "policyProbe: main entered"
  assert_contains "$host_app" "policyProbe: domain created"
  assert_contains "$host_app" "policyProbe: OK"

  if grep -Fq "com.apple.developer.fileprovider.testing-mode" \
    "$REPO_ROOT/swift/fileprovider/resources/HostApp.entitlements" \
    "$REPO_ROOT/swift/fileprovider/resources/Extension.entitlements"; then
    printf 'FileProvider testing-mode entitlement must stay out of default production entitlements\n' >&2
    exit 1
  fi
  if grep -Fq "com.apple.security.temporary-exception.files.home-relative-path.read-write" \
    "$REPO_ROOT/swift/fileprovider/resources/Extension.entitlements"; then
    printf 'FileProvider temporary path exception must stay out of default production entitlements\n' >&2
    exit 1
  fi
}

check_testing_mode_package_workflow() {
  assert_contains "$POSTINSTALL_WORKFLOW" "runner_label:"
  assert_contains "$POSTINSTALL_WORKFLOW" 'default: "macos-15"'
  assert_contains "$POSTINSTALL_WORKFLOW" "fileprovider_testing_mode=true requires a registered self-hosted Mac runner label"
  assert_contains "$TESTING_MODE_PKG_WORKFLOW" "runner_label:"
  assert_contains "$TESTING_MODE_PKG_WORKFLOW" 'default: "petting-zoo-mini"'
  assert_contains "$TESTING_MODE_PKG_WORKFLOW" "auto-development"
  assert_contains "$TESTING_MODE_PKG_WORKFLOW" "GIT_CONFIG_GLOBAL: /tmp/tcfs-fileprovider-lab-gitconfig"
  assert_contains "$TESTING_MODE_PKG_WORKFLOW" "GIT_CONFIG_NOSYSTEM"
  assert_contains "$TESTING_MODE_PKG_WORKFLOW" "signing_keychain:"
  assert_contains "$TESTING_MODE_PKG_WORKFLOW" "TCFS_CODESIGN_KEYCHAIN"
  assert_contains "$TESTING_MODE_PKG_WORKFLOW" "Apple/Mac development"
  assert_contains "$TESTING_MODE_PKG_WORKFLOW" "signing_p12_password_file"
  assert_contains "$TESTING_MODE_PKG_WORKFLOW" "--require-host-entitlement com.apple.developer.fileprovider.testing-mode"
  assert_contains "$TESTING_MODE_PKG_WORKFLOW" "com.apple.developer.fileprovider.testing-mode"
  assert_contains "$TESTING_MODE_PKG_WORKFLOW" "TCFS_FILEPROVIDER_TESTING_MODE_ENTITLEMENT=1"
  assert_contains "$TESTING_MODE_PKG_WORKFLOW" "TCFS_CODESIGN_TIMESTAMP=0"
  assert_contains "$TESTING_MODE_PKG_WORKFLOW" "TCFS_FILEPROVIDER_HOST_POLICY_PROBE_ONLY=1"
  assert_contains "$TESTING_MODE_PKG_WORKFLOW" "fileprovider-testing-policy-"
  assert_contains "$TESTING_MODE_PKG_WORKFLOW" "tcfs-\${VERSION}-macos-aarch64-fileprovider-testing-mode.pkg"
  assert_contains "$TESTING_MODE_PKG_WORKFLOW" "dist-testing-mode-pkg"
  assert_contains "$TESTING_MODE_PKG_WORKFLOW" "releases/download/\${TAG}/tcfs-\${VERSION}-macos-aarch64.tar.gz"
  assert_contains "$TESTING_MODE_PKG_WORKFLOW" "scripts/macos-build-pkg.sh"
  assert_contains "$TESTING_MODE_PKG_WORKFLOW" "scripts/macos-pkg-structure-smoke.sh"
  assert_not_contains "$TESTING_MODE_PKG_WORKFLOW" "APPLE_CERTIFICATE_BASE64"
  assert_not_contains "$TESTING_MODE_PKG_WORKFLOW" "APPLE_INSTALLER_CERTIFICATE_BASE64"
  assert_not_contains "$TESTING_MODE_PKG_WORKFLOW" "APPLE_NOTARIZE_PASSWORD"
  assert_not_contains "$TESTING_MODE_PKG_WORKFLOW" "notarytool"

  local validate_step="${TMPDIR}/testing-mode-validate-inputs-and-runner.sh"
  local resolve_assets_step="${TMPDIR}/testing-mode-resolve-assets.sh"
  local expose_rustup_step="${TMPDIR}/testing-mode-expose-rustup.sh"
  local build_app_step="${TMPDIR}/testing-mode-build-fileprovider-app.sh"
  local verify_signing_step="${TMPDIR}/testing-mode-verify-signing.sh"
  local probe_policy_step="${TMPDIR}/testing-mode-probe-policy.sh"
  local download_cli_step="${TMPDIR}/testing-mode-download-cli-tarball.sh"
  local build_pkg_step="${TMPDIR}/testing-mode-build-pkg.sh"
  local verify_pkg_step="${TMPDIR}/testing-mode-verify-package.sh"

  extract_step_from_workflow \
    "$TESTING_MODE_PKG_WORKFLOW" \
    "build-testing-mode-pkg" \
    "Validate inputs and runner" \
    "$validate_step"
  bash -n "$validate_step"
  assert_contains "$validate_step" "FileProvider testing-mode must run on a registered self-hosted Mac"
  assert_contains "$validate_step" "TCFS_FILEPROVIDER_TESTING_MODE_ENTITLEMENT=1"
  assert_contains "$validate_step" "TCFS_CODESIGN_TIMESTAMP=0"

  extract_step_from_workflow \
    "$TESTING_MODE_PKG_WORKFLOW" \
    "build-testing-mode-pkg" \
    "Prepare Rust toolchain manager" \
    "$expose_rustup_step"
  bash -n "$expose_rustup_step"
  # shellcheck disable=SC2016 # Intentional literal assertions against workflow shell.
  assert_contains "$expose_rustup_step" '$HOME/.cargo/bin/rustup'
  assert_contains "$expose_rustup_step" "--no-modify-path"
  # shellcheck disable=SC2016 # Intentional literal assertions against workflow shell.
  assert_contains "$expose_rustup_step" '$GITHUB_PATH'

  extract_step_from_workflow \
    "$TESTING_MODE_PKG_WORKFLOW" \
    "build-testing-mode-pkg" \
    "Resolve local development signing assets" \
    "$resolve_assets_step"
  bash -n "$resolve_assets_step"
  assert_contains "$resolve_assets_step" "Apple/Mac development"
  assert_contains "$resolve_assets_step" "Mac Developer"
  assert_contains "$resolve_assets_step" "find_identities"
  assert_contains "$resolve_assets_step" "signing_keychain does not exist"
  assert_contains "$resolve_assets_step" "signing_p12_path does not exist"
  assert_contains "$resolve_assets_step" "signing_p12_password_file does not exist"
  # shellcheck disable=SC2016 # Intentional literal assertion against workflow shell.
  assert_contains "$resolve_assets_step" 'security import "$SIGNING_P12_PATH"'
  # shellcheck disable=SC2016 # Intentional literal assertion against workflow shell.
  assert_contains "$resolve_assets_step" 'security list-keychains -d user -s "$SIGNING_KEYCHAIN"'
  # shellcheck disable=SC2016 # Intentional literal assertion against workflow shell.
  assert_contains "$resolve_assets_step" 'match($0, /"[^"]+"/)'
  assert_contains "$resolve_assets_step" "security unlock-keychain"
  assert_contains "$resolve_assets_step" "security set-key-partition-list"
  assert_contains "$resolve_assets_step" "codesign cannot use its private key noninteractively"
  assert_contains "$resolve_assets_step" "apple-tool:, apple:, and codesign:"
  assert_contains "$resolve_assets_step" "No local host/extension provisioning profile pair grants FileProvider testing mode"
  assert_contains "$resolve_assets_step" "--require-host-entitlement com.apple.developer.fileprovider.testing-mode"
  assert_contains "$resolve_assets_step" "TCFS_FILEPROVIDER_TESTING_MODE_ENTITLEMENT=1"
  assert_contains "$resolve_assets_step" "TCFS_CODESIGN_TIMESTAMP=0"

  extract_step_from_workflow \
    "$TESTING_MODE_PKG_WORKFLOW" \
    "build-testing-mode-pkg" \
    "Build FileProvider app" \
    "$build_app_step"
  bash -n "$build_app_step"
  assert_contains "$build_app_step" "swift/fileprovider/build.sh"

  extract_step_from_workflow \
    "$TESTING_MODE_PKG_WORKFLOW" \
    "build-testing-mode-pkg" \
    "Verify FileProvider signing and testing entitlement" \
    "$verify_signing_step"
  bash -n "$verify_signing_step"
  assert_contains "$verify_signing_step" "scripts/macos-fileprovider-preflight.sh"
  assert_contains "$verify_signing_step" "com.apple.developer.fileprovider.testing-mode"

  extract_step_from_workflow \
    "$TESTING_MODE_PKG_WORKFLOW" \
    "build-testing-mode-pkg" \
    "Probe development app launch policy" \
    "$probe_policy_step"
  bash -n "$probe_policy_step"
  assert_contains "$probe_policy_step" "spctl"
  assert_contains "$probe_policy_step" "syspolicy_check"
  assert_contains "$probe_policy_step" "TCFS_FILEPROVIDER_HOST_POLICY_PROBE_ONLY=1"
  assert_contains "$probe_policy_step" "host-policy-probe.exit"

  extract_step_from_workflow \
    "$TESTING_MODE_PKG_WORKFLOW" \
    "build-testing-mode-pkg" \
    "Download release CLI tarball" \
    "$download_cli_step"
  bash -n "$download_cli_step"
  assert_contains "$download_cli_step" "releases/download/\${TAG}/tcfs-\${VERSION}-macos-aarch64.tar.gz"

  extract_step_from_workflow \
    "$TESTING_MODE_PKG_WORKFLOW" \
    "build-testing-mode-pkg" \
    "Build testing-mode .pkg" \
    "$build_pkg_step"
  bash -n "$build_pkg_step"
  assert_contains "$build_pkg_step" "scripts/macos-build-pkg.sh"
  assert_not_contains "$build_pkg_step" "--sign"

  extract_step_from_workflow \
    "$TESTING_MODE_PKG_WORKFLOW" \
    "build-testing-mode-pkg" \
    "Verify testing-mode package structure" \
    "$verify_pkg_step"
  bash -n "$verify_pkg_step"
  assert_not_contains "$verify_pkg_step" "--require-signature"
  assert_contains "$verify_pkg_step" "--expected-postinstall scripts/macos-pkg-postinstall.sh"
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
check_postinstall_workflow_artifact_download_uses_api_zip
check_release_action_token_override
check_release_gates_and_apple_signing_requirements
check_release_apple_failure_paths
check_release_evidence_publication
check_daemon_packaging_contract
check_macos_fileprovider_principal_class
check_testing_mode_is_explicit_opt_in
check_testing_mode_package_workflow

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
        if [[ -n "${TCFS_PROFILE_DECODE_LOG:-}" ]]; then
          printf '%s\n' "$2" >>"$TCFS_PROFILE_DECODE_LOG"
        fi
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
cat >"$FAKE_BIN/PlistBuddy" <<'EOF'
#!/usr/bin/env python3
import os
from pathlib import Path
import plistlib
import sys

assert len(sys.argv) == 4 and sys.argv[1] == "-c"
assert sys.argv[2].startswith("Print :")
root = Path(os.environ["TCFS_PROFILE_FIXTURE_ROOT"]).resolve()
path = Path(sys.argv[3]).resolve()
assert root in path.parents
with path.open("rb") as source:
    value = plistlib.load(source)
try:
    for key in sys.argv[2][len("Print :"):].split(":"):
        value = value[int(key)] if isinstance(value, list) else value[key]
except (IndexError, KeyError, TypeError, ValueError):
    raise SystemExit(1)
if isinstance(value, bool):
    print("true" if value else "false")
elif isinstance(value, (str, int)):
    print(value)
else:
    raise SystemExit(1)
EOF
cat >"$FAKE_BIN/pluginkit" <<'EOF'
#!/usr/bin/env bash
printf 'pluginkit' >>"$TCFS_FAKE_POSTINSTALL_LOG"
printf ' %q' "$@" >>"$TCFS_FAKE_POSTINSTALL_LOG"
printf '\n' >>"$TCFS_FAKE_POSTINSTALL_LOG"
EOF
cat >"$FAKE_BIN/lsregister" <<'EOF'
#!/usr/bin/env bash
printf 'lsregister' >>"$TCFS_FAKE_POSTINSTALL_LOG"
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
TCFS_PLISTBUDDY="$FAKE_BIN/PlistBuddy" \
TCFS_PROFILE_FIXTURE_ROOT="$TMPDIR" \
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

# Execute the extracted production caller against its actual inventory helper.
# A valid unrelated pair sits in every runner directory: it must never rescue
# incompatible declared inputs, nor replace the exact returned build paths.
EXACT_PAIR_CASES=0
for fault in valid bad-host-group bad-extension-group mismatched-prefix swapped-roles; do
  case_dir="$TMPDIR/exact pair $fault"
  runner="$case_dir/runner with spaces"
  mkdir -p "$runner"
  cp "$RAW_HOST_PROFILE" "$case_dir/input-host"
  cp "$RAW_EXTENSION_PROFILE" "$case_dir/input-extension"
  cp "$RAW_HOST_PROFILE" "$runner/000-decoy-host.provisionprofile"
  cp "$RAW_EXTENSION_PROFILE" "$runner/000-decoy-extension.provisionprofile"
  python3 - "$case_dir" "$fault" <<'PYPAIR'
from pathlib import Path
import plistlib
import sys
root, fault = Path(sys.argv[1]), sys.argv[2]
paths = [root / "input-host", root / "input-extension"]
profiles = [plistlib.loads(path.read_bytes()) for path in paths]
if fault in ("bad-host-group", "bad-extension-group"):
    index = 0 if fault == "bad-host-group" else 1
    profiles[index]["Entitlements"]["com.apple.security.application-groups"] = ["group.fixture.unrelated"]
elif fault == "mismatched-prefix":
    entitlements = profiles[1]["Entitlements"]
    entitlements["application-identifier"] = "OTHERTEAM1.io.tinyland.tcfs.fileprovider"
    entitlements["keychain-access-groups"] = ["OTHERTEAM1.*"]
elif fault == "swapped-roles":
    profiles.reverse()
for path, value in zip(paths, profiles):
    path.write_bytes(plistlib.dumps(value))
PYPAIR
  # Establish that the decoys are compatible in the existing inventory mode.
  env PATH="$FAKE_BIN:$PATH" TCFS_PLISTBUDDY="$FAKE_BIN/PlistBuddy" \
    TCFS_PROFILE_FIXTURE_ROOT="$TMPDIR" \
    bash "$REPO_ROOT/scripts/macos-fileprovider-profile-inventory.sh" \
      --profiles-dir "$runner" --strict >"$case_dir/decoy-proof"
  assert_contains "$case_dir/decoy-proof" "compatible pair: found"

  command=(env PATH="$FAKE_BIN:$PATH" RUNNER_TEMP="$runner"
    GITHUB_ENV="$case_dir/github-env" TCFS_PLISTBUDDY="$FAKE_BIN/PlistBuddy"
    TCFS_PROFILE_FIXTURE_ROOT="$TMPDIR" TCFS_PROFILE_DECODE_LOG="$case_dir/decoded-paths"
    TCFS_HOST_PROVISIONING_PROFILE_BASE64="$(base64_file "$case_dir/input-host")"
    TCFS_EXTENSION_PROVISIONING_PROFILE_BASE64="$(base64_file "$case_dir/input-extension")"
    bash -e "$IMPORT_STEP")
  if [[ "$fault" == valid ]]; then
    "${command[@]}" >"$case_dir/result" 2>"$case_dir/error"
    assert_contains "$case_dir/result" "profile selection: exact declared pair"
    assert_contains "$case_dir/result" "profiles scanned: 2"
    assert_contains "$case_dir/github-env" "TCFS_HOST_PROVISIONING_PROFILE=$runner/tcfs-host-developer-id.provisionprofile"
    assert_contains "$case_dir/github-env" "TCFS_EXTENSION_PROVISIONING_PROFILE=$runner/tcfs-fileprovider-developer-id.provisionprofile"
  else
    if "${command[@]}" >"$case_dir/result" 2>"$case_dir/error"; then
      printf 'incompatible declared pair was rescued: %s\n' "$fault" >&2
      exit 1
    fi
    assert_contains "$case_dir/result" "compatible pair: not found"
    [[ ! -e "$case_dir/github-env" ]] || { printf 'failed pair exported build inputs\n' >&2; exit 1; }
  fi
  python3 - "$case_dir" "$runner" <<'PYPAIR'
from pathlib import Path
import sys
root, runner = map(Path, sys.argv[1:])
host = str(runner / "tcfs-host-developer-id.provisionprofile")
extension = str(runner / "tcfs-fileprovider-developer-id.provisionprofile")
# The caller decodes the two inputs, then the real inventory checks those same
# two. Neither stage may consult either compatible decoy.
assert (root / "decoded-paths").read_text().splitlines() == [host, extension, host, extension]
PYPAIR
  EXACT_PAIR_CASES=$((EXACT_PAIR_CASES + 1))
done

pair_command=(env PATH="$FAKE_BIN:$PATH" TCFS_PLISTBUDDY="$FAKE_BIN/PlistBuddy"
  TCFS_PROFILE_FIXTURE_ROOT="$TMPDIR"
  bash "$REPO_ROOT/scripts/macos-fileprovider-profile-inventory.sh")
# Exact-pair selection is strict even without --strict, and preserves roles.
assert_fails_contains "compatible pair: not found" "${pair_command[@]}" \
  --host-profile "$RAW_EXTENSION_PROFILE" --extension-profile "$RAW_HOST_PROFILE"
assert_fails_contains "requires both profile paths" "${pair_command[@]}" \
  --host-profile "$RAW_HOST_PROFILE"
assert_fails_contains "absolute readable regular file" "${pair_command[@]}" \
  --host-profile "$TMPDIR/absent-profile" --extension-profile "$RAW_EXTENSION_PROFILE"
ln -s "$RAW_HOST_PROFILE" "$TMPDIR/profile-link"
assert_fails_contains "absolute readable regular file" "${pair_command[@]}" \
  --host-profile "$TMPDIR/profile-link" --extension-profile "$RAW_EXTENSION_PROFILE"
assert_fails_contains "cannot scan --profiles-dir" "${pair_command[@]}" \
  --host-profile "$RAW_HOST_PROFILE" --extension-profile "$RAW_EXTENSION_PROFILE" --profiles-dir "$IMPORT_RUNNER_TEMP"
assert_fails_contains "distinct files" "${pair_command[@]}" \
  --host-profile "$RAW_HOST_PROFILE" --extension-profile "$RAW_HOST_PROFILE"
assert_fails_contains "requires one nonempty value" "${pair_command[@]}" \
  --host-profile "$RAW_HOST_PROFILE" --host-profile "$RAW_HOST_PROFILE" --extension-profile "$RAW_EXTENSION_PROFILE"
EXACT_PAIR_CASES=$((EXACT_PAIR_CASES + 7))
printf 'exact profile pair contract: %s cases passed\n' "$EXACT_PAIR_CASES"

assert_fails_contains \
  "::error::TCFS_EXTENSION_PROVISIONING_PROFILE_BASE64 is required" \
  env PATH="$FAKE_BIN:$PATH" \
    RUNNER_TEMP="${TMPDIR}/missing-extension-runner" \
    GITHUB_ENV="${TMPDIR}/missing-extension-env" \
    TCFS_PLISTBUDDY="$FAKE_BIN/PlistBuddy" \
    TCFS_PROFILE_FIXTURE_ROOT="$TMPDIR" \
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

NOTARIZE_PKG_STEP="${TMPDIR}/notarize-pkg.sh"
extract_step "build-pkg" "Notarize .pkg" "$NOTARIZE_PKG_STEP"
bash -n "$NOTARIZE_PKG_STEP"
assert_contains "$NOTARIZE_PKG_STEP" "set -euo pipefail"
assert_contains "$NOTARIZE_PKG_STEP" "APPLE_ID APPLE_TEAM_ID APPLE_NOTARIZE_PASSWORD"
assert_contains "$NOTARIZE_PKG_STEP" "::error::\${required} is required for signed .pkg notarization"
assert_contains "$NOTARIZE_PKG_STEP" "xcrun notarytool submit \"\$PKG\""
assert_contains "$NOTARIZE_PKG_STEP" "xcrun stapler staple \"\$PKG\""
assert_contains "$NOTARIZE_PKG_STEP" "xcrun stapler validate -v \"\$PKG\""
assert_contains "$NOTARIZE_PKG_STEP" "scripts/macos-pkg-structure-smoke.sh"
assert_contains "$NOTARIZE_PKG_STEP" "--require-signature"
assert_contains "$NOTARIZE_PKG_STEP" "--require-gatekeeper-install"
assert_contains "$NOTARIZE_PKG_STEP" "--require-stapled-ticket"
assert_not_contains "$NOTARIZE_PKG_STEP" "non-fatal"
assert_not_contains "$NOTARIZE_PKG_STEP" "::warning::.pkg"

VERIFY_RELEASE_PKG_STEP="${TMPDIR}/verify-release-package-structure.sh"
extract_step_from_workflow \
  "$POSTINSTALL_WORKFLOW" \
  "pkg-postinstall" \
  "Verify package structure" \
  "$VERIFY_RELEASE_PKG_STEP"
bash -n "$VERIFY_RELEASE_PKG_STEP"
assert_contains "$VERIFY_RELEASE_PKG_STEP" "scripts/macos-pkg-structure-smoke.sh"
assert_contains "$VERIFY_RELEASE_PKG_STEP" "--pkg \"\$PACKAGE_PATH\""
assert_contains "$VERIFY_RELEASE_PKG_STEP" "--require-signature"
assert_contains "$VERIFY_RELEASE_PKG_STEP" "require_current_postinstall"
assert_contains "$VERIFY_RELEASE_PKG_STEP" "--allow-postinstall-mismatch"
assert_contains "$VERIFY_RELEASE_PKG_STEP" "--expected-postinstall scripts/macos-pkg-postinstall.sh"

INSTALL_PACKAGE_STEP="${TMPDIR}/install-package.sh"
extract_step_from_workflow \
  "$POSTINSTALL_WORKFLOW" \
  "pkg-postinstall" \
  "Install package" \
  "$INSTALL_PACKAGE_STEP"
bash -n "$INSTALL_PACKAGE_STEP"
assert_contains "$INSTALL_PACKAGE_STEP" "sudo -n true"
assert_contains "$INSTALL_PACKAGE_STEP" "TCFS_INSTALLER"
assert_contains "$INSTALL_PACKAGE_STEP" "/usr/sbin/installer"
assert_contains "$INSTALL_PACKAGE_STEP" "TCFS_RUNNER_SUDO_PASSWORD"
assert_contains "$INSTALL_PACKAGE_STEP" ".config/sops-nix/secrets/become/password"
assert_contains "$INSTALL_PACKAGE_STEP" "sudo -S -p '' \"\$INSTALLER_BIN\" -pkg \"\$PACKAGE_PATH\" -target /"

SIGNING_PREFLIGHT_STEP="${TMPDIR}/verify-installed-fileprovider-production-signing.sh"
extract_step_from_workflow \
  "$POSTINSTALL_WORKFLOW" \
  "pkg-postinstall" \
  "Verify installed FileProvider signing" \
  "$SIGNING_PREFLIGHT_STEP"
bash -n "$SIGNING_PREFLIGHT_STEP"
assert_contains "$SIGNING_PREFLIGHT_STEP" "scripts/macos-fileprovider-preflight.sh"
assert_contains "$SIGNING_PREFLIGHT_STEP" "--signing-only"
assert_contains "$SIGNING_PREFLIGHT_STEP" "--require-production-signing"
assert_contains "$SIGNING_PREFLIGHT_STEP" "--app-path /Applications/TCFSProvider.app"

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
assert_contains "$INSTALL_BINARY_SMOKE_STEP" "scripts/install-smoke.sh --expected-version \"\${BINARY_EXPECTED_VERSION}\""

INSTALLED_POLICY_PROBE_STEP="${TMPDIR}/probe-installed-fileprovider-app-launch-policy.sh"
extract_step_from_workflow \
  "$POSTINSTALL_WORKFLOW" \
  "pkg-postinstall" \
  "Probe installed FileProvider app launch policy" \
  "$INSTALLED_POLICY_PROBE_STEP"
bash -n "$INSTALLED_POLICY_PROBE_STEP"
assert_contains "$INSTALLED_POLICY_PROBE_STEP" "/Applications/TCFSProvider.app/Contents/MacOS/TCFSProvider"
assert_contains "$INSTALLED_POLICY_PROBE_STEP" "installed-host-policy-probe.log"
assert_contains "$INSTALLED_POLICY_PROBE_STEP" "installed-host-policy-probe.exit"
assert_contains "$INSTALLED_POLICY_PROBE_STEP" "installed-host-policy-probe.ps"
assert_contains "$INSTALLED_POLICY_PROBE_STEP" "installed-host-policy-probe.sample.txt"
assert_contains "$INSTALLED_POLICY_PROBE_STEP" "TCFS_FILEPROVIDER_HOST_POLICY_PROBE_ONLY"
assert_contains "$INSTALLED_POLICY_PROBE_STEP" "TCFS_FILEPROVIDER_HOST_POLICY_PROBE_ONLY=1"
assert_contains "$INSTALLED_POLICY_PROBE_STEP" "TCFS_FILEPROVIDER_TESTING_MODE_ALWAYS_ENABLED=1"
assert_contains "$INSTALLED_POLICY_PROBE_STEP" "exit=skipped"
assert_contains "$INSTALLED_POLICY_PROBE_STEP" "exit 0"

APPLY_LAB_GATEKEEPER_OVERRIDE_STEP="${TMPDIR}/apply-pzm-lab-gatekeeper-override.sh"
extract_step_from_workflow \
  "$POSTINSTALL_WORKFLOW" \
  "pkg-postinstall" \
  "Verify PZM lab Gatekeeper profile" \
  "$APPLY_LAB_GATEKEEPER_OVERRIDE_STEP"
bash -n "$APPLY_LAB_GATEKEEPER_OVERRIDE_STEP"
assert_contains "$POSTINSTALL_WORKFLOW" "lab_gatekeeper_override"
assert_contains "$POSTINSTALL_WORKFLOW" "rebuild_domain:"
assert_contains "$POSTINSTALL_WORKFLOW" "lab_gatekeeper_override=true requires fileprovider_testing_mode=true"
assert_contains "$POSTINSTALL_WORKFLOW" "lab_gatekeeper_override=true is restricted to the petting-zoo-mini lab runner"
assert_contains "$POSTINSTALL_WORKFLOW" "github.event.inputs.lab_gatekeeper_override == 'true'"
assert_contains "$APPLY_LAB_GATEKEEPER_OVERRIDE_STEP" "scripts/macos-fileprovider-lab-gatekeeper-override.sh apply"
assert_contains "$APPLY_LAB_GATEKEEPER_OVERRIDE_STEP" "--log-dir \"\$LOG_DIR/lab-gatekeeper-override\""

REMOVE_LAB_GATEKEEPER_OVERRIDE_STEP="${TMPDIR}/remove-pzm-lab-gatekeeper-override.sh"
extract_step_from_workflow \
  "$POSTINSTALL_WORKFLOW" \
  "pkg-postinstall" \
  "Record PZM lab Gatekeeper profile cleanup" \
  "$REMOVE_LAB_GATEKEEPER_OVERRIDE_STEP"
bash -n "$REMOVE_LAB_GATEKEEPER_OVERRIDE_STEP"
assert_contains "$REMOVE_LAB_GATEKEEPER_OVERRIDE_STEP" "scripts/macos-fileprovider-lab-gatekeeper-override.sh cleanup"
assert_contains "$REMOVE_LAB_GATEKEEPER_OVERRIDE_STEP" "--log-dir \"\$LOG_DIR/lab-gatekeeper-override-cleanup\""

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
assert_contains "$VALIDATE_STORAGE_STEP" "set only one of package_url or package_artifact_run_id"
assert_contains "$VALIDATE_STORAGE_STEP" "fileprovider_testing_mode=true requires package_artifact_run_id or package_url"
assert_contains "$VALIDATE_STORAGE_STEP" "require_conflict_enumerator_status=true requires exercise_conflict_status=true"
assert_contains "$VALIDATE_STORAGE_STEP" "exercise_rename_safety=true requires fileprovider_testing_mode=true or exercise_dev_id_layered_proof=true"
assert_contains "$VALIDATE_STORAGE_STEP" "soak_cycles must be a positive integer"
assert_contains "$VALIDATE_STORAGE_STEP" "soak_cycles greater than 1 requires fileprovider_testing_mode=true or exercise_dev_id_layered_proof=true"
assert_contains "$VALIDATE_STORAGE_STEP" "lab_gatekeeper_override=true requires fileprovider_testing_mode=true"
assert_contains "$VALIDATE_STORAGE_STEP" "lab_gatekeeper_override=true is restricted to the petting-zoo-mini lab runner"

DOWNLOAD_PACKAGE_STEP="${TMPDIR}/download-package.sh"
extract_step_from_workflow \
  "$POSTINSTALL_WORKFLOW" \
  "pkg-postinstall" \
  "Download package" \
  "$DOWNLOAD_PACKAGE_STEP"
bash -n "$DOWNLOAD_PACKAGE_STEP"
assert_contains "$DOWNLOAD_PACKAGE_STEP" "PACKAGE_PATH=\"\$RUNNER_TEMP/tcfs-\${VERSION}-macos-aarch64.pkg\""
assert_contains "$DOWNLOAD_PACKAGE_STEP" "package_artifact_run_id"
assert_contains "$DOWNLOAD_PACKAGE_STEP" "package_artifact_name"
assert_contains "$DOWNLOAD_PACKAGE_STEP" "archive_download_url"
assert_contains "$DOWNLOAD_PACKAGE_STEP" "python3 -m zipfile -e"
assert_contains "$DOWNLOAD_PACKAGE_STEP" "non-expired artifact \$PACKAGE_ARTIFACT_NAME not found"
assert_contains "$DOWNLOAD_PACKAGE_STEP" "actions/runs/\${PACKAGE_ARTIFACT_RUN_ID}/artifacts?per_page=100"
assert_contains "$DOWNLOAD_PACKAGE_STEP" "package_url"
assert_contains "$DOWNLOAD_PACKAGE_STEP" "curl -fL -o \"\$PACKAGE_PATH\" \"\$PACKAGE_URL\""
assert_contains "$DOWNLOAD_PACKAGE_STEP" "releases/download/\${TAG}/tcfs-\${VERSION}-macos-aarch64.pkg"
assert_contains "$DOWNLOAD_PACKAGE_STEP" "PACKAGE_PATH=\$PACKAGE_PATH"

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

DERIVE_RUN_PATHS_STEP="${TMPDIR}/derive-run-paths.sh"
extract_step_from_workflow \
  "$POSTINSTALL_WORKFLOW" \
  "pkg-postinstall" \
  "Derive run paths" \
  "$DERIVE_RUN_PATHS_STEP"
bash -n "$DERIVE_RUN_PATHS_STEP"
assert_contains "$DERIVE_RUN_PATHS_STEP" "CONFIG_DIR=\"\$RUNNER_TEMP/tcfs-config\""
assert_contains "$DERIVE_RUN_PATHS_STEP" "CONFIG_PATH=\"\$CONFIG_DIR/config.toml\""
assert_contains "$DERIVE_RUN_PATHS_STEP" "FILEPROVIDER_LISTEN_ADDR=\"127.0.0.1:19101\""
assert_contains "$DERIVE_RUN_PATHS_STEP" "FILEPROVIDER_ENDPOINT=\"http://\${FILEPROVIDER_LISTEN_ADDR}\""
assert_contains "$POSTINSTALL_WORKFLOW" "smoke_environment:"
assert_contains "$POSTINSTALL_WORKFLOW" "remote_prefix:"
assert_contains "$DERIVE_RUN_PATHS_STEP" 'REMOTE_PREFIX="${{ github.event.inputs.remote_prefix }}"'
assert_contains "$DERIVE_RUN_PATHS_STEP" 'REMOTE_PREFIX="gha/macos-postinstall/${TAG}/${GITHUB_RUN_ID}-${GITHUB_RUN_ATTEMPT}"'
assert_contains "$DERIVE_RUN_PATHS_STEP" 'REMOTE_PREFIX="${REMOTE_PREFIX#/}"'
assert_contains "$DERIVE_RUN_PATHS_STEP" 'REMOTE_PREFIX="${REMOTE_PREFIX%/}"'
assert_contains "$DERIVE_RUN_PATHS_STEP" "remote_prefix resolved to an empty prefix"
assert_not_contains "$DERIVE_RUN_PATHS_STEP" "FILEPROVIDER_SOCKET=\"\$HOME/Library/Application Support/io.tinyland.tcfs/tcfsd.sock\""
assert_not_contains "$DERIVE_RUN_PATHS_STEP" "FILEPROVIDER_SOCKET=\"\${APP_GROUP_DIR}/tcfsd-gha.sock\""
assert_not_contains "$DERIVE_RUN_PATHS_STEP" "FILEPROVIDER_SOCKET=\"/tmp/tcfsd-fileprovider-gha.sock\""
assert_not_contains "$DERIVE_RUN_PATHS_STEP" "CONFIG_DIR=\"\$HOME/.config/tcfs\""

WRITE_LIVE_CONFIG_STEP="${TMPDIR}/write-live-config.sh"
extract_step_from_workflow \
  "$POSTINSTALL_WORKFLOW" \
  "pkg-postinstall" \
  "Write live config" \
  "$WRITE_LIVE_CONFIG_STEP"
bash -n "$WRITE_LIVE_CONFIG_STEP"
assert_contains "$WRITE_LIVE_CONFIG_STEP" "endpoint = \"\${TCFS_SMOKE_S3_ENDPOINT}\""
assert_contains "$WRITE_LIVE_CONFIG_STEP" "listen = \"\${FILEPROVIDER_LISTEN_ADDR}\""
assert_contains "$WRITE_LIVE_CONFIG_STEP" "fileprovider_endpoint = \"\${FILEPROVIDER_ENDPOINT}\""
assert_not_contains "$WRITE_LIVE_CONFIG_STEP" "fileprovider_socket = "
assert_contains "$WRITE_LIVE_CONFIG_STEP" "bucket = \"\${TCFS_SMOKE_S3_BUCKET}\""
assert_contains "$WRITE_LIVE_CONFIG_STEP" "STORAGE_ENFORCE_TLS=true"
assert_contains "$WRITE_LIVE_CONFIG_STEP" "enforce_tls = \${STORAGE_ENFORCE_TLS}"
assert_contains "$WRITE_LIVE_CONFIG_STEP" "[crypto]"
assert_contains "$WRITE_LIVE_CONFIG_STEP" "enabled = true"
assert_contains "$WRITE_LIVE_CONFIG_STEP" "master_key_file = \"\${MASTER_KEY_PATH}\""
assert_contains "$WRITE_LIVE_CONFIG_STEP" "[auth]"
assert_contains "$WRITE_LIVE_CONFIG_STEP" "require_session = false"

PROVISION_FILEPROVIDER_CONFIG_STEP="${TMPDIR}/provision-fileprovider-config.sh"
extract_step_from_workflow \
  "$POSTINSTALL_WORKFLOW" \
  "pkg-postinstall" \
  "Provision FileProvider config" \
  "$PROVISION_FILEPROVIDER_CONFIG_STEP"
bash -n "$PROVISION_FILEPROVIDER_CONFIG_STEP"
assert_contains "$POSTINSTALL_WORKFLOW" "require_cli_fileprovider_config"
assert_contains "$PROVISION_FILEPROVIDER_CONFIG_STEP" "tcfs --config \"\$CONFIG_PATH\" config fileprovider"
assert_contains "$PROVISION_FILEPROVIDER_CONFIG_STEP" "--out \"\$FILEPROVIDER_CONFIG_PATH\""
assert_contains "$PROVISION_FILEPROVIDER_CONFIG_STEP" "--device-id \"gha-macos-postinstall\""
assert_contains "$PROVISION_FILEPROVIDER_CONFIG_STEP" "--master-key-file \"\$MASTER_KEY_PATH\""
assert_contains "$PROVISION_FILEPROVIDER_CONFIG_STEP" "Installed tcfs does not support 'config fileprovider'"
assert_contains "$PROVISION_FILEPROVIDER_CONFIG_STEP" "TCFS_FILEPROVIDER_SKIP_APP_GROUP_COPY=1"
assert_contains "$PROVISION_FILEPROVIDER_CONFIG_STEP" "swift/fileprovider/provision-config.sh \"\$CONFIG_PATH\""

SEED_REMOTE_FIXTURE_STEP="${TMPDIR}/seed-remote-fixture.sh"
extract_step_from_workflow \
  "$POSTINSTALL_WORKFLOW" \
  "pkg-postinstall" \
  "Seed remote fixture" \
  "$SEED_REMOTE_FIXTURE_STEP"
bash -n "$SEED_REMOTE_FIXTURE_STEP"
assert_contains "$SEED_REMOTE_FIXTURE_STEP" "> \"\$EXPECTED_CONTENT_FILE\""
assert_contains "$SEED_REMOTE_FIXTURE_STEP" "cp \"\$EXPECTED_CONTENT_FILE\" \"\$FIXTURE_PATH\""
assert_contains "$SEED_REMOTE_FIXTURE_STEP" "run_bounded tcfs-push.log 120 tcfs --config \"\$CONFIG_PATH\" push \"\$FIXTURE_PATH\""
assert_contains "$SEED_REMOTE_FIXTURE_STEP" "tcfs push failed or timed out"

VERIFY_E2EE_STEP="${TMPDIR}/verify-remote-fixture-requires-e2ee-key.sh"
extract_step_from_workflow \
  "$POSTINSTALL_WORKFLOW" \
  "pkg-postinstall" \
  "Verify remote fixture requires E2EE key" \
  "$VERIFY_E2EE_STEP"
bash -n "$VERIFY_E2EE_STEP"
assert_contains "$VERIFY_E2EE_STEP" "NO_CRYPTO_CONFIG_PATH"
assert_contains "$VERIFY_E2EE_STEP" "run_bounded no-crypto-pull.log 60"
assert_contains "$VERIFY_E2EE_STEP" "Encrypted smoke fixture was readable without the E2EE master key"
assert_contains "$VERIFY_E2EE_STEP" "No-crypto pull timed out"
assert_contains "$VERIFY_E2EE_STEP" "run_bounded e2ee-pull.log 120"
assert_contains "$VERIFY_E2EE_STEP" "E2EE pull failed or timed out"
assert_contains "$VERIFY_E2EE_STEP" "cmp -s \"\$EXPECTED_CONTENT_FILE\" \"\$RUNNER_TEMP/e2ee-pull-check\""

START_DAEMON_STEP="${TMPDIR}/start-tcfsd-with-live-config.sh"
extract_step_from_workflow \
  "$POSTINSTALL_WORKFLOW" \
  "pkg-postinstall" \
  "Start tcfsd with live config" \
  "$START_DAEMON_STEP"
bash -n "$START_DAEMON_STEP"
assert_contains "$START_DAEMON_STEP" "for socket in /tmp/tcfsd-gha.sock"
assert_contains "$START_DAEMON_STEP" "socket.create_connection((host, int(port)), timeout=1)"
assert_contains "$START_DAEMON_STEP" "Timed out waiting for TCP listener \$FILEPROVIDER_LISTEN_ADDR"

POSTINSTALL_HARNESS_STEP="${TMPDIR}/run-macos-postinstall-harness.sh"
extract_step_from_workflow \
  "$POSTINSTALL_WORKFLOW" \
  "pkg-postinstall" \
  "Run macOS post-install harness" \
  "$POSTINSTALL_HARNESS_STEP"
bash -n "$POSTINSTALL_HARNESS_STEP"
assert_contains "$POSTINSTALL_HARNESS_STEP" "--expected-content-file \"\$EXPECTED_CONTENT_FILE\""
assert_contains "$POSTINSTALL_HARNESS_STEP" "--require-keychain-config"
assert_contains "$POSTINSTALL_HARNESS_STEP" "elect_plugin_use"
assert_contains "$POSTINSTALL_HARNESS_STEP" "--elect-plugin-use"
assert_contains "$POSTINSTALL_HARNESS_STEP" "rebuild_domain"
assert_contains "$POSTINSTALL_HARNESS_STEP" "--rebuild-domain"
assert_contains "$POSTINSTALL_WORKFLOW" "host_root_probe:"
assert_contains "$POSTINSTALL_WORKFLOW" "Require the signed HostApp root/user-visible URL probe"
assert_contains "$POSTINSTALL_HARNESS_STEP" "host_root_probe"
assert_contains "$POSTINSTALL_HARNESS_STEP" "--host-root-probe"
assert_contains "$POSTINSTALL_HARNESS_STEP" "fileprovider_testing_mode"
assert_contains "$POSTINSTALL_HARNESS_STEP" "--fileprovider-testing-mode"
assert_contains "$POSTINSTALL_HARNESS_STEP" "--exercise-evict-rehydrate"
assert_contains "$POSTINSTALL_WORKFLOW" "soak_cycles:"
assert_contains "$POSTINSTALL_HARNESS_STEP" "--soak-cycles"
assert_contains "$POSTINSTALL_WORKFLOW" "exercise_rename_safety:"
assert_contains "$POSTINSTALL_WORKFLOW" "rename a FileProvider file"
assert_contains "$POSTINSTALL_HARNESS_STEP" "exercise_rename_safety"
assert_contains "$POSTINSTALL_HARNESS_STEP" "--exercise-rename-safety"
assert_contains "$POSTINSTALL_HARNESS_STEP" "--rename-source-file"
assert_contains "$POSTINSTALL_HARNESS_STEP" "--rename-dest-file"
assert_contains "$POSTINSTALL_HARNESS_STEP" "--rename-content-file"
assert_contains "$POSTINSTALL_WORKFLOW" "require_conflict_enumerator_status:"
assert_contains "$POSTINSTALL_WORKFLOW" "hydration_state=conflict"
assert_contains "$POSTINSTALL_HARNESS_STEP" "require_conflict_enumerator_status"
assert_contains "$POSTINSTALL_HARNESS_STEP" "--require-conflict-enumerator-status"

bash -n "$PKG_POSTINSTALL"
bash -n "$LAB_GATEKEEPER_OVERRIDE"

assert_contains "$LAB_GATEKEEPER_OVERRIDE" "io.tinyland.tcfs.fileprovider.lab.system-policy"
assert_contains "$LAB_GATEKEEPER_OVERRIDE" "com.apple.systempolicy.rule"
assert_contains "$LAB_GATEKEEPER_OVERRIDE" "PayloadScope"
assert_contains "$LAB_GATEKEEPER_OVERRIDE" "profiles show -type configuration -all"
assert_contains "$LAB_GATEKEEPER_OVERRIDE" "SystemPolicyRule configuration profile is not installed"
assert_contains "$LAB_GATEKEEPER_OVERRIDE" "TCFS_RUNNER_SUDO_PASSWORD"
assert_contains "$LAB_GATEKEEPER_OVERRIDE" ".config/sops-nix/secrets/become/password"
assert_contains "$LAB_GATEKEEPER_OVERRIDE" "macOS 15"

assert_contains "$PKG_POSTINSTALL" "LSREGISTER_BIN=\"\${TCFS_POSTINSTALL_LSREGISTER:-/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister}\""
assert_contains "$PKG_POSTINSTALL" "\"\$LAUNCHCTL_BIN\" asuser \"\$CONSOLE_UID\""
assert_contains "$PKG_POSTINSTALL" "\"\$LSREGISTER_BIN\" -f \"\$APP_PATH\""
assert_contains "$PKG_POSTINSTALL" "PLIST_DIR=\"\${TCFS_POSTINSTALL_LAUNCHAGENTS_DIR:-/Library/LaunchAgents}\""
assert_contains "$PKG_POSTINSTALL" ". \"\$HOME/.config/tcfs/env\""
assert_contains "$PKG_POSTINSTALL" "tcfsd: no user config; run: /usr/local/bin/tcfs init --config-out"
assert_contains "$PKG_POSTINSTALL" "exit 0; fi; exec /usr/local/bin/tcfsd"
assert_contains "$PKG_POSTINSTALL" "exec /usr/local/bin/tcfsd --config \"\$HOME/.config/tcfs/config.toml\" --mode daemon"

POSTINSTALL_APP="${TMPDIR}/Applications/TCFSProvider.app"
POSTINSTALL_LAUNCHAGENTS="${TMPDIR}/LaunchAgents"
POSTINSTALL_LOG="${TMPDIR}/postinstall.log"
mkdir -p "$POSTINSTALL_APP/Contents/Extensions/TCFSFileProvider.appex"
TCFS_POSTINSTALL_APP_PATH="$POSTINSTALL_APP" \
TCFS_POSTINSTALL_LAUNCHAGENTS_DIR="$POSTINSTALL_LAUNCHAGENTS" \
TCFS_POSTINSTALL_PLUGINKIT="$FAKE_BIN/pluginkit" \
TCFS_POSTINSTALL_LSREGISTER="$FAKE_BIN/lsregister" \
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
assert_contains "$PLIST_PATH" ". \"\$HOME/.config/tcfs/env\""
assert_contains "$PLIST_PATH" "tcfsd: no user config; run: /usr/local/bin/tcfs init --config-out"
assert_contains "$PLIST_PATH" "exit 0; fi; exec /usr/local/bin/tcfsd"
assert_contains "$PLIST_PATH" "exec /usr/local/bin/tcfsd --config \"\$HOME/.config/tcfs/config.toml\" --mode daemon"
assert_contains "$POSTINSTALL_LOG" "lsregister -f $POSTINSTALL_APP"
assert_contains "$POSTINSTALL_LOG" "launchctl asuser 501"
assert_contains "$POSTINSTALL_LOG" "launchctl bootstrap gui/501 $PLIST_PATH"
assert_contains "$POSTINSTALL_LOG" "launchctl enable gui/501/io.tinyland.tcfsd"

UPGRADE_LAUNCHAGENTS="${TMPDIR}/upgrade-launchagents"
UPGRADE_LOG="${TMPDIR}/upgrade-postinstall.log"
mkdir -p "$UPGRADE_LAUNCHAGENTS"
printf 'stale launchagent\n' >"${UPGRADE_LAUNCHAGENTS}/io.tinyland.tcfsd.plist"
TCFS_POSTINSTALL_APP_PATH="$POSTINSTALL_APP" \
TCFS_POSTINSTALL_LAUNCHAGENTS_DIR="$UPGRADE_LAUNCHAGENTS" \
TCFS_POSTINSTALL_PLUGINKIT="$FAKE_BIN/pluginkit" \
TCFS_POSTINSTALL_LSREGISTER="$FAKE_BIN/lsregister" \
TCFS_POSTINSTALL_LAUNCHCTL="$FAKE_BIN/launchctl" \
TCFS_POSTINSTALL_SUDO="$FAKE_BIN/sudo" \
TCFS_POSTINSTALL_STAT="$FAKE_BIN/stat" \
TCFS_POSTINSTALL_ID="$FAKE_BIN/id" \
TCFS_POSTINSTALL_CHOWN="$FAKE_BIN/chown" \
TCFS_FAKE_POSTINSTALL_LOG="$UPGRADE_LOG" \
bash "$PKG_POSTINSTALL"
UPGRADE_PLIST="${UPGRADE_LAUNCHAGENTS}/io.tinyland.tcfsd.plist"
assert_contains "$UPGRADE_PLIST" "tcfsd: no user config; run: /usr/local/bin/tcfs init --config-out"
assert_contains "$UPGRADE_PLIST" "exec /usr/local/bin/tcfsd --config \"\$HOME/.config/tcfs/config.toml\" --mode daemon"
assert_not_contains "$UPGRADE_PLIST" "stale launchagent"

NO_SESSION_LAUNCHAGENTS="${TMPDIR}/no-session-launchagents"
NO_SESSION_LOG="${TMPDIR}/no-session-postinstall.log"
TCFS_POSTINSTALL_APP_PATH="${TMPDIR}/missing-app/TCFSProvider.app" \
TCFS_POSTINSTALL_LAUNCHAGENTS_DIR="$NO_SESSION_LAUNCHAGENTS" \
TCFS_POSTINSTALL_PLUGINKIT="$FAKE_BIN/pluginkit" \
TCFS_POSTINSTALL_LSREGISTER="$FAKE_BIN/lsregister" \
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
if [[ -f "$NO_SESSION_LOG" ]] && grep -Fq "lsregister" "$NO_SESSION_LOG"; then
  printf 'postinstall attempted lsregister without installed app/session\n' >&2
  cat "$NO_SESSION_LOG" >&2
  exit 1
fi

if command -v shellcheck >/dev/null 2>&1; then
  shellcheck -s bash "$IMPORT_STEP"
  shellcheck "$PKG_POSTINSTALL"
fi

printf 'release workflow FileProvider packaging tests passed\n'
