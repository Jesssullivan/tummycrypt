#!/usr/bin/env bash
#
# Non-installing structure smoke for a TCFS macOS .pkg artifact.
#
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/macos-pkg-structure-smoke.sh --pkg <path> [options]

Checks that a macOS .pkg artifact contains the expected TCFS payload and
postinstall script without installing it.

Options:
  --pkg <path>                   Package artifact to inspect
  --expected-postinstall <path>  Script expected to match package postinstall
                                 (default: scripts/macos-pkg-postinstall.sh)
  --allow-postinstall-mismatch   Warn instead of failing if the package
                                 postinstall differs from --expected-postinstall
  --require-signature            Require pkgutil --check-signature to pass
  --require-gatekeeper-install   Require spctl install assessment to pass
  --require-stapled-ticket       Require xcrun stapler validate to pass
  -h, --help                     Show this help
EOF
}

fail() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PKG_PATH=""
EXPECTED_POSTINSTALL="${REPO_ROOT}/scripts/macos-pkg-postinstall.sh"
ALLOW_POSTINSTALL_MISMATCH=0
REQUIRE_SIGNATURE=0
REQUIRE_GATEKEEPER_INSTALL=0
REQUIRE_STAPLED_TICKET=0
PKGUTIL_BIN="${TCFS_PKGUTIL:-}"
SPCTL_BIN="${TCFS_SPCTL:-}"
XCRUN_BIN="${TCFS_XCRUN:-}"
UNAME_BIN="${TCFS_UNAME:-uname}"
PYTHON_BIN="${TCFS_PYTHON:-python3}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --pkg)
      [[ $# -ge 2 ]] || fail "--pkg requires a value"
      PKG_PATH="$2"
      shift 2
      ;;
    --expected-postinstall)
      [[ $# -ge 2 ]] || fail "--expected-postinstall requires a value"
      EXPECTED_POSTINSTALL="$2"
      shift 2
      ;;
    --allow-postinstall-mismatch)
      ALLOW_POSTINSTALL_MISMATCH=1
      shift
      ;;
    --require-signature)
      REQUIRE_SIGNATURE=1
      shift
      ;;
    --require-gatekeeper-install)
      REQUIRE_GATEKEEPER_INSTALL=1
      shift
      ;;
    --require-stapled-ticket)
      REQUIRE_STAPLED_TICKET=1
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

[[ -n "$PKG_PATH" ]] || fail "set --pkg /path/to/tcfs.pkg"
[[ -f "$PKG_PATH" ]] || fail "package not found: $PKG_PATH"
[[ -f "$EXPECTED_POSTINSTALL" ]] || fail "expected postinstall not found: $EXPECTED_POSTINSTALL"

if [[ "$("$UNAME_BIN" -s)" != "Darwin" ]]; then
  fail "scripts/macos-pkg-structure-smoke.sh only inspects packages with macOS pkgutil"
fi

if [[ -z "$PKGUTIL_BIN" ]]; then
  if command -v pkgutil >/dev/null 2>&1; then
    PKGUTIL_BIN="$(command -v pkgutil)"
  elif [[ -x /usr/sbin/pkgutil ]]; then
    PKGUTIL_BIN="/usr/sbin/pkgutil"
  else
    PKGUTIL_BIN="pkgutil"
  fi
fi

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-pkg-structure.XXXXXX")"
trap 'rm -rf "$tmp_dir"' EXIT

payload_files="${tmp_dir}/payload-files.txt"
normalized_payload="${tmp_dir}/payload-files-normalized.txt"
expanded_dir="${tmp_dir}/expanded"
expanded_full_dir="${tmp_dir}/expanded-full"

"$PKGUTIL_BIN" --payload-files "$PKG_PATH" >"$payload_files"
sed -E 's#^\./##' "$payload_files" >"$normalized_payload"

payload_contains_exact() {
  local expected="$1"

  grep -Fxq "$expected" "$normalized_payload"
}

payload_contains_prefix() {
  local expected_prefix="$1"

  grep -Eq "^${expected_prefix}(/|$)" "$normalized_payload"
}

payload_contains_exact "usr/local/bin/tcfs" ||
  fail "package payload missing usr/local/bin/tcfs"
payload_contains_exact "usr/local/bin/tcfsd" ||
  fail "package payload missing usr/local/bin/tcfsd"
payload_contains_prefix "Applications/TCFSProvider.app" ||
  fail "package payload missing Applications/TCFSProvider.app"
payload_contains_prefix "Applications/TCFSProvider.app/Contents/Extensions/TCFSFileProvider.appex" ||
  fail "package payload missing TCFSFileProvider.appex"

"$PKGUTIL_BIN" --expand "$PKG_PATH" "$expanded_dir"
postinstall="$(find "$expanded_dir" -path '*/Scripts/postinstall' -type f | head -1)"
[[ -n "$postinstall" ]] || fail "package expansion did not contain Scripts/postinstall"

postinstall_status="matches $EXPECTED_POSTINSTALL"
if ! cmp -s "$EXPECTED_POSTINSTALL" "$postinstall"; then
  if [[ "$ALLOW_POSTINSTALL_MISMATCH" == "1" ]]; then
    postinstall_status="differs from $EXPECTED_POSTINSTALL"
    printf 'warning: package postinstall does not match %s\n' "$EXPECTED_POSTINSTALL" >&2
  else
    fail "package postinstall does not match $EXPECTED_POSTINSTALL"
  fi
fi

"$PKGUTIL_BIN" --expand-full "$PKG_PATH" "$expanded_full_dir"
fileprovider_info_plist="$(
  find "$expanded_full_dir" \
    -path '*/Applications/TCFSProvider.app/Contents/Extensions/TCFSFileProvider.appex/Contents/Info.plist' \
    -type f \
    | head -1
)"
[[ -n "$fileprovider_info_plist" ]] ||
  fail "package full expansion did not contain TCFSFileProvider.appex/Contents/Info.plist"

"$PYTHON_BIN" - "$fileprovider_info_plist" <<'PY'
import plistlib
import sys

path = sys.argv[1]
with open(path, "rb") as handle:
    plist = plistlib.load(handle)

extension = plist.get("NSExtension")
if not isinstance(extension, dict):
    raise SystemExit("FileProvider Info.plist missing NSExtension dictionary")

point = extension.get("NSExtensionPointIdentifier")
if point != "com.apple.fileprovider-nonui":
    raise SystemExit(f"unexpected FileProvider extension point: {point!r}")

if extension.get("NSExtensionFileProviderSupportsEnumeration") is not True:
    raise SystemExit("FileProvider extension does not declare enumeration support")

if extension.get("NSExtensionFileProviderSupportsDatalessFolders") is not True:
    raise SystemExit("FileProvider extension does not declare dataless-folder support")

decorations = extension.get("NSFileProviderDecorations")
if not isinstance(decorations, list):
    raise SystemExit("FileProvider extension missing NSFileProviderDecorations")

decoration_ids = {
    item.get("Identifier")
    for item in decorations
    if isinstance(item, dict)
}
required_decoration_ids = {
    "io.tinyland.tcfs.fileprovider.decoration.conflict",
    "io.tinyland.tcfs.fileprovider.decoration.locked",
    "io.tinyland.tcfs.fileprovider.decoration.pinned",
    "io.tinyland.tcfs.fileprovider.decoration.excluded",
}
missing_decorations = sorted(required_decoration_ids - decoration_ids)
if missing_decorations:
    raise SystemExit(
        "missing FileProvider decorations: " + ", ".join(missing_decorations)
    )

actions = extension.get("NSExtensionFileProviderActions")
if not isinstance(actions, list):
    raise SystemExit("FileProvider extension missing NSExtensionFileProviderActions")

action_ids = {
    item.get("NSExtensionFileProviderActionIdentifier")
    for item in actions
    if isinstance(item, dict)
}
required_action_ids = {
    "io.tinyland.tcfs.action.unsync",
    "io.tinyland.tcfs.action.pin",
}
missing_actions = sorted(required_action_ids - action_ids)
if missing_actions:
    raise SystemExit("missing FileProvider actions: " + ", ".join(missing_actions))
PY

if [[ "$REQUIRE_SIGNATURE" == "1" ]]; then
  "$PKGUTIL_BIN" --check-signature "$PKG_PATH" >/dev/null
fi

if [[ "$REQUIRE_GATEKEEPER_INSTALL" == "1" ]]; then
  if [[ -z "$SPCTL_BIN" ]]; then
    if command -v spctl >/dev/null 2>&1; then
      SPCTL_BIN="$(command -v spctl)"
    elif [[ -x /usr/sbin/spctl ]]; then
      SPCTL_BIN="/usr/sbin/spctl"
    else
      SPCTL_BIN="spctl"
    fi
  fi
  "$SPCTL_BIN" --assess --type install --verbose=4 "$PKG_PATH" >/dev/null
fi

if [[ "$REQUIRE_STAPLED_TICKET" == "1" ]]; then
  if [[ -z "$XCRUN_BIN" ]]; then
    if command -v xcrun >/dev/null 2>&1; then
      XCRUN_BIN="$(command -v xcrun)"
    elif [[ -x /usr/bin/xcrun ]]; then
      XCRUN_BIN="/usr/bin/xcrun"
    else
      XCRUN_BIN="xcrun"
    fi
  fi
  "$XCRUN_BIN" stapler validate -v "$PKG_PATH" >/dev/null
fi

printf 'package: %s\n' "$PKG_PATH"
printf 'payload: usr/local/bin/tcfs present\n'
printf 'payload: usr/local/bin/tcfsd present\n'
printf 'payload: TCFSProvider.app present\n'
printf 'payload: TCFSFileProvider.appex present\n'
printf 'payload: FileProvider status decorations/actions present\n'
printf 'postinstall: %s\n' "$postinstall_status"
if [[ "$REQUIRE_SIGNATURE" == "1" ]]; then
  printf 'signature: valid\n'
fi
if [[ "$REQUIRE_GATEKEEPER_INSTALL" == "1" ]]; then
  printf 'gatekeeper: install assessment passed\n'
fi
if [[ "$REQUIRE_STAPLED_TICKET" == "1" ]]; then
  printf 'notarization: stapled ticket valid\n'
fi
printf 'macOS package structure smoke passed\n'
