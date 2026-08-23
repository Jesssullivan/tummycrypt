#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/installed-runtime-acceptance.sh --platform linux|macos|ios [options]

Linux/macOS:
  --tcfs-bin PATH       Installed tcfs executable (must be outside this checkout)
  --root ID             Reconcile-capable registered root
  --execute             Execute the exact dry-run plan, then require convergence

macOS/iOS:
  --app-bundle PATH     Installed TCFS .app bundle
  --provider PATH       Installed FileProvider .appex bundle

Dry-run is the default. The runner never deletes and named-root execution is
bound to the freshly emitted plan SHA-256.
EOF
}

platform=""
tcfs_bin=""
root_id=""
app_bundle=""
provider_bundle=""
execute=0

while (($#)); do
  case "$1" in
    --platform) platform=${2:?}; shift 2 ;;
    --tcfs-bin) tcfs_bin=${2:?}; shift 2 ;;
    --root) root_id=${2:?}; shift 2 ;;
    --app-bundle) app_bundle=${2:?}; shift 2 ;;
    --provider) provider_bundle=${2:?}; shift 2 ;;
    --execute) execute=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

case "$platform" in
  linux|macos|ios) ;;
  *) echo "--platform must be linux, macos, or ios" >&2; exit 2 ;;
esac

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)

require_absolute() {
  case "$2" in
    /*) ;;
    *) echo "$1 must be absolute: $2" >&2; exit 2 ;;
  esac
}

reject_checkout_path() {
  local label=$1 path=$2 probe=$2
  [[ -d "$probe" ]] || probe=$(dirname "$probe")
  if env -u GIT_DIR -u GIT_WORK_TREE -u GIT_INDEX_FILE \
    -u GIT_OBJECT_DIRECTORY -u GIT_ALTERNATE_OBJECT_DIRECTORIES \
    GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null \
    "${GIT_BIN:-git}" -C "$probe" rev-parse --is-inside-work-tree 2>/dev/null \
      | grep -qx true; then
    echo "$label must be an installed runtime outside every Git checkout: $path" >&2
    exit 1
  fi
}

verify_apple_bundle() {
  local label=$1 bundle=$2 expected_type=$3
  require_absolute "$label" "$bundle"
  [[ -d "$bundle" && ! -L "$bundle" ]] || { echo "$label is missing or symlinked: $bundle" >&2; exit 1; }
  [[ -f "$bundle/Info.plist" && ! -L "$bundle/Info.plist" ]] || { echo "$label lacks a real Info.plist" >&2; exit 1; }
  reject_checkout_path "$label" "$bundle"
  local package_type
  package_type=$(${PLUTIL_BIN:-/usr/bin/plutil} -extract CFBundlePackageType raw "$bundle/Info.plist")
  [[ "$package_type" == "$expected_type" ]] || {
    echo "$label has package type $package_type, expected $expected_type" >&2
    exit 1
  }
  ${CODESIGN_BIN:-/usr/bin/codesign} --verify --strict "$bundle"
}

plan_sha=""
if [[ "$platform" == "linux" || "$platform" == "macos" ]]; then
  [[ -n "$tcfs_bin" && -n "$root_id" ]] || {
    echo "--tcfs-bin and --root are required for $platform" >&2
    exit 2
  }
  require_absolute "--tcfs-bin" "$tcfs_bin"
  [[ -x "$tcfs_bin" && ! -d "$tcfs_bin" ]] || {
    echo "tcfs executable is unavailable: $tcfs_bin" >&2
    exit 1
  }
  tcfs_bin=$(${PYTHON_BIN:-python3} -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$tcfs_bin")
  case "$tcfs_bin" in
    "$repo_root"/*) echo "tcfs must be an installed runtime outside the checkout" >&2; exit 1 ;;
  esac
  reject_checkout_path "--tcfs-bin" "$tcfs_bin"

  "$tcfs_bin" --version >/dev/null
  root_json=$("$tcfs_bin" roots status "$root_id" --json)
  printf '%s' "$root_json" | ${PYTHON_BIN:-python3} -c '
import json, sys
value=json.load(sys.stdin)
if value.get("availability") != "ready": raise SystemExit("registered root is not ready")
if value.get("reconcile_support") != "plan-and-execute": raise SystemExit("registered root is not reconcile-capable")
'

  dry_run=$("$tcfs_bin" reconcile --root "$root_id")
  printf '%s\n' "$dry_run" | ${PYTHON_BIN:-python3} -c '
import re, sys
text=sys.stdin.read()
match=re.search(r"Plan: (\d+) push, (\d+) pull, (\d+) create-dir, (\d+) delete-local, (\d+) delete-remote, (\d+) conflict", text)
if not match: raise SystemExit("reconcile plan summary is missing")
if int(match.group(4)) or int(match.group(5)): raise SystemExit("registered-root plan attempted deletion")
'
  plan_sha=$(printf '%s\n' "$dry_run" | sed -n 's/^Plan SHA-256: \([0-9a-f]\{64\}\)$/\1/p')
  [[ ${#plan_sha} -eq 64 ]] || { echo "reconcile did not emit one plan SHA-256" >&2; exit 1; }

  if ((execute)); then
    "$tcfs_bin" reconcile --root "$root_id" --execute --expect-plan "$plan_sha"
    settled=$("$tcfs_bin" reconcile --root "$root_id")
    printf '%s\n' "$settled" | ${PYTHON_BIN:-python3} -c '
import re, sys
text=sys.stdin.read()
match=re.search(r"Plan: (\d+) push, (\d+) pull, (\d+) create-dir, (\d+) delete-local, (\d+) delete-remote, (\d+) conflict", text)
if not match: raise SystemExit("settled reconcile summary is missing")
if any(int(value) for value in match.groups()): raise SystemExit("installed runtime did not converge")
'
  fi
fi

if [[ "$platform" == "macos" ]]; then
  [[ -n "$app_bundle" && -n "$provider_bundle" ]] || {
    echo "--app-bundle and --provider are required for macos" >&2
    exit 2
  }
  verify_apple_bundle "--app-bundle" "$app_bundle" APPL
  verify_apple_bundle "--provider" "$provider_bundle" XPC!
elif [[ "$platform" == "ios" ]]; then
  [[ -n "$app_bundle" && -n "$provider_bundle" ]] || {
    echo "--app-bundle and --provider are required for ios" >&2
    exit 2
  }
  verify_apple_bundle "--app-bundle" "$app_bundle" APPL
  verify_apple_bundle "--provider" "$provider_bundle" XPC!
  extension_point=$(${PLUTIL_BIN:-/usr/bin/plutil} -extract NSExtension.NSExtensionPointIdentifier raw "$provider_bundle/Info.plist")
  [[ "$extension_point" == com.apple.fileprovider-nonui ]] || {
    echo "iOS provider has unexpected extension point: $extension_point" >&2
    exit 1
  }
fi

printf 'installed-runtime-acceptance: PASS platform=%s executed=%s plan_sha256=%s\n' \
  "$platform" "$execute" "${plan_sha:-n/a}"
