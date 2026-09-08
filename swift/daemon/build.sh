#!/usr/bin/env bash
# Package an explicitly identified tcfsd input as a signed TCFSDaemon.app.
# This produces an intermediate artifact, not a notarized or installable release.
# Source revision is caller-supplied provenance; the release build must separately
# establish its association with the supplied input SHA256. No build is run here.
set -euo pipefail

if [[ $# != 8 ]]; then
  printf '%s\n' 'Usage: build.sh <tcfsd_binary> <new_output_dir> <certificate_sha1> <team_id> <existing_keychain> <input_sha256> <source_revision> <version>' >&2
  exit 2
fi

TCFSD_BINARY="$1"
OUTPUT_DIR="$2"
CERTIFICATE_SHA1="$3"
TEAM_ID="$4"
KEYCHAIN_PATH="$5"
INPUT_SHA256="$6"
SOURCE_REVISION="$7"
VERSION="$8"
IDENTIFIER=io.tinyland.tcfsd
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RESOURCES_DIR="$SCRIPT_DIR/resources"
# Same fake-tool seam as the registered release workflow fixtures. Production
# orchestration uses the native default and independently verifies the artifact.
CODESIGN="${TCFS_DAEMON_CODESIGN:-/usr/bin/codesign}"

[[ "$CERTIFICATE_SHA1" =~ ^[A-F0-9]{40}$ && "$CERTIFICATE_SHA1" != 0000000000000000000000000000000000000000 ]] || {
  printf '%s\n' 'An explicit nonzero uppercase certificate SHA1 is required; auto and ad hoc signing are unsupported.' >&2
  exit 2
}
[[ "$TEAM_ID" =~ ^[A-Z0-9]{10}$ ]] || { printf '%s\n' 'Invalid Team ID.' >&2; exit 2; }
[[ "$INPUT_SHA256" =~ ^[a-f0-9]{64}$ && "$INPUT_SHA256" != 0000000000000000000000000000000000000000000000000000000000000000 ]] || {
  printf '%s\n' 'An explicit nonzero input SHA256 is required.' >&2; exit 2;
}
[[ "$SOURCE_REVISION" =~ ^[a-f0-9]{40}$ && "$SOURCE_REVISION" != 0000000000000000000000000000000000000000 ]] || {
  printf '%s\n' 'An explicit nonzero source revision is required.' >&2; exit 2;
}
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.]+)?$ ]] || { printf '%s\n' 'Version must match the release numeric version with an optional prerelease suffix.' >&2; exit 2; }
[[ "$KEYCHAIN_PATH" == /* && -f "$KEYCHAIN_PATH" && ! -L "$KEYCHAIN_PATH" && -r "$KEYCHAIN_PATH" ]] || {
  printf '%s\n' 'An existing readable regular keychain path is required.' >&2; exit 2;
}
[[ "$OUTPUT_DIR" == /* && "$CODESIGN" == /* && -x "$CODESIGN" ]] || {
  printf '%s\n' 'Output and codesign paths must be absolute.' >&2; exit 2;
}

# Validate before creating output, then copy from the held input descriptor.
# A pre-existing output is never overwritten. Failed owned output is retained
# for inspection; this packager performs no recursive cleanup or installation.
python3 - "$TCFSD_BINARY" "$OUTPUT_DIR" "$RESOURCES_DIR" "$INPUT_SHA256" "$SOURCE_REVISION" "$VERSION" "$TEAM_ID" <<'PY'
import hashlib
import os
import plistlib
import stat
import sys
from pathlib import Path

binary, output, resources, expected, revision, version, team = sys.argv[1:]
resources = Path(resources)
with (resources / "Info.plist").open("rb") as source:
    info = plistlib.load(source)
if (info.get("CFBundleIdentifier") != "io.tinyland.tcfsd"
        or info.get("CFBundleExecutable") != "tcfsd"
        or info.get("CFBundleVersion") != "$(TCFSVersion)"
        or info.get("CFBundleShortVersionString") != "$(TCFSVersion)"):
    raise SystemExit("Unexpected daemon identity or version template")
# Apple bundle version fields stay numeric; the signed release label preserves
# the caller's full prerelease version used by archive and evidence filenames.
info["CFBundleVersion"] = info["CFBundleShortVersionString"] = version.partition("-")[0]
info["TCFSReleaseVersion"] = version
# These signed fields retain the caller's provenance declaration. They do not
# replace an independently qualified source-to-binary build receipt.
info["TCFSSourceRevision"] = revision
info["TCFSInputSHA256"] = expected
entitlements = (resources / "tcfsd.entitlements").read_bytes()
entitlements = entitlements.replace(b"$(TeamIdentifierPrefix)", (team + ".").encode("ascii"))
plistlib.loads(entitlements)

descriptor = os.open(binary, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
with os.fdopen(descriptor, "rb") as source:
    before = os.fstat(source.fileno())
    if not stat.S_ISREG(before.st_mode) or not 0 < before.st_size <= 512 * 1024 * 1024:
        raise SystemExit("Input must be a nonempty regular binary of at most 512 MiB")
    def chunks():
        remaining = before.st_size
        while remaining:
            chunk = source.read(min(1024 * 1024, remaining))
            if not chunk:
                raise SystemExit("Input shortened while packaging")
            remaining -= len(chunk)
            yield chunk
    digest = hashlib.sha256()
    for chunk in chunks():
        digest.update(chunk)
    if digest.hexdigest() != expected:
        raise SystemExit("Input SHA256 mismatch")
    source.seek(0)
    root = Path(output)
    root.mkdir(mode=0o700)
    app = root / "TCFSDaemon.app" / "Contents"
    executable_dir = app / "MacOS"
    executable_dir.mkdir(parents=True)
    copied = hashlib.sha256()
    with (executable_dir / "tcfsd").open("xb") as target:
        for chunk in chunks():
            copied.update(chunk)
            target.write(chunk)
        os.fchmod(target.fileno(), 0o755)
    after = os.fstat(source.fileno())
    if (copied.hexdigest() != expected
            or (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns, before.st_ctime_ns)
            != (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns, after.st_ctime_ns)):
        raise SystemExit("Input changed while packaging; unsigned output retained")
    with (app / "Info.plist").open("xb") as target:
        plistlib.dump(info, target, sort_keys=True)
    with (root / "tcfsd.entitlements").open("xb") as target:
        target.write(entitlements)
PY

APP_BUNDLE="$OUTPUT_DIR/TCFSDaemon.app"
SIGN_ENTITLEMENTS="$OUTPUT_DIR/tcfsd.entitlements"
REQUIREMENT="identifier \"$IDENTIFIER\" and anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = \"$TEAM_ID\" and certificate leaf = H\"$CERTIFICATE_SHA1\""

# Use only the explicitly selected existing keychain. Never import, unlock,
# change its search list/grants, choose an identity or retry with another signer.
"$CODESIGN" --force --sign "$CERTIFICATE_SHA1" --keychain "$KEYCHAIN_PATH" \
  --identifier "$IDENTIFIER" --options runtime --timestamp \
  --entitlements "$SIGN_ENTITLEMENTS" "$APP_BUNDLE"
"$CODESIGN" --verify --strict --test-requirement "$REQUIREMENT" "$APP_BUNDLE"
"$CODESIGN" --display --verbose=4 "$APP_BUNDLE" 2> "$OUTPUT_DIR/signature-details.txt"
python3 - "$OUTPUT_DIR/signature-details.txt" "$TEAM_ID" <<'PY'
import re
import sys
from pathlib import Path

with Path(sys.argv[1]).open("rb") as source:
    raw = source.read(65537)
if len(raw) > 65536:
    raise SystemExit("Signature details exceed bound")
lines = raw.decode("utf-8", errors="strict").splitlines()
def exactly_one(prefix):
    values = [line[len(prefix):] for line in lines if line.startswith(prefix)]
    if len(values) != 1:
        raise SystemExit("Missing or ambiguous signature metadata: " + prefix)
    return values[0]
if exactly_one("Identifier=") != "io.tinyland.tcfsd" or exactly_one("TeamIdentifier=") != sys.argv[2]:
    raise SystemExit("Signature identity mismatch")
flags = re.search(r"\bflags=0x([0-9a-fA-F]+)\b", exactly_one("CodeDirectory "))
if not flags or not int(flags[1], 16) & 0x10000 or int(flags[1], 16) & 0x2:
    raise SystemExit("Signature must use hardened runtime without ad hoc signing")
if exactly_one("Timestamp=").strip().lower() in ("", "none", "not set"):
    raise SystemExit("Secure timestamp missing")
PY

printf '%s\n' "Signed intermediate: $APP_BUNDLE" \
  "Declared source: $SOURCE_REVISION; input SHA256: $INPUT_SHA256; version: $VERSION" \
  'Notarization, linkage, entitlements, runtime qualification and final release publication remain separate gates.'
