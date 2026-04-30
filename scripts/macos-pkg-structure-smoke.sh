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
PKGUTIL_BIN="${TCFS_PKGUTIL:-pkgutil}"
UNAME_BIN="${TCFS_UNAME:-uname}"

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

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-pkg-structure.XXXXXX")"
trap 'rm -rf "$tmp_dir"' EXIT

payload_files="${tmp_dir}/payload-files.txt"
normalized_payload="${tmp_dir}/payload-files-normalized.txt"
expanded_dir="${tmp_dir}/expanded"

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

if [[ "$REQUIRE_SIGNATURE" == "1" ]]; then
  "$PKGUTIL_BIN" --check-signature "$PKG_PATH" >/dev/null
fi

printf 'package: %s\n' "$PKG_PATH"
printf 'payload: usr/local/bin/tcfs present\n'
printf 'payload: usr/local/bin/tcfsd present\n'
printf 'payload: TCFSProvider.app present\n'
printf 'payload: TCFSFileProvider.appex present\n'
printf 'postinstall: %s\n' "$postinstall_status"
printf 'macOS package structure smoke passed\n'
