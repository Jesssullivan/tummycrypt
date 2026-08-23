#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)
runner="$repo_root/scripts/installed-runtime-acceptance.sh"
tmp=$(mktemp -d "${TMPDIR:-/tmp}/tcfs-installed-runtime-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT

mkdir -p "$tmp/bin"
cat >"$tmp/bin/tcfs" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
case "${1:-}" in
  --version) echo 'tcfs 0.test' ;;
  roots)
    [[ "${2:-}" == status && "${4:-}" == --json ]]
    printf '{"availability":"ready","reconcile_support":"plan-and-execute"}\n'
    ;;
  reconcile)
    printf 'Plan: 0 push, 0 pull, 0 create-dir, %s delete-local, 0 delete-remote, 0 conflict, 1 up-to-date\n' "${FAKE_DELETE_COUNT:-0}"
    printf 'Plan SHA-256: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n'
    ;;
  *) exit 64 ;;
esac
EOF
chmod +x "$tmp/bin/tcfs"

output=$(bash "$runner" --platform linux --tcfs-bin "$tmp/bin/tcfs" --root egreg --execute)
grep -q 'PASS platform=linux executed=1' <<<"$output"

if FAKE_DELETE_COUNT=1 bash "$runner" --platform linux --tcfs-bin "$tmp/bin/tcfs" --root egreg --execute >/dev/null 2>&1; then
  echo "runner accepted a deleting registered-root plan" >&2
  exit 1
fi

if bash "$runner" --platform linux --tcfs-bin "$repo_root/target/debug/tcfs" --root egreg >/dev/null 2>&1; then
  echo "runner accepted a checkout-local binary" >&2
  exit 1
fi
ln -s "$runner" "$tmp/bin/checkout-link"
if bash "$runner" --platform linux --tcfs-bin "$tmp/bin/checkout-link" --root egreg >/dev/null 2>&1; then
  echo "runner accepted a symlink to a checkout-local executable" >&2
  exit 1
fi

mkdir -p "$tmp/other-checkout/bin"
git -C "$tmp/other-checkout" init --quiet
cp "$tmp/bin/tcfs" "$tmp/other-checkout/bin/tcfs"
if bash "$runner" --platform linux --tcfs-bin "$tmp/other-checkout/bin/tcfs" --root egreg >/dev/null 2>&1; then
  echo "runner accepted a binary from another Git checkout" >&2
  exit 1
fi

mkdir -p "$tmp/TCFS.app" "$tmp/Provider.appex"
printf 'plist' >"$tmp/TCFS.app/Info.plist"
printf 'plist' >"$tmp/Provider.appex/Info.plist"
cat >"$tmp/bin/plutil" <<'EOF'
#!/usr/bin/env bash
case "$2" in
  CFBundlePackageType) [[ "$4" == *.app/Info.plist ]] && echo APPL || echo 'XPC!' ;;
  NSExtension.NSExtensionPointIdentifier) echo com.apple.fileprovider-nonui ;;
  *) exit 1 ;;
esac
EOF
cat >"$tmp/bin/codesign" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "$tmp/bin/plutil" "$tmp/bin/codesign"

PLUTIL_BIN="$tmp/bin/plutil" CODESIGN_BIN="$tmp/bin/codesign" \
  bash "$runner" --platform ios --app-bundle "$tmp/TCFS.app" \
  --provider "$tmp/Provider.appex" | grep -q 'PASS platform=ios'

bash -n "$runner"
echo 'installed-runtime acceptance tests: PASS'
