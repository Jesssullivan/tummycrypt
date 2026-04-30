#!/usr/bin/env bash
#
# Regression tests for macos-fileprovider-profile-inventory.sh.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="${REPO_ROOT}/scripts/macos-fileprovider-profile-inventory.sh"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-profile-inventory-test.XXXXXX")"
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

FAKE_BIN="${TMPDIR}/fake-bin"
PROFILES_DIR="${TMPDIR}/profiles"
EMPTY_PROFILES_DIR="${TMPDIR}/empty-profiles"
MISMATCH_PROFILES_DIR="${TMPDIR}/mismatch-profiles"
mkdir -p "$FAKE_BIN" "$PROFILES_DIR" "$EMPTY_PROFILES_DIR" "$MISMATCH_PROFILES_DIR"

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
chmod +x "$FAKE_BIN"/*

write_profile \
  "$PROFILES_DIR/host.provisionprofile" \
  "TCFS Host" \
  "HOST-UUID" \
  "QP994XQKNH" \
  "io.tinyland.tcfs" \
  "group.io.tinyland.tcfs"
write_profile \
  "$PROFILES_DIR/extension.provisionprofile" \
  "TCFS FileProvider Extension" \
  "EXT-UUID" \
  "QP994XQKNH" \
  "io.tinyland.tcfs.fileprovider" \
  "group.io.tinyland.tcfs"
write_profile \
  "$PROFILES_DIR/irrelevant.mobileprovision" \
  "Irrelevant" \
  "OTHER-UUID" \
  "QP994XQKNH" \
  "io.tinyland.other" \
  "group.io.tinyland.tcfs"

OUT="${TMPDIR}/inventory.out"
PATH="$FAKE_BIN:$PATH" bash "$SCRIPT" \
  --profiles-dir "$PROFILES_DIR" \
  --strict \
  >"$OUT"

assert_contains "$OUT" "profiles scanned: 3"
assert_contains "$OUT" "host candidates: 1"
assert_contains "$OUT" "extension candidates: 1"
assert_contains "$OUT" "compatible pair: found"
assert_contains "$OUT" "host profile: $PROFILES_DIR/host.provisionprofile"
assert_contains "$OUT" "extension profile: $PROFILES_DIR/extension.provisionprofile"
assert_contains "$OUT" "team_prefix: QP994XQKNH"
assert_contains "$OUT" "TCFS_HOST_PROVISIONING_PROFILE=$PROFILES_DIR/host.provisionprofile"
assert_contains "$OUT" "TCFS_EXTENSION_PROVISIONING_PROFILE=$PROFILES_DIR/extension.provisionprofile"
assert_contains "$OUT" "TCFS_REQUIRE_PRODUCTION_SIGNING=1"

ENV_ONLY_OUT="${TMPDIR}/env-only.out"
PATH="$FAKE_BIN:$PATH" bash "$SCRIPT" \
  --profiles-dir "$PROFILES_DIR" \
  --env-only \
  --strict \
  >"$ENV_ONLY_OUT"
assert_contains "$ENV_ONLY_OUT" "TCFS_HOST_PROVISIONING_PROFILE=$PROFILES_DIR/host.provisionprofile"
assert_contains "$ENV_ONLY_OUT" "TCFS_EXTENSION_PROVISIONING_PROFILE=$PROFILES_DIR/extension.provisionprofile"
assert_contains "$ENV_ONLY_OUT" "TCFS_REQUIRE_PRODUCTION_SIGNING=1"
if grep -Fq "profiles scanned:" "$ENV_ONLY_OUT"; then
  printf 'env-only mode printed human-readable diagnostics\n' >&2
  exit 1
fi

EMPTY_OUT="${TMPDIR}/empty.out"
PATH="$FAKE_BIN:$PATH" bash "$SCRIPT" \
  --profiles-dir "$EMPTY_PROFILES_DIR" \
  >"$EMPTY_OUT"
assert_contains "$EMPTY_OUT" "profiles scanned: 0"
assert_contains "$EMPTY_OUT" "compatible pair: not found"

assert_fails_contains \
  "compatible pair: not found" \
  env PATH="$FAKE_BIN:$PATH" bash "$SCRIPT" \
    --profiles-dir "$EMPTY_PROFILES_DIR" \
    --strict

write_profile \
  "$MISMATCH_PROFILES_DIR/host.provisionprofile" \
  "TCFS Host" \
  "HOST-UUID" \
  "QP994XQKNH" \
  "io.tinyland.tcfs" \
  "group.io.tinyland.tcfs"
write_profile \
  "$MISMATCH_PROFILES_DIR/extension.provisionprofile" \
  "TCFS FileProvider Extension" \
  "EXT-UUID" \
  "Z9ZZZZZZZZ" \
  "io.tinyland.tcfs.fileprovider" \
  "group.io.tinyland.tcfs"

assert_fails_contains \
  "compatible pair: not found" \
  env PATH="$FAKE_BIN:$PATH" bash "$SCRIPT" \
    --profiles-dir "$MISMATCH_PROFILES_DIR" \
    --strict

printf 'macOS FileProvider profile inventory tests passed\n'
