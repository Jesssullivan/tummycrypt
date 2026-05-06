#!/usr/bin/env bash
#
# Add or remove a non-production Gatekeeper assessment label for the TCFS
# FileProvider lab app installed on a registered development Mac.
#
# This is intentionally not a production distribution path. It exists so the
# petting-zoo-mini testing-mode lane can prove FileProvider lifecycle behavior
# when a Mac App Development-signed .pkg is blocked by AppleSystemPolicy before
# Swift entry.
#
set -euo pipefail

APP_PATH="${TCFS_FILEPROVIDER_APP_PATH:-/Applications/TCFSProvider.app}"
EXTENSION_PATH="${TCFS_FILEPROVIDER_EXTENSION_PATH:-${APP_PATH}/Contents/Extensions/TCFSFileProvider.appex}"
LABEL="${TCFS_FILEPROVIDER_LAB_GATEKEEPER_LABEL:-TCFSFileProviderLab}"
LOG_DIR="${LOG_DIR:-${RUNNER_TEMP:-/tmp}/tcfs-fileprovider-lab-gatekeeper}"
SUDO_PASSWORD_FILE="${TCFS_RUNNER_SUDO_PASSWORD_FILE:-$HOME/.config/sops-nix/secrets/become/password}"
MODE=""

usage() {
  cat <<'USAGE'
Usage: scripts/macos-fileprovider-lab-gatekeeper-override.sh <apply|cleanup> [options]

Options:
  --app-path <path>       Installed host app path (default: /Applications/TCFSProvider.app)
  --extension-path <path> Installed FileProvider extension path
  --label <label>         Gatekeeper assessment label (default: TCFSFileProviderLab)
  --log-dir <path>        Directory for before/after assessment logs
  -h, --help              Show this help

The apply mode requires sudo. It records spctl/syspolicy_check evidence before
and after adding a labeled assessment rule for the host app and extension.
cleanup removes every rule with the chosen label.
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
    --label)
      require_value "$1" "${2:-}"
      LABEL="$2"
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
  apply | cleanup) ;;
  *)
    usage >&2
    die "mode must be apply or cleanup"
    ;;
esac

mkdir -p "$LOG_DIR"

SPCTL_BIN="$(command -v spctl || true)"
if [[ -z "$SPCTL_BIN" && -x /usr/sbin/spctl ]]; then
  SPCTL_BIN=/usr/sbin/spctl
fi
[[ -n "$SPCTL_BIN" ]] || die "spctl not found"

sudo_run() {
  if sudo -n true 2>/dev/null; then
    sudo "$@"
  elif [[ -n "${TCFS_RUNNER_SUDO_PASSWORD:-}" ]]; then
    printf '%s\n' "$TCFS_RUNNER_SUDO_PASSWORD" | sudo -S -p '' "$@"
  elif [[ -r "$SUDO_PASSWORD_FILE" ]]; then
    # shellcheck disable=SC2024 # The redirection intentionally feeds sudo's password prompt.
    sudo -S -p '' "$@" < "$SUDO_PASSWORD_FILE"
  else
    die "sudo requires a password; set TCFS_RUNNER_SUDO_PASSWORD or provide $SUDO_PASSWORD_FILE on the runner"
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

assess_bundle() {
  local label="$1"
  local phase="$2"
  local path="$3"
  local out="$LOG_DIR/${label}-${phase}-spctl-execute.txt"
  local status=0

  "$SPCTL_BIN" --assess --type execute --verbose=4 "$path" > "$out" 2>&1 || status=$?
  printf 'exit=%s\n' "$status" >> "$out"

  if command -v syspolicy_check >/dev/null 2>&1; then
    syspolicy_check distribution "$path" \
      > "$LOG_DIR/${label}-${phase}-syspolicy-distribution.txt" 2>&1 || true
    syspolicy_check notary-submission "$path" \
      > "$LOG_DIR/${label}-${phase}-syspolicy-notary-submission.txt" 2>&1 || true
  else
    printf 'syspolicy_check not found\n' \
      > "$LOG_DIR/${label}-${phase}-syspolicy-distribution.txt"
    printf 'syspolicy_check not found\n' \
      > "$LOG_DIR/${label}-${phase}-syspolicy-notary-submission.txt"
  fi
}

case "$MODE" in
  apply)
    bundle_exists host-app "$APP_PATH"
    bundle_exists fileprovider-extension "$EXTENSION_PATH"

    {
      printf 'mode=apply\n'
      printf 'label=%s\n' "$LABEL"
      printf 'app=%s\n' "$APP_PATH"
      printf 'extension=%s\n' "$EXTENSION_PATH"
      printf 'spctl=%s\n' "$SPCTL_BIN"
    } > "$LOG_DIR/summary.txt"

    assess_bundle host-app before "$APP_PATH"
    assess_bundle fileprovider-extension before "$EXTENSION_PATH"

    sudo_run "$SPCTL_BIN" --add --label "$LABEL" "$APP_PATH" \
      > "$LOG_DIR/spctl-add-host-app.txt" 2>&1
    sudo_run "$SPCTL_BIN" --add --label "$LABEL" "$EXTENSION_PATH" \
      > "$LOG_DIR/spctl-add-fileprovider-extension.txt" 2>&1
    sudo_run "$SPCTL_BIN" --enable --label "$LABEL" \
      > "$LOG_DIR/spctl-enable-label.txt" 2>&1

    assess_bundle host-app after "$APP_PATH"
    assess_bundle fileprovider-extension after "$EXTENSION_PATH"
    ;;
  cleanup)
    {
      printf 'mode=cleanup\n'
      printf 'label=%s\n' "$LABEL"
      printf 'spctl=%s\n' "$SPCTL_BIN"
    } > "$LOG_DIR/cleanup-summary.txt"
    sudo_run "$SPCTL_BIN" --remove --label "$LABEL" \
      > "$LOG_DIR/spctl-remove-label.txt" 2>&1 || true
    ;;
esac
