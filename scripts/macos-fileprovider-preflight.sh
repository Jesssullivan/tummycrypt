#!/usr/bin/env bash
#
# Non-mutating readiness check for the macOS TCFS FileProvider surface.
#
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/macos-fileprovider-preflight.sh [options]

Verify local macOS FileProvider readiness without launching TCFSProvider.app or
changing FileProvider domain state.

Options:
  --config <path>             tcfs config path
                              (default: ~/.config/tcfs/config.toml)
  --fileprovider-config <path>
                              FileProvider config path
                              (default: ~/.config/tcfs/fileprovider/config.json)
  --expected-version <ver>    Require tcfs/tcfsd --version output to include this string
  --app-path <path>           Installed TCFSProvider.app path
                              (default: auto-detect /Applications or ~/Applications)
  --cloud-root <path>         Expected CloudStorage root
                              (default: auto-detect ~/Library/CloudStorage/TCFS*)
  --plugin-id <id>            FileProvider extension bundle id
                              (default: io.tinyland.tcfs.fileprovider)
  --domain-id <id>            FileProvider domain id
                              (default: io.tinyland.tcfs)
  --allow-multiple-plugin-registrations
                              Warn instead of failing if pluginkit shows more
                              than one registration for --plugin-id
  --signing-only
                              Only inspect TCFSProvider.app signing,
                              entitlements, and embedded profiles
  --require-production-signing
                              Fail unless host app and extension have
                              keychain-access-groups entitlements and embedded
                              provisioning profiles
  --tcfs <path-or-name>       CLI binary to use (default: tcfs)
  --tcfsd <path-or-name>      Daemon binary to use (default: tcfsd)
  -h, --help                  Show this help
EOF
}

CONFIG_PATH="${TCFS_CONFIG:-$HOME/.config/tcfs/config.toml}"
FILEPROVIDER_CONFIG="${TCFS_FILEPROVIDER_CONFIG:-$HOME/.config/tcfs/fileprovider/config.json}"
EXPECTED_VERSION=""
APP_PATH="${TCFS_APP_PATH:-}"
CLOUD_ROOT="${TCFS_CLOUD_ROOT:-}"
PLUGIN_ID="${TCFS_PLUGIN_ID:-io.tinyland.tcfs.fileprovider}"
DOMAIN_ID="${TCFS_DOMAIN_ID:-io.tinyland.tcfs}"
ALLOW_MULTIPLE_PLUGIN_REGISTRATIONS="${TCFS_ALLOW_MULTIPLE_PLUGIN_REGISTRATIONS:-0}"
SIGNING_ONLY="${TCFS_SIGNING_ONLY:-0}"
REQUIRE_PRODUCTION_SIGNING="${TCFS_REQUIRE_PRODUCTION_SIGNING:-0}"
APP_GROUP_ID="${TCFS_APP_GROUP_ID:-group.io.tinyland.tcfs}"
KEYCHAIN_GROUP_SUFFIX="${TCFS_KEYCHAIN_GROUP_SUFFIX:-group.io.tinyland.tcfs}"
TCFS_BIN="${TCFS_BIN:-tcfs}"
TCFSD_BIN="${TCFSD_BIN:-tcfsd}"
SIGNING_FAILURES=0
SIGNING_TMP_DIR=""

cleanup() {
  if [[ -n "$SIGNING_TMP_DIR" ]]; then
    rm -rf "$SIGNING_TMP_DIR"
  fi
}
trap cleanup EXIT

while [[ $# -gt 0 ]]; do
  case "$1" in
    --config)
      CONFIG_PATH="$2"
      shift 2
      ;;
    --fileprovider-config)
      FILEPROVIDER_CONFIG="$2"
      shift 2
      ;;
    --expected-version)
      EXPECTED_VERSION="$2"
      shift 2
      ;;
    --app-path)
      APP_PATH="$2"
      shift 2
      ;;
    --cloud-root)
      CLOUD_ROOT="$2"
      shift 2
      ;;
    --plugin-id)
      PLUGIN_ID="$2"
      shift 2
      ;;
    --domain-id)
      DOMAIN_ID="$2"
      shift 2
      ;;
    --allow-multiple-plugin-registrations)
      ALLOW_MULTIPLE_PLUGIN_REGISTRATIONS=1
      shift
      ;;
    --signing-only)
      SIGNING_ONLY=1
      shift
      ;;
    --require-production-signing)
      REQUIRE_PRODUCTION_SIGNING=1
      shift
      ;;
    --tcfs)
      TCFS_BIN="$2"
      shift 2
      ;;
    --tcfsd)
      TCFSD_BIN="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "scripts/macos-fileprovider-preflight.sh only runs on macOS" >&2
  exit 1
fi

require_file() {
  local path="$1"
  [[ -f "$path" ]] || {
    echo "required file not found: $path" >&2
    exit 1
  }
}

require_dir() {
  local path="$1"
  [[ -d "$path" ]] || {
    echo "required directory not found: $path" >&2
    exit 1
  }
}

resolve_bin() {
  local candidate="$1"
  if [[ "$candidate" == */* ]]; then
    [[ -x "$candidate" ]] || {
      echo "binary is not executable: $candidate" >&2
      exit 1
    }
    printf '%s\n' "$candidate"
    return
  fi

  command -v "$candidate" >/dev/null 2>&1 || {
    echo "command not found: $candidate" >&2
    exit 1
  }
  command -v "$candidate"
}

check_version() {
  local label="$1"
  local bin="$2"
  local output

  output="$("$bin" --version)"
  echo "$label binary: $bin"
  echo "$label version: $output"

  if [[ -n "$EXPECTED_VERSION" && "$output" != *"$EXPECTED_VERSION"* ]]; then
    echo "$label version mismatch: expected output containing '$EXPECTED_VERSION'" >&2
    exit 1
  fi
}

detect_app_path() {
  if [[ -n "$APP_PATH" ]]; then
    require_dir "$APP_PATH"
    printf '%s\n' "$APP_PATH"
    return
  fi

  local candidate
  for candidate in \
    "/Applications/TCFSProvider.app" \
    "$HOME/Applications/TCFSProvider.app"
  do
    if [[ -d "$candidate" ]]; then
      printf '%s\n' "$candidate"
      return
    fi
  done

  echo "TCFSProvider.app not found in /Applications or ~/Applications" >&2
  exit 1
}

check_pluginkit() {
  local output
  local count

  output="$(pluginkit -m -A -D -vvv -i "$PLUGIN_ID" 2>&1)" || {
    echo "pluginkit lookup failed for $PLUGIN_ID" >&2
    echo "$output" >&2
    exit 1
  }

  echo "pluginkit registration:"
  echo "$output"

  grep -q "$PLUGIN_ID" <<<"$output" || {
    echo "pluginkit output did not include $PLUGIN_ID" >&2
    exit 1
  }

  count="$(grep -c "$PLUGIN_ID" <<<"$output")"
  if (( count > 1 )); then
    if [[ "$ALLOW_MULTIPLE_PLUGIN_REGISTRATIONS" == "1" ]]; then
      echo "warning: pluginkit shows $count registrations for $PLUGIN_ID" >&2
    else
      echo "multiple FileProvider registrations found for $PLUGIN_ID; remove stale app/extension copies or pass --allow-multiple-plugin-registrations for diagnostic runs" >&2
      print_pluginkit_duplicate_hint "$output"
      exit 1
    fi
  fi
}

print_pluginkit_duplicate_hint() {
  local output="$1"

  echo "registered FileProvider extension paths:" >&2
  awk '
    /^[[:space:]]*Path = / {
      path = $0
      sub(/^[[:space:]]*Path = /, "", path)
      print "  extension: " path
    }
    /^[[:space:]]*Parent Bundle = / {
      parent = $0
      sub(/^[[:space:]]*Parent Bundle = /, "", parent)
      print "  parent app: " parent
    }
  ' <<<"$output" >&2
  echo "cleanup is not performed automatically; remove stale app/extension copies or run pluginkit -r intentionally, then rerun preflight" >&2
}

check_domain_listing() {
  if ! command -v fileproviderctl >/dev/null 2>&1; then
    echo "warning: fileproviderctl not found; skipping domain listing" >&2
    return
  fi

  local output
  if output="$(fileproviderctl domain list 2>&1)"; then
    if grep -q "$DOMAIN_ID" <<<"$output"; then
      echo "fileproviderctl domain listing includes $DOMAIN_ID"
    else
      echo "warning: fileproviderctl domain listing did not include $DOMAIN_ID" >&2
      echo "$output" >&2
    fi
  else
    echo "warning: fileproviderctl domain list unavailable on this host" >&2
    echo "$output" >&2
  fi
}

check_cloud_roots() {
  if [[ -n "$CLOUD_ROOT" ]]; then
    require_dir "$CLOUD_ROOT"
    echo "CloudStorage root: $CLOUD_ROOT"
    return
  fi

  local roots=()
  local candidate
  while IFS= read -r candidate; do
    roots+=("$candidate")
  done < <(find "$HOME/Library/CloudStorage" -mindepth 1 -maxdepth 1 -type d -name 'TCFS*' 2>/dev/null | sort)

  case "${#roots[@]}" in
    0)
      echo "warning: no TCFS CloudStorage root found under $HOME/Library/CloudStorage" >&2
      ;;
    1)
      echo "CloudStorage root: ${roots[0]}"
      ;;
    *)
      echo "multiple TCFS CloudStorage roots found; pass --cloud-root explicitly" >&2
      printf '  %s\n' "${roots[@]}" >&2
      exit 1
      ;;
  esac
}

signing_warning() {
  local message="$1"
  if [[ "$REQUIRE_PRODUCTION_SIGNING" == "1" ]]; then
    echo "$message" >&2
    SIGNING_FAILURES=$((SIGNING_FAILURES + 1))
    return
  fi
  echo "warning: $message" >&2
}

plist_print() {
  local plist="$1"
  local path="$2"

  /usr/libexec/PlistBuddy -c "Print :$path" "$plist" 2>/dev/null
}

plist_first_array_value() {
  local plist="$1"
  local path="$2"

  plist_print "$plist" "$path:0"
}

plist_array_contains() {
  local plist="$1"
  local path="$2"
  local expected="$3"
  local index=0
  local value

  while value="$(plist_print "$plist" "$path:$index")"; do
    if [[ "$value" == "$expected" ]]; then
      return 0
    fi
    index=$((index + 1))
  done

  return 1
}

keychain_group_matches_suffix() {
  local group="$1"

  [[ "$group" == *".$KEYCHAIN_GROUP_SUFFIX" ]]
}

keychain_group_prefix() {
  local group="$1"

  if keychain_group_matches_suffix "$group"; then
    printf '%s\n' "${group%."$KEYCHAIN_GROUP_SUFFIX"}"
  fi
}

keychain_group_covers_signed() {
  local profile_group="$1"
  local signed_group="$2"
  local profile_prefix

  [[ -n "$profile_group" && -n "$signed_group" ]] || return 1
  if [[ "$profile_group" == "$signed_group" ]]; then
    return 0
  fi

  if [[ "$profile_group" == *".*" ]]; then
    profile_prefix="${profile_group%.*}"
    [[ "$signed_group" == "$profile_prefix."* ]]
    return
  fi

  return 1
}

profile_application_id() {
  local plist="$1"
  local app_id

  app_id="$(plist_print "$plist" "Entitlements:application-identifier" || true)"
  if [[ -n "$app_id" ]]; then
    printf '%s\n' "$app_id"
    return
  fi

  plist_print "$plist" "Entitlements:com.apple.application-identifier" || true
}

cert_sha1() {
  local cert_file="$1"

  openssl dgst -sha1 "$cert_file" | awk '{ print toupper($NF) }'
}

base64_decode_file() {
  local input="$1"
  local output="$2"

  if base64 -D <"$input" >"$output" 2>/dev/null; then
    return 0
  fi

  base64 --decode <"$input" >"$output"
}

bundle_signing_cert_sha1() {
  local label="$1"
  local bundle="$2"
  local cert_prefix="$SIGNING_TMP_DIR/${label// /-}-codesign-cert-"
  local cert_file="${cert_prefix}0"

  rm -f "${cert_prefix}"*
  if ! codesign -d --extract-certificates="$cert_prefix" "$bundle" >/dev/null 2>&1; then
    signing_warning "$label signing certificate could not be extracted"
    return 1
  fi

  if [[ ! -s "$cert_file" ]]; then
    signing_warning "$label signing certificate extraction produced no leaf certificate"
    return 1
  fi

  cert_sha1 "$cert_file"
}

profile_developer_cert_sha1s() {
  local label="$1"
  local profile_plist="$2"
  local index=0
  local b64_file
  local cert_file

  while true; do
    b64_file="$SIGNING_TMP_DIR/${label// /-}-profile-cert-${index}.b64"
    cert_file="$SIGNING_TMP_DIR/${label// /-}-profile-cert-${index}.der"

    if ! plutil -extract "DeveloperCertificates.$index" raw -o "$b64_file" "$profile_plist" 2>/dev/null; then
      break
    fi

    if base64_decode_file "$b64_file" "$cert_file" 2>/dev/null; then
      cert_sha1 "$cert_file"
    else
      signing_warning "$label provisioning profile DeveloperCertificates:$index could not be decoded"
    fi

    index=$((index + 1))
  done
}

check_profile_developer_certificate() {
  local label="$1"
  local bundle="$2"
  local profile_plist="$3"
  local bundle_sha1
  local profile_sha1
  local found=0
  local saw_profile_cert=0

  bundle_sha1="$(bundle_signing_cert_sha1 "$label" "$bundle")" || return

  while IFS= read -r profile_sha1; do
    [[ -n "$profile_sha1" ]] || continue
    saw_profile_cert=1
    if [[ "$profile_sha1" == "$bundle_sha1" ]]; then
      found=1
    fi
  done < <(profile_developer_cert_sha1s "$label" "$profile_plist")

  if [[ "$saw_profile_cert" == "0" ]]; then
    signing_warning "$label provisioning profile missing DeveloperCertificates"
  elif [[ "$found" == "1" ]]; then
    echo "$label provisioning profile contains signing certificate: $bundle_sha1"
  else
    signing_warning "$label provisioning profile does not contain bundle signing certificate: $bundle_sha1"
  fi
}

decode_provisioning_profile() {
  local label="$1"
  local profile="$2"
  local out="$3"
  local err="${out}.err"

  if security cms -D -i "$profile" >"$out" 2>"$err"; then
    rm -f "$err"
    return 0
  fi

  if openssl cms -verify -inform DER -noverify -in "$profile" -out "$out" >"$err" 2>&1; then
    rm -f "$err"
    return 0
  fi

  signing_warning "$label provisioning profile could not be decoded"
  if [[ -s "$err" ]]; then
    cat "$err" >&2
  fi

  return 1
}

check_signed_entitlements() {
  local label="$1"
  local entitlements_file="$2"
  local app_group_ok=0
  local keychain_group

  if plist_array_contains "$entitlements_file" "com.apple.security.application-groups" "$APP_GROUP_ID"; then
    echo "$label app group entitlement: $APP_GROUP_ID"
    app_group_ok=1
  else
    signing_warning "$label app group entitlement missing: $APP_GROUP_ID"
  fi

  keychain_group="$(plist_first_array_value "$entitlements_file" "keychain-access-groups" || true)"
  if [[ -z "$keychain_group" ]]; then
    signing_warning "$label keychain access group entitlement missing; no-embedded production Keychain path unavailable"
  elif keychain_group_matches_suffix "$keychain_group"; then
    echo "$label keychain access group entitlement: $keychain_group"
  else
    signing_warning "$label keychain access group entitlement does not end with .$KEYCHAIN_GROUP_SUFFIX: $keychain_group"
  fi

  [[ "$app_group_ok" == "1" && -n "$keychain_group" ]]
}

check_profile_entitlements() {
  local label="$1"
  local profile_plist="$2"
  local bundle_id="$3"
  local signed_keychain_group="$4"
  local signed_team_prefix
  local profile_keychain_group
  local app_id

  if plist_array_contains "$profile_plist" "Entitlements:com.apple.security.application-groups" "$APP_GROUP_ID"; then
    echo "$label provisioning profile app group: $APP_GROUP_ID"
  else
    signing_warning "$label provisioning profile missing app group: $APP_GROUP_ID"
  fi

  profile_keychain_group="$(plist_first_array_value "$profile_plist" "Entitlements:keychain-access-groups" || true)"
  if [[ -z "$profile_keychain_group" ]]; then
    signing_warning "$label provisioning profile missing keychain access group"
  elif [[ -n "$signed_keychain_group" ]] && ! keychain_group_covers_signed "$profile_keychain_group" "$signed_keychain_group"; then
    signing_warning "$label provisioning profile keychain group does not cover signed entitlement: $profile_keychain_group != $signed_keychain_group"
  elif [[ -z "$signed_keychain_group" && "$profile_keychain_group" != *".*" ]] && ! keychain_group_matches_suffix "$profile_keychain_group"; then
    signing_warning "$label provisioning profile keychain group does not match .$KEYCHAIN_GROUP_SUFFIX or a team wildcard: $profile_keychain_group"
  else
    echo "$label provisioning profile keychain group: $profile_keychain_group"
  fi

  app_id="$(profile_application_id "$profile_plist")"
  signed_team_prefix="$(keychain_group_prefix "$signed_keychain_group")"
  if [[ -z "$app_id" ]]; then
    signing_warning "$label provisioning profile missing application identifier"
  elif [[ -n "$signed_team_prefix" && "$app_id" != "$signed_team_prefix.$bundle_id" ]]; then
    signing_warning "$label provisioning profile application identifier mismatch: $app_id != $signed_team_prefix.$bundle_id"
  elif [[ -z "$signed_team_prefix" && "$app_id" != *".$bundle_id" ]]; then
    signing_warning "$label provisioning profile application identifier does not match bundle id: $app_id"
  else
    echo "$label provisioning profile application identifier: $app_id"
  fi
}

check_signed_bundle() {
  local label="$1"
  local bundle="$2"
  local profile="$bundle/Contents/embedded.provisionprofile"
  local bundle_info="$bundle/Contents/Info.plist"
  local bundle_id
  local entitlements_file
  local profile_plist
  local verify_output
  local entitlements
  local signed_keychain_group

  require_dir "$bundle"
  require_file "$bundle_info"
  bundle_id="$(plist_print "$bundle_info" "CFBundleIdentifier" || true)"
  if [[ -z "$bundle_id" ]]; then
    signing_warning "$label bundle identifier missing"
  else
    echo "$label bundle identifier: $bundle_id"
  fi

  verify_output="$(codesign -vvv "$bundle" 2>&1)" || {
    echo "$label codesign validation failed" >&2
    echo "$verify_output" >&2
    exit 1
  }
  echo "$label codesign: valid"

  mkdir -p "$SIGNING_TMP_DIR"
  entitlements_file="$SIGNING_TMP_DIR/${label// /-}-entitlements.plist"
  entitlements="$(codesign -d --entitlements :- "$bundle" 2>/dev/null || true)"
  if [[ -n "$entitlements" ]]; then
    printf '%s\n' "$entitlements" >"$entitlements_file"
    check_signed_entitlements "$label" "$entitlements_file" || true
    signed_keychain_group="$(plist_first_array_value "$entitlements_file" "keychain-access-groups" || true)"
  else
    signing_warning "$label signed entitlements unavailable"
    signed_keychain_group=""
  fi

  if [[ -f "$profile" ]]; then
    echo "$label provisioning profile: $profile"
    profile_plist="$SIGNING_TMP_DIR/${label// /-}-profile.plist"
    if [[ -n "$bundle_id" ]] && decode_provisioning_profile "$label" "$profile" "$profile_plist"; then
      check_profile_entitlements "$label" "$profile_plist" "$bundle_id" "$signed_keychain_group"
      check_profile_developer_certificate "$label" "$bundle" "$profile_plist"
    fi
  else
    signing_warning "$label provisioning profile missing; restricted entitlements may fail at launch"
  fi
}

check_signing() {
  local extension_path="$APP_PATH/Contents/Extensions/TCFSFileProvider.appex"

  SIGNING_FAILURES=0
  SIGNING_TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-macos-preflight-signing.XXXXXX")"
  check_signed_bundle "host app" "$APP_PATH"
  check_signed_bundle "FileProvider extension" "$extension_path"
  if [[ "$REQUIRE_PRODUCTION_SIGNING" == "1" && "$SIGNING_FAILURES" -gt 0 ]]; then
    echo "production signing preflight failed with $SIGNING_FAILURES issue(s)" >&2
    exit 1
  fi
}

if [[ "$SIGNING_ONLY" != "1" ]]; then
  TCFS_PATH="$(resolve_bin "$TCFS_BIN")"
  TCFSD_PATH="$(resolve_bin "$TCFSD_BIN")"
  check_version "tcfs" "$TCFS_PATH"
  check_version "tcfsd" "$TCFSD_PATH"

  require_file "$CONFIG_PATH"
  echo "tcfs config: $CONFIG_PATH"

  require_file "$FILEPROVIDER_CONFIG"
  echo "FileProvider config: $FILEPROVIDER_CONFIG"
fi

APP_PATH="$(detect_app_path)"
echo "host app: $APP_PATH"

check_signing
if [[ "$SIGNING_ONLY" == "1" ]]; then
  echo "macOS FileProvider signing preflight passed"
  exit 0
fi

check_pluginkit
check_domain_listing
check_cloud_roots

echo "macOS FileProvider preflight passed"
