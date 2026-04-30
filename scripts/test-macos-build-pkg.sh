#!/usr/bin/env bash
#
# Regression tests for macos-build-pkg.sh using fake macOS packaging tools.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="${REPO_ROOT}/scripts/macos-build-pkg.sh"
POSTINSTALL="${REPO_ROOT}/scripts/macos-pkg-postinstall.sh"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-build-pkg-test.XXXXXX")"
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

write_cli_tar() {
  local tar_path="$1"
  local version="$2"
  local include_daemon="$3"
  local root="${TMPDIR}/cli-${version}"
  local cli_dir="${root}/tcfs-${version}-macos-aarch64"

  rm -rf "$root"
  mkdir -p "$cli_dir"
  printf '#!/bin/sh\necho tcfs\n' >"$cli_dir/tcfs"
  chmod +x "$cli_dir/tcfs"
  if [[ "$include_daemon" == "1" ]]; then
    printf '#!/bin/sh\necho tcfsd\n' >"$cli_dir/tcfsd"
    chmod +x "$cli_dir/tcfsd"
  fi
  printf '#!/bin/sh\necho tcfs-tui\n' >"$cli_dir/tcfs-tui"
  chmod +x "$cli_dir/tcfs-tui"

  tar czf "$tar_path" -C "$root" "tcfs-${version}-macos-aarch64"
}

FAKE_BIN="${TMPDIR}/fake-bin"
mkdir -p "$FAKE_BIN"

cat >"$FAKE_BIN/uname" <<'EOF'
#!/usr/bin/env bash
printf 'Darwin\n'
EOF
cat >"$FAKE_BIN/ditto" <<'EOF'
#!/usr/bin/env bash
dest="$4"
if [[ "${TCFS_FAKE_DITTO_NO_APP:-0}" == "1" ]]; then
  mkdir -p "$dest"
  exit 0
fi
mkdir -p "$dest/TCFSProvider.app/Contents/Extensions/TCFSFileProvider.appex"
EOF
cat >"$FAKE_BIN/pkgbuild" <<'EOF'
#!/usr/bin/env bash
root=""
scripts=""
out=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --root)
      root="$2"
      shift 2
      ;;
    --scripts)
      scripts="$2"
      shift 2
      ;;
    --identifier|--version|--install-location)
      shift 2
      ;;
    *)
      out="$1"
      shift
      ;;
  esac
done

[[ -f "$root/usr/local/bin/tcfs" ]] || exit 11
[[ -f "$root/usr/local/bin/tcfsd" ]] || exit 12
[[ -d "$root/Applications/TCFSProvider.app/Contents/Extensions/TCFSFileProvider.appex" ]] || exit 13
[[ -f "$scripts/postinstall" ]] || exit 14
printf 'unsigned package\n' >"$out"
printf 'pkgbuild root=%s scripts=%s out=%s\n' "$root" "$scripts" "$out" >>"$TCFS_FAKE_BUILD_PKG_LOG"
EOF
cat >"$FAKE_BIN/productsign" <<'EOF'
#!/usr/bin/env bash
identity=""
unsigned=""
signed=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --sign)
      identity="$2"
      shift 2
      ;;
    *)
      if [[ -z "$unsigned" ]]; then
        unsigned="$1"
      else
        signed="$1"
      fi
      shift
      ;;
  esac
done
cp "$unsigned" "$signed"
printf 'productsign identity=%s unsigned=%s signed=%s\n' "$identity" "$unsigned" "$signed" >>"$TCFS_FAKE_BUILD_PKG_LOG"
EOF
cat >"$FAKE_BIN/structure-smoke" <<'EOF'
#!/usr/bin/env bash
pkg=""
postinstall=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --pkg)
      pkg="$2"
      shift 2
      ;;
    --expected-postinstall)
      postinstall="$2"
      shift 2
      ;;
    *)
      shift
      ;;
  esac
done
[[ -f "$pkg" ]] || exit 21
[[ -f "$postinstall" ]] || exit 22
printf 'structure-smoke pkg=%s postinstall=%s\n' "$pkg" "$postinstall" >>"$TCFS_FAKE_BUILD_PKG_LOG"
EOF
chmod +x "$FAKE_BIN"/*

VERSION="9.8.7"
CLI_TAR="${TMPDIR}/tcfs-${VERSION}-macos-aarch64.tar.gz"
FP_ZIP="${TMPDIR}/TCFSProvider-${VERSION}-macos-aarch64.zip"
OUT_PKG="${TMPDIR}/tcfs-${VERSION}-macos-aarch64.pkg"
LOG="${TMPDIR}/build-pkg.log"
write_cli_tar "$CLI_TAR" "$VERSION" 1
: >"$FP_ZIP"

TCFS_UNAME="$FAKE_BIN/uname" \
TCFS_DITTO="$FAKE_BIN/ditto" \
TCFS_PKGBUILD="$FAKE_BIN/pkgbuild" \
TCFS_PRODUCTSIGN="$FAKE_BIN/productsign" \
TCFS_PKG_STRUCTURE_SMOKE="$FAKE_BIN/structure-smoke" \
TCFS_FAKE_BUILD_PKG_LOG="$LOG" \
bash "$SCRIPT" \
  --version "$VERSION" \
  --cli-tar "$CLI_TAR" \
  --fileprovider-zip "$FP_ZIP" \
  --output "$OUT_PKG" \
  >"${TMPDIR}/unsigned.out"

[[ -f "$OUT_PKG" ]] || {
  printf 'expected unsigned package output\n' >&2
  exit 1
}
assert_contains "$LOG" "pkgbuild root="
assert_contains "$LOG" "structure-smoke pkg=$OUT_PKG postinstall=$POSTINSTALL"
assert_contains "${TMPDIR}/unsigned.out" "macOS package built: $OUT_PKG"

SIGNED_PKG="${TMPDIR}/tcfs-${VERSION}-macos-aarch64-signed.pkg"
TCFS_UNAME="$FAKE_BIN/uname" \
TCFS_DITTO="$FAKE_BIN/ditto" \
TCFS_PKGBUILD="$FAKE_BIN/pkgbuild" \
TCFS_PRODUCTSIGN="$FAKE_BIN/productsign" \
TCFS_PKG_STRUCTURE_SMOKE="$FAKE_BIN/structure-smoke" \
TCFS_FAKE_BUILD_PKG_LOG="$LOG" \
bash "$SCRIPT" \
  --version "$VERSION" \
  --cli-tar "$CLI_TAR" \
  --fileprovider-zip "$FP_ZIP" \
  --output "$SIGNED_PKG" \
  --sign "Developer ID Installer: Test (TEAMID)" \
  >"${TMPDIR}/signed.out"
assert_contains "$LOG" "productsign identity=Developer ID Installer: Test (TEAMID)"
assert_contains "$LOG" "structure-smoke pkg=$SIGNED_PKG postinstall=$POSTINSTALL"

NO_DAEMON_TAR="${TMPDIR}/missing-daemon.tar.gz"
write_cli_tar "$NO_DAEMON_TAR" "$VERSION" 0
assert_fails_contains \
  "CLI tarball missing required binary: tcfsd" \
  env TCFS_UNAME="$FAKE_BIN/uname" \
    TCFS_DITTO="$FAKE_BIN/ditto" \
    TCFS_PKGBUILD="$FAKE_BIN/pkgbuild" \
    TCFS_PRODUCTSIGN="$FAKE_BIN/productsign" \
    TCFS_PKG_STRUCTURE_SMOKE="$FAKE_BIN/structure-smoke" \
    TCFS_FAKE_BUILD_PKG_LOG="$LOG" \
    bash "$SCRIPT" \
      --version "$VERSION" \
      --cli-tar "$NO_DAEMON_TAR" \
      --fileprovider-zip "$FP_ZIP" \
      --output "${TMPDIR}/missing-daemon.pkg"

assert_fails_contains \
  "FileProvider zip did not extract TCFSProvider.app" \
  env TCFS_UNAME="$FAKE_BIN/uname" \
    TCFS_DITTO="$FAKE_BIN/ditto" \
    TCFS_PKGBUILD="$FAKE_BIN/pkgbuild" \
    TCFS_PRODUCTSIGN="$FAKE_BIN/productsign" \
    TCFS_PKG_STRUCTURE_SMOKE="$FAKE_BIN/structure-smoke" \
    TCFS_FAKE_DITTO_NO_APP=1 \
    TCFS_FAKE_BUILD_PKG_LOG="$LOG" \
    bash "$SCRIPT" \
      --version "$VERSION" \
      --cli-tar "$CLI_TAR" \
      --fileprovider-zip "$FP_ZIP" \
      --output "${TMPDIR}/missing-app.pkg"

assert_fails_contains \
  "requires macOS packaging tools" \
  env TCFS_UNAME=/bin/echo bash "$SCRIPT" \
    --version "$VERSION" \
    --cli-tar "$CLI_TAR" \
    --fileprovider-zip "$FP_ZIP" \
    --output "${TMPDIR}/linux.pkg"

printf 'macOS build package tests passed\n'
