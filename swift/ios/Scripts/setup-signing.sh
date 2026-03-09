#!/usr/bin/env bash
# Setup code signing for TCFS iOS from Terminal.app
# Run this in Terminal.app (NOT Claude Code) — requires keychain entitlements.
#
# Prerequisites:
#   - /tmp/tcfs-dist-new.key (RSA-2048 private key)
#   - /tmp/tcfs-dist-new.cer (Apple Distribution cert, DER)
#   - /tmp/AppleWWDRCAG3.cer (WWDR G3 intermediate)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

echo "==> TCFS iOS Code Signing Setup"
echo "    This script imports the Apple Distribution certificate"
echo "    into the Data Protection keychain for codesign access."
echo ""

# Check we have the cert files
for f in /tmp/tcfs-dist-new.key /tmp/tcfs-dist-new.cer /tmp/AppleWWDRCAG3.cer; do
    if [ ! -f "$f" ]; then
        echo "ERROR: Missing $f" >&2
        exit 1
    fi
done

# Convert DER cert to PEM if needed
if ! head -1 /tmp/tcfs-dist-new.cer | grep -q "BEGIN"; then
    openssl x509 -in /tmp/tcfs-dist-new.cer -inform DER -out /tmp/tcfs-dist-new.pem -outform PEM
else
    cp /tmp/tcfs-dist-new.cer /tmp/tcfs-dist-new.pem
fi

# Convert WWDR G3 to PEM
openssl x509 -in /tmp/AppleWWDRCAG3.cer -inform DER -out /tmp/AppleWWDRCAG3.pem -outform PEM 2>/dev/null || true

# Create P12 with Triple-DES (macOS-compatible)
echo "==> Creating PKCS12 bundle..."
openssl pkcs12 -export \
    -inkey /tmp/tcfs-dist-new.key \
    -in /tmp/tcfs-dist-new.pem \
    -certfile /tmp/AppleWWDRCAG3.pem \
    -out /tmp/tcfs-dist-3des.p12 \
    -passout pass:tcfs \
    -certpbe PBE-SHA1-3DES \
    -keypbe PBE-SHA1-3DES \
    -macalg sha1

# Import into login keychain (the standard macOS keychain with Data Protection)
echo "==> Importing into login keychain..."
echo "    You may be prompted to unlock your keychain."
security import /tmp/tcfs-dist-3des.p12 \
    -k ~/Library/Keychains/login.keychain-db \
    -P "tcfs" \
    -T /usr/bin/codesign \
    -T /usr/bin/security \
    -T /usr/bin/productbuild \
    -A

# Import WWDR G3 intermediate
security import /tmp/AppleWWDRCAG3.cer \
    -k ~/Library/Keychains/login.keychain-db \
    -T /usr/bin/codesign \
    -A 2>/dev/null || true

# Set partition list (allows codesign without GUI prompts)
echo "==> Setting keychain access controls..."
echo "    You may be prompted for your login keychain password."
security set-key-partition-list -S "apple-tool:,apple:,codesign:" \
    -s -k "" ~/Library/Keychains/login.keychain-db 2>/dev/null || \
    echo "    (partition list may need your keychain password — try manually if needed)"

# Ensure login keychain is in the search list
security list-keychains -d user -s \
    ~/Library/Keychains/login.keychain-db \
    /Library/Keychains/System.keychain

echo ""
echo "==> Verifying..."
security find-identity -v -p codesigning

echo ""
VALID=$(security find-identity -v -p codesigning | grep -c "valid identities" || true)
if security find-identity -v -p codesigning | grep -q "Apple Distribution"; then
    echo "SUCCESS! Apple Distribution identity is ready for codesigning."
    echo ""
    echo "You can now run the archive:"
    echo "  cd $(dirname "$SCRIPT_DIR")"
    echo "  ./Scripts/upload-testflight.sh"
else
    echo "WARNING: Identity not showing as valid yet."
    echo "Try: security unlock-keychain ~/Library/Keychains/login.keychain-db"
    echo "Then re-run this script."
fi
