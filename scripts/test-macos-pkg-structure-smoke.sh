#!/usr/bin/env bash
#
# Regression tests for macos-pkg-structure-smoke.sh using fake pkgutil output.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="${REPO_ROOT}/scripts/macos-pkg-structure-smoke.sh"
POSTINSTALL="${REPO_ROOT}/scripts/macos-pkg-postinstall.sh"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-pkg-structure-test.XXXXXX")"
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
mkdir -p "$FAKE_BIN"

cat >"$FAKE_BIN/uname" <<'EOF'
#!/usr/bin/env bash
printf 'Darwin\n'
EOF
cat >"$FAKE_BIN/pkgutil" <<'EOF'
#!/usr/bin/env bash
case "${1:-}" in
  --payload-files)
    cat "$2.payload"
    ;;
  --expand)
    mkdir -p "$3/Scripts"
    cp "$2.postinstall" "$3/Scripts/postinstall"
    ;;
  --check-signature)
    exit "${TCFS_FAKE_SIGNATURE_STATUS:-0}"
    ;;
  *)
    printf 'unexpected pkgutil invocation:' >&2
    printf ' %q' "$@" >&2
    printf '\n' >&2
    exit 1
    ;;
esac
EOF
chmod +x "$FAKE_BIN"/*

write_pkg_fixture() {
  local pkg="$1"
  local payload="$2"
  local postinstall="$3"

  : >"$pkg"
  printf '%s\n' "$payload" >"$pkg.payload"
  cp "$postinstall" "$pkg.postinstall"
}

GOOD_PAYLOAD='.
./usr
./usr/local
./usr/local/bin
./usr/local/bin/tcfs
./usr/local/bin/tcfsd
./Applications
./Applications/TCFSProvider.app
./Applications/TCFSProvider.app/Contents
./Applications/TCFSProvider.app/Contents/Extensions
./Applications/TCFSProvider.app/Contents/Extensions/TCFSFileProvider.appex'

GOOD_PKG="${TMPDIR}/tcfs-good.pkg"
write_pkg_fixture "$GOOD_PKG" "$GOOD_PAYLOAD" "$POSTINSTALL"

GOOD_OUT="${TMPDIR}/good.out"
PATH="$FAKE_BIN:$PATH" bash "$SCRIPT" --pkg "$GOOD_PKG" >"$GOOD_OUT"
assert_contains "$GOOD_OUT" "payload: usr/local/bin/tcfs present"
assert_contains "$GOOD_OUT" "payload: usr/local/bin/tcfsd present"
assert_contains "$GOOD_OUT" "payload: TCFSProvider.app present"
assert_contains "$GOOD_OUT" "payload: TCFSFileProvider.appex present"
assert_contains "$GOOD_OUT" "postinstall: matches $POSTINSTALL"
assert_contains "$GOOD_OUT" "macOS package structure smoke passed"

SIGNED_OUT="${TMPDIR}/signed.out"
PATH="$FAKE_BIN:$PATH" bash "$SCRIPT" --pkg "$GOOD_PKG" --require-signature >"$SIGNED_OUT"
assert_contains "$SIGNED_OUT" "macOS package structure smoke passed"

MISSING_DAEMON_PAYLOAD='.
./usr/local/bin/tcfs
./Applications/TCFSProvider.app
./Applications/TCFSProvider.app/Contents/Extensions/TCFSFileProvider.appex'
MISSING_DAEMON_PKG="${TMPDIR}/missing-daemon.pkg"
write_pkg_fixture "$MISSING_DAEMON_PKG" "$MISSING_DAEMON_PAYLOAD" "$POSTINSTALL"
assert_fails_contains \
  "package payload missing usr/local/bin/tcfsd" \
  env PATH="$FAKE_BIN:$PATH" bash "$SCRIPT" --pkg "$MISSING_DAEMON_PKG"

BAD_POSTINSTALL="${TMPDIR}/bad-postinstall.sh"
printf '#!/bin/sh\nexit 0\n' >"$BAD_POSTINSTALL"
BAD_POSTINSTALL_PKG="${TMPDIR}/bad-postinstall.pkg"
write_pkg_fixture "$BAD_POSTINSTALL_PKG" "$GOOD_PAYLOAD" "$BAD_POSTINSTALL"
assert_fails_contains \
  "package postinstall does not match" \
  env PATH="$FAKE_BIN:$PATH" bash "$SCRIPT" --pkg "$BAD_POSTINSTALL_PKG"

ALLOW_MISMATCH_OUT="${TMPDIR}/allow-mismatch.out"
ALLOW_MISMATCH_ERR="${TMPDIR}/allow-mismatch.err"
PATH="$FAKE_BIN:$PATH" \
  bash "$SCRIPT" --pkg "$BAD_POSTINSTALL_PKG" --allow-postinstall-mismatch \
  >"$ALLOW_MISMATCH_OUT" 2>"$ALLOW_MISMATCH_ERR"
assert_contains "$ALLOW_MISMATCH_OUT" "postinstall: differs from $POSTINSTALL"
assert_contains "$ALLOW_MISMATCH_OUT" "macOS package structure smoke passed"
assert_contains "$ALLOW_MISMATCH_ERR" "warning: package postinstall does not match $POSTINSTALL"

assert_fails_contains \
  "only inspects packages with macOS pkgutil" \
  env TCFS_UNAME=/bin/echo PATH="$FAKE_BIN:$PATH" bash "$SCRIPT" --pkg "$GOOD_PKG"

printf 'macOS package structure smoke tests passed\n'
