#!/usr/bin/env bash
#
# Regression tests for macos-fileprovider-preflight.sh using fake platform tools.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="${REPO_ROOT}/scripts/macos-fileprovider-preflight.sh"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-macos-preflight-test.XXXXXX")"
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

FAKE_BIN="${TMPDIR}/fake-bin"
HOME_DIR="${TMPDIR}/home"
APP_PATH="${TMPDIR}/TCFSProvider.app"
CLOUD_ROOT="${HOME_DIR}/Library/CloudStorage/TCFS"
CONFIG_PATH="${HOME_DIR}/.config/tcfs/config.toml"
FILEPROVIDER_CONFIG="${HOME_DIR}/.config/tcfs/fileprovider/config.json"
SIGNING_CERT_SHA1="$(printf '%s' "tcfs-signing-cert" | openssl dgst -sha1 | awk '{ print toupper($NF) }')"

write_info_plist() {
  local path="$1"
  local bundle_id="$2"

  mkdir -p "$(dirname "$path")"
  cat >"$path" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key>
  <string>${bundle_id}</string>
</dict>
</plist>
EOF
}

write_profile_plist() {
  local path="$1"
  local bundle_id="$2"
  local cert_material="${3:-tcfs-signing-cert}"
  local cert_b64

  cert_b64="$(printf '%s' "$cert_material" | base64 | tr -d '\n')"

  mkdir -p "$(dirname "$path")"
  cat >"$path" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Entitlements</key>
  <dict>
    <key>application-identifier</key>
    <string>QP994XQKNH.${bundle_id}</string>
    <key>com.apple.security.application-groups</key>
    <array>
      <string>group.io.tinyland.tcfs</string>
    </array>
    <key>keychain-access-groups</key>
    <array>
      <string>QP994XQKNH.group.io.tinyland.tcfs</string>
    </array>
  </dict>
  <key>DeveloperCertificates</key>
  <array>
    <data>${cert_b64}</data>
  </array>
</dict>
</plist>
EOF
}

mkdir -p \
  "$FAKE_BIN" \
  "$APP_PATH/Contents/Extensions/TCFSFileProvider.appex/Contents" \
  "$CLOUD_ROOT" \
  "$(dirname "$CONFIG_PATH")" \
  "$(dirname "$FILEPROVIDER_CONFIG")"

printf '[storage]\nendpoint = "http://example.invalid:8333"\nbucket = "tcfs"\n' >"$CONFIG_PATH"
printf '{"socket_path":"/tmp/tcfs-fileprovider.sock"}\n' >"$FILEPROVIDER_CONFIG"
write_info_plist "$APP_PATH/Contents/Info.plist" "io.tinyland.tcfs"
write_info_plist "$APP_PATH/Contents/Extensions/TCFSFileProvider.appex/Contents/Info.plist" "io.tinyland.tcfs.fileprovider"

cat >"$FAKE_BIN/uname" <<'EOF'
#!/usr/bin/env bash
printf 'Darwin\n'
EOF
cat >"$FAKE_BIN/tcfs" <<'EOF'
#!/usr/bin/env bash
if [[ "${1:-}" == "--version" ]]; then
  printf 'tcfs 0.12.2\n'
else
  exit 1
fi
EOF
cat >"$FAKE_BIN/tcfsd" <<'EOF'
#!/usr/bin/env bash
if [[ "${1:-}" == "--version" ]]; then
  printf 'tcfsd 0.12.2\n'
else
  exit 1
fi
EOF
cat >"$FAKE_BIN/pluginkit" <<'EOF'
#!/usr/bin/env bash
printf 'io.tinyland.tcfs.fileprovider(0.2.0)\n'
printf '            Path = /Users/test/Applications/TCFSProvider.app/Contents/Extensions/TCFSFileProvider.appex\n'
if [[ "${TCFS_FAKE_PLUGIN_DUPES:-0}" == "1" ]]; then
  printf 'io.tinyland.tcfs.fileprovider(0.1.0)\n'
  printf '            Path = /Users/test/git/tummycrypt/build/TCFSProvider.app/Contents/Extensions/TCFSFileProvider.appex\n'
fi
EOF
cat >"$FAKE_BIN/fileproviderctl" <<'EOF'
#!/usr/bin/env bash
case "${TCFS_FAKE_FILEPROVIDERCTL:-ok}" in
  ok)
    printf 'io.tinyland.tcfs\n'
    ;;
  unavailable)
    printf 'File Provider control utility.\n' >&2
    exit 64
    ;;
esac
EOF
cat >"$FAKE_BIN/codesign" <<'EOF'
#!/usr/bin/env bash
if [[ "${1:-}" == "-vvv" ]]; then
  exit 0
fi
if [[ "${1:-}" == "-d" ]]; then
  if [[ "$*" == *"--extract-certificates"* ]]; then
    prefix=""
    bundle=""
    shift
    while [[ $# -gt 0 ]]; do
      case "$1" in
        --extract-certificates=*)
          prefix="${1#--extract-certificates=}"
          shift
          ;;
        --extract-certificates)
          prefix="${2:-}"
          shift 2
          ;;
        *)
          bundle="$1"
          shift
          ;;
      esac
    done

    [[ -n "$prefix" ]] || exit 1
    material="${TCFS_FAKE_SIGNING_CERT:-tcfs-signing-cert}"
    if [[ "$bundle" == *"TCFSFileProvider.appex"* && -n "${TCFS_FAKE_EXTENSION_SIGNING_CERT:-}" ]]; then
      material="$TCFS_FAKE_EXTENSION_SIGNING_CERT"
    elif [[ "$bundle" != *"TCFSFileProvider.appex"* && -n "${TCFS_FAKE_HOST_SIGNING_CERT:-}" ]]; then
      material="$TCFS_FAKE_HOST_SIGNING_CERT"
    fi
    printf '%s' "$material" >"${prefix}0"
    exit 0
  fi

  if [[ "${TCFS_FAKE_KEYCHAIN_ENTITLEMENT:-0}" == "1" ]]; then
    cat <<'PLIST'
<plist><dict><key>com.apple.security.application-groups</key><array><string>group.io.tinyland.tcfs</string></array><key>keychain-access-groups</key><array><string>QP994XQKNH.group.io.tinyland.tcfs</string></array></dict></plist>
PLIST
  else
    cat <<'PLIST'
<plist><dict></dict></plist>
PLIST
  fi
  exit 0
fi
exit 1
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

OUT="${TMPDIR}/positive.out"
ERR="${TMPDIR}/positive.err"
PATH="$FAKE_BIN:$PATH" \
HOME="$HOME_DIR" \
bash "$SCRIPT" \
  --expected-version 0.12.2 \
  --config "$CONFIG_PATH" \
  --fileprovider-config "$FILEPROVIDER_CONFIG" \
  --app-path "$APP_PATH" \
  --cloud-root "$CLOUD_ROOT" \
  >"$OUT" \
  2>"$ERR"

assert_contains "$OUT" "tcfs version: tcfs 0.12.2"
assert_contains "$OUT" "tcfsd version: tcfsd 0.12.2"
assert_contains "$OUT" "tcfs binary: $FAKE_BIN/tcfs"
assert_contains "$OUT" "tcfsd binary: $FAKE_BIN/tcfsd"
assert_contains "$OUT" "tcfs config: $CONFIG_PATH"
assert_contains "$OUT" "FileProvider config: $FILEPROVIDER_CONFIG"
assert_contains "$OUT" "host app: $APP_PATH"
assert_contains "$OUT" "host app bundle identifier: io.tinyland.tcfs"
assert_contains "$OUT" "host app codesign: valid"
assert_contains "$OUT" "FileProvider extension bundle identifier: io.tinyland.tcfs.fileprovider"
assert_contains "$OUT" "FileProvider extension codesign: valid"
assert_contains "$OUT" "pluginkit registration:"
assert_contains "$OUT" "fileproviderctl domain listing includes io.tinyland.tcfs"
assert_contains "$OUT" "CloudStorage root: $CLOUD_ROOT"
assert_contains "$OUT" "macOS FileProvider preflight passed"
assert_contains "$ERR" "warning: host app app group entitlement missing"
assert_contains "$ERR" "warning: host app keychain access group entitlement missing"
assert_contains "$ERR" "warning: host app provisioning profile missing"
assert_contains "$ERR" "warning: FileProvider extension app group entitlement missing"
assert_contains "$ERR" "warning: FileProvider extension keychain access group entitlement missing"
assert_contains "$ERR" "warning: FileProvider extension provisioning profile missing"

assert_fails_contains \
  "host app keychain access group entitlement missing" \
  env PATH="$FAKE_BIN:$PATH" HOME="$HOME_DIR" \
    bash "$SCRIPT" \
      --expected-version 0.12.2 \
      --config "$CONFIG_PATH" \
      --fileprovider-config "$FILEPROVIDER_CONFIG" \
      --app-path "$APP_PATH" \
      --cloud-root "$CLOUD_ROOT" \
      --require-production-signing
assert_contains "${TMPDIR}/failure.combined" "host app app group entitlement missing"
assert_contains "${TMPDIR}/failure.combined" "host app provisioning profile missing"
assert_contains "${TMPDIR}/failure.combined" "FileProvider extension app group entitlement missing"
assert_contains "${TMPDIR}/failure.combined" "FileProvider extension keychain access group entitlement missing"
assert_contains "${TMPDIR}/failure.combined" "FileProvider extension provisioning profile missing"
assert_contains "${TMPDIR}/failure.combined" "production signing preflight failed with 6 issue(s)"

write_profile_plist \
  "$APP_PATH/Contents/embedded.provisionprofile" \
  "io.tinyland.tcfs"
write_profile_plist \
  "$APP_PATH/Contents/Extensions/TCFSFileProvider.appex/Contents/embedded.provisionprofile" \
  "io.tinyland.tcfs.fileprovider"

STRICT_OUT="${TMPDIR}/strict.out"
PATH="$FAKE_BIN:$PATH" \
HOME="$HOME_DIR" \
TCFS_FAKE_KEYCHAIN_ENTITLEMENT=1 \
bash "$SCRIPT" \
  --expected-version 0.12.2 \
  --config "$CONFIG_PATH" \
  --fileprovider-config "$FILEPROVIDER_CONFIG" \
  --app-path "$APP_PATH" \
  --cloud-root "$CLOUD_ROOT" \
  --require-production-signing \
  >"$STRICT_OUT"

assert_contains "$STRICT_OUT" "host app app group entitlement: group.io.tinyland.tcfs"
assert_contains "$STRICT_OUT" "host app keychain access group entitlement: QP994XQKNH.group.io.tinyland.tcfs"
assert_contains "$STRICT_OUT" "host app provisioning profile: $APP_PATH/Contents/embedded.provisionprofile"
assert_contains "$STRICT_OUT" "host app provisioning profile app group: group.io.tinyland.tcfs"
assert_contains "$STRICT_OUT" "host app provisioning profile keychain group: QP994XQKNH.group.io.tinyland.tcfs"
assert_contains "$STRICT_OUT" "host app provisioning profile application identifier: QP994XQKNH.io.tinyland.tcfs"
assert_contains "$STRICT_OUT" "host app provisioning profile contains signing certificate: $SIGNING_CERT_SHA1"
assert_contains "$STRICT_OUT" "FileProvider extension app group entitlement: group.io.tinyland.tcfs"
assert_contains "$STRICT_OUT" "FileProvider extension keychain access group entitlement: QP994XQKNH.group.io.tinyland.tcfs"
assert_contains "$STRICT_OUT" "FileProvider extension provisioning profile: $APP_PATH/Contents/Extensions/TCFSFileProvider.appex/Contents/embedded.provisionprofile"
assert_contains "$STRICT_OUT" "FileProvider extension provisioning profile app group: group.io.tinyland.tcfs"
assert_contains "$STRICT_OUT" "FileProvider extension provisioning profile keychain group: QP994XQKNH.group.io.tinyland.tcfs"
assert_contains "$STRICT_OUT" "FileProvider extension provisioning profile application identifier: QP994XQKNH.io.tinyland.tcfs.fileprovider"
assert_contains "$STRICT_OUT" "FileProvider extension provisioning profile contains signing certificate: $SIGNING_CERT_SHA1"
assert_contains "$STRICT_OUT" "macOS FileProvider preflight passed"

SIGNING_ONLY_OUT="${TMPDIR}/signing-only.out"
PATH="$FAKE_BIN:$PATH" \
HOME="$HOME_DIR" \
TCFS_FAKE_KEYCHAIN_ENTITLEMENT=1 \
bash "$SCRIPT" \
  --app-path "$APP_PATH" \
  --signing-only \
  --require-production-signing \
  >"$SIGNING_ONLY_OUT"

assert_contains "$SIGNING_ONLY_OUT" "host app bundle identifier: io.tinyland.tcfs"
assert_contains "$SIGNING_ONLY_OUT" "FileProvider extension bundle identifier: io.tinyland.tcfs.fileprovider"
assert_contains "$SIGNING_ONLY_OUT" "macOS FileProvider signing preflight passed"
if grep -Fq "tcfs version:" "$SIGNING_ONLY_OUT"; then
  printf 'signing-only mode unexpectedly ran tcfs version checks\n' >&2
  exit 1
fi

write_profile_plist \
  "$APP_PATH/Contents/Extensions/TCFSFileProvider.appex/Contents/embedded.provisionprofile" \
  "io.tinyland.tcfs"
assert_fails_contains \
  "FileProvider extension provisioning profile application identifier mismatch" \
  env PATH="$FAKE_BIN:$PATH" HOME="$HOME_DIR" TCFS_FAKE_KEYCHAIN_ENTITLEMENT=1 \
    bash "$SCRIPT" \
      --expected-version 0.12.2 \
      --config "$CONFIG_PATH" \
      --fileprovider-config "$FILEPROVIDER_CONFIG" \
      --app-path "$APP_PATH" \
      --cloud-root "$CLOUD_ROOT" \
      --require-production-signing
assert_contains "${TMPDIR}/failure.combined" "production signing preflight failed with 1 issue(s)"

write_profile_plist \
  "$APP_PATH/Contents/Extensions/TCFSFileProvider.appex/Contents/embedded.provisionprofile" \
  "io.tinyland.tcfs.fileprovider"
assert_fails_contains \
  "FileProvider extension provisioning profile does not contain bundle signing certificate" \
  env PATH="$FAKE_BIN:$PATH" HOME="$HOME_DIR" TCFS_FAKE_KEYCHAIN_ENTITLEMENT=1 TCFS_FAKE_EXTENSION_SIGNING_CERT="wrong-signing-cert" \
    bash "$SCRIPT" \
      --expected-version 0.12.2 \
      --config "$CONFIG_PATH" \
      --fileprovider-config "$FILEPROVIDER_CONFIG" \
      --app-path "$APP_PATH" \
      --cloud-root "$CLOUD_ROOT" \
      --require-production-signing
assert_contains "${TMPDIR}/failure.combined" "production signing preflight failed with 1 issue(s)"

DUPES_OUT="${TMPDIR}/dupes.out"
DUPES_ERR="${TMPDIR}/dupes.err"
if env PATH="$FAKE_BIN:$PATH" HOME="$HOME_DIR" TCFS_FAKE_PLUGIN_DUPES=1 \
  bash "$SCRIPT" \
    --expected-version 0.12.2 \
    --config "$CONFIG_PATH" \
    --fileprovider-config "$FILEPROVIDER_CONFIG" \
    --app-path "$APP_PATH" \
    --cloud-root "$CLOUD_ROOT" \
    >"$DUPES_OUT" \
    2>"$DUPES_ERR"; then
  printf 'expected duplicate pluginkit registrations to fail\n' >&2
  exit 1
fi
cat "$DUPES_OUT" "$DUPES_ERR" >"${TMPDIR}/dupes.combined"
assert_contains "${TMPDIR}/dupes.combined" "multiple FileProvider registrations found"
assert_contains "${TMPDIR}/dupes.combined" "registered FileProvider extension paths:"
assert_contains "${TMPDIR}/dupes.combined" "/Users/test/git/tummycrypt/build/TCFSProvider.app"
assert_contains "${TMPDIR}/dupes.combined" "cleanup is not performed automatically"

UNAVAILABLE_OUT="${TMPDIR}/unavailable.out"
UNAVAILABLE_ERR="${TMPDIR}/unavailable.err"
PATH="$FAKE_BIN:$PATH" \
HOME="$HOME_DIR" \
TCFS_FAKE_FILEPROVIDERCTL=unavailable \
bash "$SCRIPT" \
  --expected-version 0.12.2 \
  --config "$CONFIG_PATH" \
  --fileprovider-config "$FILEPROVIDER_CONFIG" \
  --app-path "$APP_PATH" \
  --cloud-root "$CLOUD_ROOT" \
  >"$UNAVAILABLE_OUT" \
  2>"$UNAVAILABLE_ERR"

assert_contains "$UNAVAILABLE_OUT" "macOS FileProvider preflight passed"
assert_contains "$UNAVAILABLE_ERR" "fileproviderctl domain list unavailable"

printf 'macOS FileProvider preflight tests passed\n'
