#!/usr/bin/env bash
#
# Regression tests for ASC-backed macOS FileProvider lab provisioning helpers.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ASC_SCRIPT="${REPO_ROOT}/scripts/asc-fileprovider-lab-provision.py"
P12_PROBE="${REPO_ROOT}/scripts/macos-codesign-p12-probe.sh"
CONFIG="${REPO_ROOT}/config/macos-fileprovider-lab.asc.json"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-asc-fileprovider-test.XXXXXX")"
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

python3 -m py_compile "$ASC_SCRIPT"

VALIDATE_OUT="${TMPDIR}/validate.out"
python3 "$ASC_SCRIPT" --config "$CONFIG" --validate-config >"$VALIDATE_OUT"
assert_contains "$VALIDATE_OUT" "config valid:"
assert_contains "$VALIDATE_OUT" "profiles: 2"

HELP_OUT="${TMPDIR}/help.out"
python3 "$ASC_SCRIPT" --help >"$HELP_OUT"
assert_contains "$HELP_OUT" "--apply"
assert_contains "$HELP_OUT" "--create-certificate"
assert_contains "$HELP_OUT" "--certificate-sha1"

P12_HELP_OUT="${TMPDIR}/p12-help.out"
bash "$P12_PROBE" --help >"$P12_HELP_OUT"
assert_contains "$P12_HELP_OUT" "--p12 <path>"
assert_contains "$P12_HELP_OUT" "--identity <name-or-sha1>"
assert_contains "$P12_PROBE" "security list-keychains -d user -s"
assert_contains "$P12_PROBE" "awk '/Apple Development/"
assert_contains "$ASC_SCRIPT" "security"
assert_contains "$ASC_SCRIPT" "export"
assert_contains "$ASC_SCRIPT" "pkcs12"

BAD_CONFIG="${TMPDIR}/bad.json"
cat >"$BAD_CONFIG" <<'JSON'
{
  "profiles": []
}
JSON
assert_fails_contains \
  "config.profiles must not be empty" \
  python3 "$ASC_SCRIPT" --config "$BAD_CONFIG" --validate-config

python3 - "$CONFIG" <<'PY'
import json
import sys

config = json.load(open(sys.argv[1], encoding="utf-8"))
assert config["team_id"] == "QP994XQKNH"
assert config["runner"]["name"] == "petting-zoo-mini"
assert config["runner"]["device_udid"] == "00008132-001240C80138801C"
profiles = {profile["role"]: profile for profile in config["profiles"]}
assert profiles["host"]["bundle_id"] == "io.tinyland.tcfs"
assert profiles["extension"]["bundle_id"] == "io.tinyland.tcfs.fileprovider"
assert "FILEPROVIDER_TESTINGMODE" in profiles["host"]["required_capabilities"]
assert profiles["host"]["required_entitlements"]["com.apple.developer.fileprovider.testing-mode"] is True
assert profiles["host"]["install_filename"] == "tcfshostdevelopmenttestingmodepzm.provisionprofile"
assert profiles["extension"]["install_filename"] == "tcfsfileproviderdevelopmentpzm.provisionprofile"
PY

printf 'ASC FileProvider lab provisioning tests passed\n'
