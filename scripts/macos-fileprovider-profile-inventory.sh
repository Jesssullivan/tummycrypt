#!/usr/bin/env bash
#
# Field-compatibility inventory for TCFS macOS FileProvider profiles.
# Decoding is not Apple authorization proof; security cms can import certificates
# into its command keychain context. This tool performs no signing or installation.
#
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/macos-fileprovider-profile-inventory.sh [options]

Inspect decoded profile fields for host-app / FileProvider-extension
compatibility. This is not independent Apple profile authentication.

Options:
  --host-profile <path>       Check exactly this host profile and the declared
  --extension-profile <path>  extension profile; both absolute regular paths are
                              required. Exact-pair mode is always strict and
                              cannot be combined with --profiles-dir.
  --profiles-dir <path>       Provisioning profile directory
                              (default: ~/Library/MobileDevice/Provisioning Profiles)
  --host-bundle-id <id>       Host app bundle id
                              (default: io.tinyland.tcfs)
  --extension-bundle-id <id>  FileProvider extension bundle id
                              (default: io.tinyland.tcfs.fileprovider)
  --app-group <id>            Required App Group entitlement
                              (default: group.io.tinyland.tcfs)
  --keychain-group-suffix <s> Required Keychain group suffix
                              (default: group.io.tinyland.tcfs)
  --require-host-entitlement <key>
                              Require a boolean true entitlement on the host
                              profile. Intended for FileProvider testing mode.
  --env-only                  Print only shell assignments for the compatible
                              pair; intended for build automation
  --strict                    Fail unless a compatible host/extension pair exists
  -h, --help                  Show this help
EOF
}

fail() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

PROFILES_DIR="${TCFS_PROFILES_DIR:-$HOME/Library/MobileDevice/Provisioning Profiles}"
HOST_BUNDLE_ID="${TCFS_HOST_BUNDLE_ID:-io.tinyland.tcfs}"
EXTENSION_BUNDLE_ID="${TCFS_EXTENSION_BUNDLE_ID:-io.tinyland.tcfs.fileprovider}"
APP_GROUP_ID="${TCFS_APP_GROUP_ID:-group.io.tinyland.tcfs}"
KEYCHAIN_GROUP_SUFFIX="${TCFS_KEYCHAIN_GROUP_SUFFIX:-group.io.tinyland.tcfs}"
REQUIRED_HOST_ENTITLEMENT="${TCFS_REQUIRED_HOST_ENTITLEMENT:-}"
# Keep the native production tool; owned platform fixtures supply its stand-in.
PLISTBUDDY_BIN="${TCFS_PLISTBUDDY:-/usr/libexec/PlistBuddy}"
ENV_ONLY=0
STRICT=0
PROFILES_DIR_EXPLICIT=0
EXACT_HOST_PROFILE=""
EXACT_EXTENSION_PROFILE=""
EXACT_PAIR=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --profiles-dir)
      [[ $# -ge 2 ]] || fail "--profiles-dir requires a value"
      PROFILES_DIR="$2"
      PROFILES_DIR_EXPLICIT=1
      shift 2
      ;;
    --host-profile)
      [[ $# -ge 2 && -n "$2" && -z "$EXACT_HOST_PROFILE" ]] || fail "--host-profile requires one nonempty value"
      EXACT_HOST_PROFILE="$2"
      shift 2
      ;;
    --extension-profile)
      [[ $# -ge 2 && -n "$2" && -z "$EXACT_EXTENSION_PROFILE" ]] || fail "--extension-profile requires one nonempty value"
      EXACT_EXTENSION_PROFILE="$2"
      shift 2
      ;;
    --host-bundle-id)
      [[ $# -ge 2 ]] || fail "--host-bundle-id requires a value"
      HOST_BUNDLE_ID="$2"
      shift 2
      ;;
    --extension-bundle-id)
      [[ $# -ge 2 ]] || fail "--extension-bundle-id requires a value"
      EXTENSION_BUNDLE_ID="$2"
      shift 2
      ;;
    --app-group)
      [[ $# -ge 2 ]] || fail "--app-group requires a value"
      APP_GROUP_ID="$2"
      shift 2
      ;;
    --keychain-group-suffix)
      [[ $# -ge 2 ]] || fail "--keychain-group-suffix requires a value"
      KEYCHAIN_GROUP_SUFFIX="$2"
      shift 2
      ;;
    --require-host-entitlement)
      [[ $# -ge 2 ]] || fail "--require-host-entitlement requires a value"
      REQUIRED_HOST_ENTITLEMENT="$2"
      shift 2
      ;;
    --env-only)
      ENV_ONLY=1
      shift
      ;;
    --strict)
      STRICT=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      printf 'unknown argument: %s\n' "$1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ -n "$EXACT_HOST_PROFILE" || -n "$EXACT_EXTENSION_PROFILE" ]]; then
  [[ -n "$EXACT_HOST_PROFILE" && -n "$EXACT_EXTENSION_PROFILE" ]] || fail "exact-pair mode requires both profile paths"
  [[ "$PROFILES_DIR_EXPLICIT" == 0 ]] || fail "exact-pair mode cannot scan --profiles-dir"
  for profile in "$EXACT_HOST_PROFILE" "$EXACT_EXTENSION_PROFILE"; do
    [[ "$profile" == /* && -f "$profile" && ! -L "$profile" && -r "$profile" ]] || fail "exact profile must be an absolute readable regular file, not a symlink"
  done
  [[ ! "$EXACT_HOST_PROFILE" -ef "$EXACT_EXTENSION_PROFILE" ]] || fail "exact profiles must be distinct files"
  EXACT_PAIR=1
  STRICT=1
fi

if [[ "$(uname -s)" != "Darwin" ]]; then
  fail "scripts/macos-fileprovider-profile-inventory.sh only runs on macOS"
fi

plist_print() {
  local plist="$1"
  local path="$2"

  "$PLISTBUDDY_BIN" -c "Print :$path" "$plist" 2>/dev/null
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

team_prefix_from_app_id() {
  local app_id="$1"
  local bundle_id="$2"

  if [[ "$app_id" == *".$bundle_id" ]]; then
    printf '%s\n' "${app_id%."$bundle_id"}"
  fi
}

profile_keychain_group_covers_required() {
  local group="$1"
  local team_prefix="$2"

  [[ -n "$group" && -n "$team_prefix" ]] || return 1
  [[ "$group" == "$team_prefix.$KEYCHAIN_GROUP_SUFFIX" || "$group" == "$team_prefix.*" ]]
}

decode_profile() {
  local profile="$1"
  local out="$2"

  if security cms -D -i "$profile" >"$out" 2>/dev/null; then
    return 0
  fi

  openssl cms -verify -inform DER -noverify -in "$profile" -out "$out" >/dev/null 2>&1
}

profile_matches_bundle() {
  local plist="$1"
  local bundle_id="$2"
  local app_id
  local keychain_group
  local team_prefix

  app_id="$(profile_application_id "$plist")"
  keychain_group="$(plist_first_array_value "$plist" "Entitlements:keychain-access-groups" || true)"
  team_prefix="$(team_prefix_from_app_id "$app_id" "$bundle_id")"

  [[ -n "$app_id" ]] || return 1
  [[ -n "$team_prefix" ]] || return 1
  [[ "$app_id" == "$team_prefix.$bundle_id" ]] || return 1
  profile_keychain_group_covers_required "$keychain_group" "$team_prefix" || return 1
  plist_array_contains "$plist" "Entitlements:com.apple.security.application-groups" "$APP_GROUP_ID" || return 1
}

profile_entitlement_true() {
  local plist="$1"
  local entitlement="$2"
  local value

  [[ -n "$entitlement" ]] || return 0
  value="$(plist_print "$plist" "Entitlements:$entitlement" || true)"
  [[ "$value" == "true" ]]
}

profile_name() {
  local plist="$1"

  plist_print "$plist" "Name" || printf '(unnamed)'
}

profile_uuid() {
  local plist="$1"

  plist_print "$plist" "UUID" || printf '(no-uuid)'
}

print_profile_summary() {
  local label="$1"
  local profile="$2"
  local plist="$3"
  local app_id
  local keychain_group
  local team_prefix

  app_id="$(profile_application_id "$plist")"
  keychain_group="$(plist_first_array_value "$plist" "Entitlements:keychain-access-groups" || true)"
  if [[ "$app_id" == *.* ]]; then
    team_prefix="${app_id%%.*}"
  else
    team_prefix="$(keychain_group_prefix "$keychain_group")"
  fi

  printf '%s profile: %s\n' "$label" "$profile"
  printf '  name: %s\n' "$(profile_name "$plist")"
  printf '  uuid: %s\n' "$(profile_uuid "$plist")"
  printf '  team_prefix: %s\n' "${team_prefix:-unknown}"
  printf '  application_identifier: %s\n' "${app_id:-missing}"
  printf '  keychain_group: %s\n' "${keychain_group:-missing}"
}

profile_files=()
if [[ "$EXACT_PAIR" == 1 ]]; then
  # Preserve each declared role. Never substitute a profile from the directory,
  # even when an unrelated compatible pair exists beside these inputs.
  profile_files=("$EXACT_HOST_PROFILE" "$EXACT_EXTENSION_PROFILE")
elif [[ -d "$PROFILES_DIR" ]]; then
  while IFS= read -r profile; do
    profile_files+=("$profile")
  done < <(find "$PROFILES_DIR" -maxdepth 1 -type f \( -name '*.provisionprofile' -o -name '*.mobileprovision' \) | sort)
fi

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-profile-inventory.XXXXXX")"
trap 'rm -rf "$tmp_dir"' EXIT

host_profiles=()
host_plists=()
extension_profiles=()
extension_plists=()

if ((${#profile_files[@]} > 0)); then
  for profile in "${profile_files[@]}"; do
    plist="$tmp_dir/profile-${#host_plists[@]}-${#extension_plists[@]}.plist"
    if ! decode_profile "$profile" "$plist"; then
      printf 'warning: could not decode provisioning profile: %s\n' "$profile" >&2
      continue
    fi

    if [[ "$EXACT_PAIR" == 0 || "$profile" == "$EXACT_HOST_PROFILE" ]] \
      && profile_matches_bundle "$plist" "$HOST_BUNDLE_ID" \
      && profile_entitlement_true "$plist" "$REQUIRED_HOST_ENTITLEMENT"; then
      host_profiles+=("$profile")
      host_plists+=("$plist")
    fi
    if [[ "$EXACT_PAIR" == 0 || "$profile" == "$EXACT_EXTENSION_PROFILE" ]] \
      && profile_matches_bundle "$plist" "$EXTENSION_BUNDLE_ID"; then
      extension_profiles+=("$profile")
      extension_plists+=("$plist")
    fi
  done
fi

if [[ "$ENV_ONLY" != "1" ]]; then
  if [[ "$EXACT_PAIR" == 1 ]]; then
    printf 'profile selection: exact declared pair\n'
  else
    printf 'profiles dir: %s\n' "$PROFILES_DIR"
  fi
  printf 'profiles scanned: %s\n' "${#profile_files[@]}"
  printf 'required app group: %s\n' "$APP_GROUP_ID"
  printf 'required keychain suffix: %s\n' "$KEYCHAIN_GROUP_SUFFIX"
  if [[ -n "$REQUIRED_HOST_ENTITLEMENT" ]]; then
    printf 'required host entitlement: %s=true\n' "$REQUIRED_HOST_ENTITLEMENT"
  fi
  printf 'host bundle id: %s\n' "$HOST_BUNDLE_ID"
  printf 'extension bundle id: %s\n' "$EXTENSION_BUNDLE_ID"
  printf 'host candidates: %s\n' "${#host_profiles[@]}"
  printf 'extension candidates: %s\n' "${#extension_profiles[@]}"
fi

host_choice=""
extension_choice=""
host_choice_plist=""
extension_choice_plist=""

if ((${#host_profiles[@]} > 0 && ${#extension_profiles[@]} > 0)); then
  for host_index in "${!host_profiles[@]}"; do
    host_app_id="$(profile_application_id "${host_plists[$host_index]}")"
    host_team_prefix="$(team_prefix_from_app_id "$host_app_id" "$HOST_BUNDLE_ID")"
    for extension_index in "${!extension_profiles[@]}"; do
      extension_app_id="$(profile_application_id "${extension_plists[$extension_index]}")"
      extension_team_prefix="$(team_prefix_from_app_id "$extension_app_id" "$EXTENSION_BUNDLE_ID")"
      if [[ -n "$host_team_prefix" && "$host_team_prefix" == "$extension_team_prefix" ]]; then
        host_choice="${host_profiles[$host_index]}"
        extension_choice="${extension_profiles[$extension_index]}"
        host_choice_plist="${host_plists[$host_index]}"
        extension_choice_plist="${extension_plists[$extension_index]}"
        break 2
      fi
    done
  done
fi

if [[ -n "$host_choice" && -n "$extension_choice" ]]; then
  if [[ "$ENV_ONLY" != "1" ]]; then
    printf 'compatible pair: found\n'
    print_profile_summary "host" "$host_choice" "$host_choice_plist"
    print_profile_summary "extension" "$extension_choice" "$extension_choice_plist"
    printf '\n'
  fi
  printf 'TCFS_HOST_PROVISIONING_PROFILE=%q\n' "$host_choice"
  printf 'TCFS_EXTENSION_PROVISIONING_PROFILE=%q\n' "$extension_choice"
  printf 'TCFS_REQUIRE_PRODUCTION_SIGNING=1\n'
  exit 0
fi

if [[ "$ENV_ONLY" != "1" ]]; then
  printf 'compatible pair: not found\n'
fi
if [[ "$STRICT" == "1" ]]; then
  exit 1
fi
