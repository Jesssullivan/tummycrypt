#!/usr/bin/env bash
#
# Static contract tests for the macOS/iOS FileProvider surface.
#
# This does not prove Finder renders badges or progress. It prevents the Swift
# item/extension contract and Info.plist declarations from drifting before the
# live macOS postinstall smoke exercises the signed app.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ITEM_SWIFT="${REPO_ROOT}/swift/fileprovider/Sources/Extension/FileProviderItem.swift"
EXTENSION_SWIFT="${REPO_ROOT}/swift/fileprovider/Sources/Extension/FileProviderExtension.swift"
PLIST="${REPO_ROOT}/swift/fileprovider/resources/Extension-Info.plist"

fail() {
  printf 'FileProvider surface contract test failed: %s\n' "$*" >&2
  exit 1
}

assert_contains() {
  local file="$1"
  local needle="$2"
  grep -Fq -- "$needle" "$file" || fail "missing '${needle}' in ${file#"$REPO_ROOT"/}"
}

python3 - "$PLIST" <<'PY'
import plistlib
import sys

plist_path = sys.argv[1]
with open(plist_path, "rb") as handle:
    root = plistlib.load(handle)

extension = root.get("NSExtension", {})
decorations = {
    entry.get("Identifier")
    for entry in extension.get("NSFileProviderDecorations", [])
}
actions = {
    entry.get("NSExtensionFileProviderActionIdentifier")
    for entry in extension.get("NSExtensionFileProviderActions", [])
}

expected_decorations = {
    "io.tinyland.tcfs.fileprovider.decoration.conflict",
    "io.tinyland.tcfs.fileprovider.decoration.locked",
    "io.tinyland.tcfs.fileprovider.decoration.pinned",
    "io.tinyland.tcfs.fileprovider.decoration.excluded",
}
expected_actions = {
    "io.tinyland.tcfs.action.unsync",
    "io.tinyland.tcfs.action.pin",
}

missing_decorations = sorted(expected_decorations - decorations)
missing_actions = sorted(expected_actions - actions)

if missing_decorations or missing_actions:
    if missing_decorations:
        print(f"missing FileProvider decorations: {missing_decorations}", file=sys.stderr)
    if missing_actions:
        print(f"missing FileProvider actions: {missing_actions}", file=sys.stderr)
    sys.exit(1)
PY

assert_contains "$ITEM_SWIFT" "NSFileProviderItemDecorating"
assert_contains "$ITEM_SWIFT" "var decorations: [NSFileProviderItemDecorationIdentifier]?"
assert_contains "$ITEM_SWIFT" 'case "conflict":'
assert_contains "$ITEM_SWIFT" "return [TCFSDecoration.conflict]"
assert_contains "$ITEM_SWIFT" 'case "locked":'
assert_contains "$ITEM_SWIFT" "return [TCFSDecoration.locked]"

for decoration in conflict locked pinned excluded; do
  assert_contains "$ITEM_SWIFT" "static let ${decoration}"
  assert_contains "$PLIST" "io.tinyland.tcfs.fileprovider.decoration.${decoration}"
done

assert_contains "$EXTENSION_SWIFT" "tcfs_provider_fetch_with_progress"
assert_contains "$EXTENSION_SWIFT" "prog.totalUnitCount = Int64(total)"
assert_contains "$EXTENSION_SWIFT" "prog.completedUnitCount = Int64(completed)"
assert_contains "$EXTENSION_SWIFT" "Unmanaged<Progress>.fromOpaque(progressPtr).release()"
assert_contains "$EXTENSION_SWIFT" "self.signalEnumeratorUpdate(for: parentId)"

assert_contains "$EXTENSION_SWIFT" "NSFileProviderCustomAction"
assert_contains "$EXTENSION_SWIFT" 'case "io.tinyland.tcfs.action.unsync":'
assert_contains "$EXTENSION_SWIFT" "tcfs_provider_unsync"
assert_contains "$EXTENSION_SWIFT" 'case "io.tinyland.tcfs.action.pin":'
assert_contains "$EXTENSION_SWIFT" "tcfs_provider_fetch"
assert_contains "$EXTENSION_SWIFT" "progress.completedUnitCount += 1"
assert_contains "$EXTENSION_SWIFT" "signalEnumerator("

printf 'FileProvider surface contract tests passed\n'
