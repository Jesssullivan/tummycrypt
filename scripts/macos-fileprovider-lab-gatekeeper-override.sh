#!/usr/bin/env bash
#
# Generate and verify the non-production Gatekeeper SystemPolicyRule profile
# needed by the TCFS FileProvider lab app on a registered development Mac.
#
# macOS 15 no longer supports mutating Gatekeeper's assessment rule database
# with spctl --add/--remove. Apple points that path at configuration profiles.
# This helper therefore does not change system policy directly: it writes the
# desired .mobileconfig into the run logs, verifies whether that profile is
# already installed, and fails with a clear operator action if it is missing.
#
set -euo pipefail

APP_PATH="${TCFS_FILEPROVIDER_APP_PATH:-/Applications/TCFSProvider.app}"
EXTENSION_PATH="${TCFS_FILEPROVIDER_EXTENSION_PATH:-${APP_PATH}/Contents/Extensions/TCFSFileProvider.appex}"
PROFILE_IDENTIFIER="${TCFS_FILEPROVIDER_LAB_SYSTEM_POLICY_PROFILE_ID:-io.tinyland.tcfs.fileprovider.lab.system-policy}"
LOG_DIR="${LOG_DIR:-${RUNNER_TEMP:-/tmp}/tcfs-fileprovider-lab-gatekeeper}"
PROFILE_OUTPUT="${TCFS_FILEPROVIDER_LAB_SYSTEM_POLICY_PROFILE:-}"
SUDO_PASSWORD_FILE="${TCFS_RUNNER_SUDO_PASSWORD_FILE:-$HOME/.config/sops-nix/secrets/become/password}"
MODE=""

usage() {
  cat <<'USAGE'
Usage: scripts/macos-fileprovider-lab-gatekeeper-override.sh <apply|generate|verify|cleanup> [options]

Options:
  --app-path <path>       Installed host app path (default: /Applications/TCFSProvider.app)
  --extension-path <path> Installed FileProvider extension path
  --profile-id <id>       Configuration profile PayloadIdentifier
  --profile-output <path> Output .mobileconfig path
  --log-dir <path>        Directory for generated profile and verification logs
  -h, --help              Show this help

Modes:
  generate  Write the desired SystemPolicyRule .mobileconfig from installed bundle requirements.
  verify    Verify the profile identifier is installed on this Mac.
  apply     Generate the desired profile, then verify it is installed.
  cleanup   No-op marker; profile installation/removal is manual or MDM-managed.

On macOS 15+, install configuration profiles through System Settings or MDM.
The profiles CLI can list/remove profiles, but it cannot install configuration
profiles.
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

require_value() {
  local flag="$1"
  local value="${2:-}"

  if [[ -z "$value" ]]; then
    die "$flag requires a value"
  fi
}

if [[ $# -eq 0 ]]; then
  usage >&2
  exit 2
fi

MODE="$1"
shift

while [[ $# -gt 0 ]]; do
  case "$1" in
    --app-path)
      require_value "$1" "${2:-}"
      APP_PATH="$2"
      shift 2
      ;;
    --extension-path)
      require_value "$1" "${2:-}"
      EXTENSION_PATH="$2"
      shift 2
      ;;
    --profile-id)
      require_value "$1" "${2:-}"
      PROFILE_IDENTIFIER="$2"
      shift 2
      ;;
    --profile-output)
      require_value "$1" "${2:-}"
      PROFILE_OUTPUT="$2"
      shift 2
      ;;
    --log-dir)
      require_value "$1" "${2:-}"
      LOG_DIR="$2"
      shift 2
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      die "unknown argument: $1"
      ;;
  esac
done

case "$MODE" in
  apply | generate | verify | cleanup) ;;
  *)
    usage >&2
    die "mode must be apply, generate, verify, or cleanup"
    ;;
esac

mkdir -p "$LOG_DIR"
if [[ -z "$PROFILE_OUTPUT" ]]; then
  PROFILE_OUTPUT="$LOG_DIR/tcfs-fileprovider-lab-system-policy.mobileconfig"
fi

sudo_run() {
  if sudo -n true 2>/dev/null; then
    sudo "$@"
  elif [[ -n "${TCFS_RUNNER_SUDO_PASSWORD:-}" ]]; then
    printf '%s\n' "$TCFS_RUNNER_SUDO_PASSWORD" | sudo -S -p '' "$@"
  elif [[ -r "$SUDO_PASSWORD_FILE" ]]; then
    # shellcheck disable=SC2024 # The redirection intentionally feeds sudo's password prompt.
    sudo -S -p '' "$@" < "$SUDO_PASSWORD_FILE"
  else
    return 125
  fi
}

bundle_exists() {
  local label="$1"
  local path="$2"

  if [[ ! -e "$path" ]]; then
    printf '%s not found: %s\n' "$label" "$path" > "$LOG_DIR/${label}-missing.txt"
    die "$label not found: $path"
  fi
}

designated_requirement() {
  local label="$1"
  local path="$2"
  local out="$LOG_DIR/${label}-designated-requirement.txt"
  local raw
  local requirement

  bundle_exists "$label" "$path"
  if ! raw="$(codesign -dr - "$path" 2>&1)"; then
    printf '%s\n' "$raw" > "$out"
    die "codesign could not read the designated requirement for $path"
  fi

  printf '%s\n' "$raw" > "$out"
  requirement="$(printf '%s\n' "$raw" | sed -n 's/^designated => //p' | head -n 1)"
  if [[ -z "$requirement" ]]; then
    die "codesign did not print a designated requirement for $path"
  fi

  printf '%s\n' "$requirement"
}

generate_profile() {
  local host_requirement
  local extension_requirement

  host_requirement="$(designated_requirement host-app "$APP_PATH")"
  extension_requirement="$(designated_requirement fileprovider-extension "$EXTENSION_PATH")"

  python3 - "$PROFILE_OUTPUT" "$PROFILE_IDENTIFIER" "$host_requirement" "$extension_requirement" <<'PY'
import plistlib
import sys
import uuid

output, profile_id, host_requirement, extension_requirement = sys.argv[1:5]

profile_uuid = str(uuid.uuid4()).upper()
host_uuid = str(uuid.uuid4()).upper()
extension_uuid = str(uuid.uuid4()).upper()

profile = {
    "PayloadContent": [
        {
            "PayloadType": "com.apple.systempolicy.rule",
            "PayloadIdentifier": f"{profile_id}.host",
            "PayloadUUID": host_uuid,
            "PayloadVersion": 1,
            "PayloadDisplayName": "TCFSProvider Host Lab Gatekeeper Rule",
            "Comment": "Allow TCFSProvider host app execution in the PZM FileProvider testing-mode lab.",
            "OperationType": "operation:execute",
            "Priority": 1000.0,
            "Requirement": host_requirement,
        },
        {
            "PayloadType": "com.apple.systempolicy.rule",
            "PayloadIdentifier": f"{profile_id}.extension",
            "PayloadUUID": extension_uuid,
            "PayloadVersion": 1,
            "PayloadDisplayName": "TCFS FileProvider Extension Lab Gatekeeper Rule",
            "Comment": "Allow TCFS FileProvider extension execution in the PZM FileProvider testing-mode lab.",
            "OperationType": "operation:execute",
            "Priority": 1000.0,
            "Requirement": extension_requirement,
        },
    ],
    "PayloadDescription": "Non-production TCFS FileProvider lab Gatekeeper execution rules for petting-zoo-mini.",
    "PayloadDisplayName": "TCFS FileProvider Lab Gatekeeper Rules",
    "PayloadIdentifier": profile_id,
    "PayloadOrganization": "Tinyland",
    "PayloadScope": "System",
    "PayloadType": "Configuration",
    "PayloadUUID": profile_uuid,
    "PayloadVersion": 1,
}

with open(output, "wb") as handle:
    plistlib.dump(profile, handle, sort_keys=False)
PY

  {
    printf 'mode=generate\n'
    printf 'profile_identifier=%s\n' "$PROFILE_IDENTIFIER"
    printf 'profile_output=%s\n' "$PROFILE_OUTPUT"
    printf 'app=%s\n' "$APP_PATH"
    printf 'extension=%s\n' "$EXTENSION_PATH"
  } > "$LOG_DIR/summary.txt"
}

show_profiles() {
  local out="$LOG_DIR/profiles-show.txt"
  local status=0

  if command -v profiles >/dev/null 2>&1; then
    if sudo_run profiles show -type configuration -all > "$out" 2>&1; then
      printf 'sudo profiles show -type configuration -all\n' > "$LOG_DIR/profiles-show-command.txt"
      return 0
    fi
    status=$?
    printf 'sudo profiles show exit=%s\n' "$status" > "$LOG_DIR/profiles-show-command.txt"
    profiles show -type configuration > "$out" 2>&1 || true
    printf 'profiles show -type configuration fallback\n' >> "$LOG_DIR/profiles-show-command.txt"
    return 0
  fi

  printf 'profiles command not found\n' > "$out"
  return 0
}

verify_profile_installed() {
  show_profiles

  if grep -Fq "$PROFILE_IDENTIFIER" "$LOG_DIR/profiles-show.txt"; then
    {
      printf 'mode=verify\n'
      printf 'profile_identifier=%s\n' "$PROFILE_IDENTIFIER"
      printf 'installed=true\n'
    } > "$LOG_DIR/verify-summary.txt"
    return 0
  fi

  {
    printf 'mode=verify\n'
    printf 'profile_identifier=%s\n' "$PROFILE_IDENTIFIER"
    printf 'installed=false\n'
    printf 'generated_profile=%s\n' "$PROFILE_OUTPUT"
  } > "$LOG_DIR/verify-summary.txt"

  printf 'TCFS lab SystemPolicyRule configuration profile is not installed.\n' >&2
  printf 'Generated desired profile: %s\n' "$PROFILE_OUTPUT" >&2
  printf 'Install it on petting-zoo-mini via System Settings > Privacy & Security > Profiles, then rerun this smoke.\n' >&2
  return 4
}

case "$MODE" in
  generate)
    generate_profile
    ;;
  verify)
    verify_profile_installed
    ;;
  apply)
    generate_profile
    verify_profile_installed
    ;;
  cleanup)
    {
      printf 'mode=cleanup\n'
      printf 'profile_identifier=%s\n' "$PROFILE_IDENTIFIER"
      printf 'note=configuration profile install/remove is manual or MDM-managed on macOS 15+\n'
    } > "$LOG_DIR/cleanup-summary.txt"
    ;;
esac
