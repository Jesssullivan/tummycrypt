#!/usr/bin/env bash
#
# Build the TCFS macOS .pkg from the release CLI tarball and FileProvider zip.
#
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/macos-build-pkg.sh --version <version> --cli-tar <path> --fileprovider-zip <path> --output <path> [options]

Options:
  --identifier <id>             Package identifier (default: io.tinyland.tcfs)
  --postinstall <path>          Postinstall script
                                (default: scripts/macos-pkg-postinstall.sh)
  --sign <identity>             Developer ID Installer identity for productsign
  --work-dir <path>             Working directory to use instead of mktemp
  --skip-structure-smoke        Do not run macOS package structure smoke
  -h, --help                    Show this help
EOF
}

fail() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION=""
CLI_TAR=""
FILEPROVIDER_ZIP=""
OUTPUT_PKG=""
IDENTIFIER="io.tinyland.tcfs"
POSTINSTALL="${REPO_ROOT}/scripts/macos-pkg-postinstall.sh"
SIGN_IDENTITY=""
WORK_DIR=""
SKIP_STRUCTURE_SMOKE=0

TAR_BIN="${TCFS_TAR:-tar}"
DITTO_BIN="${TCFS_DITTO:-/usr/bin/ditto}"
PKGBUILD_BIN="${TCFS_PKGBUILD:-pkgbuild}"
PRODUCTSIGN_BIN="${TCFS_PRODUCTSIGN:-productsign}"
UNAME_BIN="${TCFS_UNAME:-uname}"
PKG_STRUCTURE_SMOKE="${TCFS_PKG_STRUCTURE_SMOKE:-${REPO_ROOT}/scripts/macos-pkg-structure-smoke.sh}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)
      [[ $# -ge 2 ]] || fail "--version requires a value"
      VERSION="$2"
      shift 2
      ;;
    --cli-tar)
      [[ $# -ge 2 ]] || fail "--cli-tar requires a value"
      CLI_TAR="$2"
      shift 2
      ;;
    --fileprovider-zip)
      [[ $# -ge 2 ]] || fail "--fileprovider-zip requires a value"
      FILEPROVIDER_ZIP="$2"
      shift 2
      ;;
    --output)
      [[ $# -ge 2 ]] || fail "--output requires a value"
      OUTPUT_PKG="$2"
      shift 2
      ;;
    --identifier)
      [[ $# -ge 2 ]] || fail "--identifier requires a value"
      IDENTIFIER="$2"
      shift 2
      ;;
    --postinstall)
      [[ $# -ge 2 ]] || fail "--postinstall requires a value"
      POSTINSTALL="$2"
      shift 2
      ;;
    --sign)
      [[ $# -ge 2 ]] || fail "--sign requires a value"
      SIGN_IDENTITY="$2"
      shift 2
      ;;
    --work-dir)
      [[ $# -ge 2 ]] || fail "--work-dir requires a value"
      WORK_DIR="$2"
      shift 2
      ;;
    --skip-structure-smoke)
      SKIP_STRUCTURE_SMOKE=1
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

[[ -n "$VERSION" ]] || fail "set --version"
[[ -n "$CLI_TAR" ]] || fail "set --cli-tar"
[[ -n "$FILEPROVIDER_ZIP" ]] || fail "set --fileprovider-zip"
[[ -n "$OUTPUT_PKG" ]] || fail "set --output"
[[ -f "$CLI_TAR" ]] || fail "CLI tarball not found: $CLI_TAR"
[[ -f "$FILEPROVIDER_ZIP" ]] || fail "FileProvider zip not found: $FILEPROVIDER_ZIP"
[[ -f "$POSTINSTALL" ]] || fail "postinstall script not found: $POSTINSTALL"

if [[ "$("$UNAME_BIN" -s)" != "Darwin" ]]; then
  fail "scripts/macos-build-pkg.sh requires macOS packaging tools"
fi

cleanup_work_dir=0
if [[ -z "$WORK_DIR" ]]; then
  WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-macos-pkg.XXXXXX")"
  cleanup_work_dir=1
else
  mkdir -p "$WORK_DIR"
fi
if [[ "$cleanup_work_dir" == "1" ]]; then
  trap 'rm -rf "$WORK_DIR"' EXIT
fi

extract_dir="${WORK_DIR}/cli-extract"
pkg_root="${WORK_DIR}/pkg-root"
pkg_scripts="${WORK_DIR}/pkg-scripts"
unsigned_pkg="${WORK_DIR}/tcfs-${VERSION}-unsigned.pkg"

rm -rf "$extract_dir" "$pkg_root" "$pkg_scripts"
mkdir -p "$extract_dir" "$pkg_root/usr/local/bin" "$pkg_root/Applications" "$pkg_scripts"

"$TAR_BIN" xzf "$CLI_TAR" -C "$extract_dir"

cli_dir="$extract_dir/tcfs-${VERSION}-macos-aarch64"
if [[ ! -d "$cli_dir" ]]; then
  tcfs_path="$(find "$extract_dir" -type f -name tcfs -perm -111 -print -quit)"
  if [[ -n "$tcfs_path" ]]; then
    cli_dir="$(dirname "$tcfs_path")"
  fi
fi
[[ -n "$cli_dir" && -d "$cli_dir" ]] || fail "could not locate extracted CLI directory"

for required_bin in tcfs tcfsd; do
  [[ -f "$cli_dir/$required_bin" ]] ||
    fail "CLI tarball missing required binary: $required_bin"
  cp "$cli_dir/$required_bin" "$pkg_root/usr/local/bin/"
done

for optional_bin in tcfs-tui tcfs-mcp; do
  if [[ -f "$cli_dir/$optional_bin" ]]; then
    cp "$cli_dir/$optional_bin" "$pkg_root/usr/local/bin/"
  fi
done
chmod +x "$pkg_root/usr/local/bin/"*

"$DITTO_BIN" -x -k "$FILEPROVIDER_ZIP" "$pkg_root/Applications/"
[[ -d "$pkg_root/Applications/TCFSProvider.app" ]] ||
  fail "FileProvider zip did not extract TCFSProvider.app"
[[ -d "$pkg_root/Applications/TCFSProvider.app/Contents/Extensions/TCFSFileProvider.appex" ]] ||
  fail "FileProvider zip did not include TCFSFileProvider.appex"

install -m 755 "$POSTINSTALL" "$pkg_scripts/postinstall"

"$PKGBUILD_BIN" \
  --root "$pkg_root" \
  --scripts "$pkg_scripts" \
  --identifier "$IDENTIFIER" \
  --version "$VERSION" \
  --install-location / \
  "$unsigned_pkg"

mkdir -p "$(dirname "$OUTPUT_PKG")"
if [[ -n "$SIGN_IDENTITY" && "$SIGN_IDENTITY" != "-" ]]; then
  printf 'Signing .pkg with: %s\n' "$SIGN_IDENTITY"
  "$PRODUCTSIGN_BIN" --sign "$SIGN_IDENTITY" "$unsigned_pkg" "$OUTPUT_PKG"
else
  cp "$unsigned_pkg" "$OUTPUT_PKG"
fi

if [[ "$SKIP_STRUCTURE_SMOKE" != "1" ]]; then
  bash "$PKG_STRUCTURE_SMOKE" --pkg "$OUTPUT_PKG" --expected-postinstall "$POSTINSTALL"
fi

printf 'macOS package built: %s\n' "$OUTPUT_PKG"
