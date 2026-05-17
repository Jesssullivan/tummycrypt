#!/usr/bin/env bash
#
# Regression tests for macos-fileprovider-neo-cleanup-packet.sh.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$REPO_ROOT/scripts/macos-fileprovider-neo-cleanup-packet.sh"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-macos-cleanup-test.XXXXXX")"
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

assert_not_contains() {
  local file="$1"
  local unexpected="$2"

  if grep -Fq -- "$unexpected" "$file"; then
    printf 'did not expect to find %s in %s\n' "$unexpected" "$file" >&2
    printf '%s\n' '--- output ---' >&2
    cat "$file" >&2
    exit 1
  fi
}

FAKE_BIN="$TMPDIR/fake-bin"
HOME_OK="$TMPDIR/home"
EVIDENCE="$TMPDIR/evidence"
PKG="$TMPDIR/tcfs.pkg"
APP="$TMPDIR/Applications/TCFSProvider.app"
mkdir -p \
  "$FAKE_BIN" \
  "$HOME_OK/.config/google-workspace" \
  "$HOME_OK/Applications" \
  "$HOME_OK/tcfs/secrets/api" \
  "$HOME_OK/tcfs/shared" \
  "$APP/Contents/Extensions/TCFSFileProvider.appex"
printf 'fake pkg\n' >"$PKG"
printf 'fake token metadata\n' >"$HOME_OK/.config/google-workspace/tinyland-business-ops-token.json"
printf 'fake encrypted token\n' >"$HOME_OK/tcfs/secrets/api/github_token.age"
printf 'fixture\n' >"$HOME_OK/tcfs/shared/alpha-test.txt"

cat >"$FAKE_BIN/tcfs" <<'EOF'
#!/usr/bin/env bash
printf 'tcfs 0.12.test\n'
EOF
cat >"$FAKE_BIN/tcfsd" <<'EOF'
#!/usr/bin/env bash
printf 'tcfsd 0.12.test\n'
EOF
cat >"$FAKE_BIN/pluginkit" <<'EOF'
#!/usr/bin/env bash
printf 'Path = /Users/test/Applications/TCFSProvider.app/Contents/Extensions/TCFSFileProvider.appex\n'
EOF
cat >"$FAKE_BIN/codesign" <<'EOF'
#!/usr/bin/env bash
printf 'Authority=Developer ID Application: Test\n'
EOF
cat >"$FAKE_BIN/spctl" <<'EOF'
#!/usr/bin/env bash
printf 'accepted\n'
EOF
cat >"$FAKE_BIN/pkgutil" <<'EOF'
#!/usr/bin/env bash
case "$1" in
  --check-signature)
    printf 'Status: signed by a developer certificate\n'
    ;;
  --expand)
    mkdir -p "$3/Payload"
    printf 'expanded\n'
    ;;
esac
EOF
cat >"$FAKE_BIN/installer" <<'EOF'
#!/usr/bin/env bash
printf 'installer args:'
printf ' %s' "$@"
printf '\n'
EOF
cat >"$FAKE_BIN/sudo" <<'EOF'
#!/usr/bin/env bash
printf 'sudo args:'
printf ' %s' "$@"
printf '\n'
if [[ "$1" == "-n" ]]; then
  shift
fi
"$@"
EOF
cat >"$FAKE_BIN/osascript" <<'EOF'
#!/usr/bin/env bash
printf 'osascript args:'
printf ' %s' "$@"
printf '\n'
EOF
cat >"$FAKE_BIN/launchctl" <<'EOF'
#!/usr/bin/env bash
printf 'io.tinyland.tcfsd\n'
EOF
chmod +x "$FAKE_BIN"/*

OUT="$TMPDIR/positive.out"
PATH="$FAKE_BIN:$PATH" HOME="$HOME_OK" bash "$SCRIPT" \
  --evidence-dir "$EVIDENCE" \
  --pkg "$PKG" \
  --app-path "$APP" \
  >"$OUT"

assert_contains "$OUT" "macOS cleanup evidence:"
assert_contains "$EVIDENCE/README.md" "macOS FileProvider neo Cleanup Packet"
assert_contains "$EVIDENCE/README.md" "Strict production-adjacent Finder smoke remains blocked"
assert_contains "$EVIDENCE/README.md" "Install status: \`not-run\`."
assert_contains "$EVIDENCE/README.md" "This run's strict preflight status: \`not-run\`."
assert_contains "$EVIDENCE/run-metadata.env" "install_pkg=0"
assert_contains "$EVIDENCE/run-metadata.env" "install_mode=sudo-n"
assert_contains "$EVIDENCE/run-metadata.env" "quarantine_stale=0"
assert_contains "$EVIDENCE/run-metadata.env" "strict_preflight=0"
assert_contains "$EVIDENCE/pre-cleanup-inventory/path-resolution.out" "tcfs"
assert_contains "$EVIDENCE/pre-cleanup-inventory/tcfs-version.out" "tcfs 0.12.test"
assert_contains "$EVIDENCE/pre-cleanup-inventory/tcfsd-version.out" "tcfsd 0.12.test"
assert_contains "$EVIDENCE/pre-cleanup-inventory/pluginkit.out" "TCFSProvider.app"
assert_contains "$EVIDENCE/pre-cleanup-inventory/pkgutil-check-signature.out" "signed by a developer certificate"
assert_not_contains "$EVIDENCE/pre-cleanup-inventory/configs.out" "tinyland-business-ops-token.json"
assert_not_contains "$EVIDENCE/pre-cleanup-inventory/tcfs-home-inventory.out" "github_token.age"
assert_contains "$EVIDENCE/pre-cleanup-inventory/tcfs-home-inventory.out" "$HOME_OK/tcfs/shared/alpha-test.txt"

PATH="$FAKE_BIN:$PATH" HOME="$HOME_OK" bash "$SCRIPT" \
  --evidence-dir "$TMPDIR/install-evidence" \
  --pkg "$PKG" \
  --app-path "$APP" \
  --install-pkg \
  --install-mode sudo-n \
  >"$TMPDIR/install.out"

assert_contains "$TMPDIR/install-evidence/README.md" "Install mode: \`sudo-n\`."
assert_contains "$TMPDIR/install-evidence/README.md" "Install status: \`0\`."
assert_contains "$TMPDIR/install-evidence/run-metadata.env" "install_mode=sudo-n"
assert_contains "$TMPDIR/install-evidence/install-pkg-command.env" "command=sudo -n installer -pkg"
assert_contains "$TMPDIR/install-evidence/install-pkg.out" "sudo args: -n installer -pkg $PKG -target /"
assert_contains "$TMPDIR/install-evidence/install-pkg.out" "installer args: -pkg $PKG -target /"

PATH="$FAKE_BIN:$PATH" HOME="$HOME_OK" bash "$SCRIPT" \
  --evidence-dir "$TMPDIR/osascript-install-evidence" \
  --pkg "$PKG" \
  --app-path "$APP" \
  --install-pkg \
  --install-mode osascript \
  >"$TMPDIR/osascript-install.out"

assert_contains "$TMPDIR/osascript-install-evidence/README.md" "Install mode: \`osascript\`."
assert_contains "$TMPDIR/osascript-install-evidence/README.md" "Install status: \`0\`."
assert_contains "$TMPDIR/osascript-install-evidence/install-pkg-command.env" "command=osascript do shell script"
assert_contains "$TMPDIR/osascript-install-evidence/install-pkg.out" "osascript args: -e do shell script"

STALE="$HOME_OK/Applications/TCFSProvider.app"
mkdir -p "$STALE"
PATH="$FAKE_BIN:$PATH" HOME="$HOME_OK" bash "$SCRIPT" \
  --evidence-dir "$TMPDIR/quarantine-evidence" \
  --app-path "$APP" \
  --quarantine-stale \
  >"$TMPDIR/quarantine.out"

assert_contains "$TMPDIR/quarantine-evidence/quarantine-actions.log" "move $STALE"
test ! -e "$STALE"

printf 'macOS FileProvider neo cleanup packet tests passed\n'
