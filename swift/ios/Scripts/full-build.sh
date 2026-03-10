#!/usr/bin/env bash
# Full TCFS iOS build + sign + export pipeline
# Must run from Terminal.app (needs keychain entitlements)
set -euo pipefail
trap 'echo "==> FAILED at line $LINENO (exit $?): $(sed -n "${LINENO}p" "$0")" >&2' ERR

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
IOS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$IOS_DIR/../.." && pwd)"

echo "==> TCFS iOS Full Build Pipeline"
echo "    Repo: $REPO_ROOT"
echo "    iOS:  $IOS_DIR"
echo ""

# --- Step 1: Import signing identity ---
echo "==> Step 1: Setting up signing identity..."
KEYCHAIN="$HOME/Library/Keychains/tcfs-build.keychain-db"
KC_PASS="tcfs-build"

security delete-keychain "$KEYCHAIN" 2>/dev/null || true
security create-keychain -p "$KC_PASS" "$KEYCHAIN"
security set-keychain-settings -lut 7200 "$KEYCHAIN"
security unlock-keychain -p "$KC_PASS" "$KEYCHAIN"

# Create P12 if needed
if [ ! -f /tmp/tcfs-dist-3des.p12 ]; then
  if ! head -1 /tmp/tcfs-dist-new.cer | grep -q "BEGIN"; then
    openssl x509 -in /tmp/tcfs-dist-new.cer -inform DER -out /tmp/tcfs-dist-new.pem -outform PEM
  fi
  openssl x509 -in /tmp/AppleWWDRCAG3.cer -inform DER -out /tmp/AppleWWDRCAG3.pem -outform PEM 2>/dev/null || true
  openssl pkcs12 -export \
    -inkey /tmp/tcfs-dist-new.key \
    -in /tmp/tcfs-dist-new.pem \
    -certfile /tmp/AppleWWDRCAG3.pem \
    -out /tmp/tcfs-dist-3des.p12 \
    -passout pass:tcfs \
    -certpbe PBE-SHA1-3DES -keypbe PBE-SHA1-3DES -macalg sha1
fi

security import /tmp/tcfs-dist-3des.p12 -k "$KEYCHAIN" -P "tcfs" -T /usr/bin/codesign -A
security import /tmp/AppleWWDRCAG3.cer -k "$KEYCHAIN" -T /usr/bin/codesign -A 2>/dev/null || true
[ -f /tmp/AppleRootCA.cer ] && security import /tmp/AppleRootCA.cer -k "$KEYCHAIN" -T /usr/bin/codesign -A 2>/dev/null || true
security set-key-partition-list -S "apple-tool:,apple:,codesign:" -s -k "$KC_PASS" "$KEYCHAIN" >/dev/null 2>&1

# Also import to login keychain for SecItem API visibility
security import /tmp/tcfs-dist-3des.p12 -k ~/Library/Keychains/login.keychain-db -P "tcfs" -T /usr/bin/codesign -T /usr/bin/security -A 2>/dev/null || true
security import /tmp/AppleWWDRCAG3.cer -k ~/Library/Keychains/login.keychain-db -T /usr/bin/codesign -A 2>/dev/null || true
security set-key-partition-list -S "apple-tool:,apple:,codesign:" -s -k "" ~/Library/Keychains/login.keychain-db >/dev/null 2>&1 || true

# Update search list
EXISTING=$(security list-keychains -d user | tr -d '"' | tr '\n' ' ')
security list-keychains -d user -s "$KEYCHAIN" $EXISTING

echo "    Signing identities:"
security find-identity -v -p codesigning
echo ""

# --- Step 1b: Download provisioning profiles via ASC API ---
echo "==> Step 1b: Installing provisioning profiles..."
PROFILES_DIR="$HOME/Library/MobileDevice/Provisioning Profiles"
mkdir -p "$PROFILES_DIR"

ASC_KEY_PATH="$HOME/.private_keys/AuthKey_ZV65L9B864.p8"
ASC_KEY_ID="ZV65L9B864"
ASC_ISSUER_ID="d5db1c0a-0a82-4a50-9490-7d86be080506"

ASC_JWT_SCRIPT="$SCRIPT_DIR/asc-jwt.py"
if command -v python3 &>/dev/null && [ -f "$ASC_JWT_SCRIPT" ]; then
  JWT=$(python3 "$ASC_JWT_SCRIPT" "$ASC_KEY_ID" "$ASC_ISSUER_ID" "$ASC_KEY_PATH")

  # Download host app profile (DSJJF296HL)
  HOST_UUID="d4a36b3a-33bd-49aa-badf-97a6f6462afb"
  if [ ! -f "$PROFILES_DIR/$HOST_UUID.mobileprovision" ]; then
    echo "    Downloading host app profile..."
    HOST_B64=$(curl -sf "https://api.appstoreconnect.apple.com/v1/profiles/DSJJF296HL" \
      -H "Authorization: Bearer $JWT" | python3 -c "import sys,json; print(json.load(sys.stdin)['data']['attributes']['profileContent'])")
    echo "$HOST_B64" | base64 -d > "$PROFILES_DIR/$HOST_UUID.mobileprovision"
  fi

  # Download extension profile (XUPUW332LB)
  EXT_UUID="df186456-5ce7-4152-bd09-dcdca0f89bfc"
  if [ ! -f "$PROFILES_DIR/$EXT_UUID.mobileprovision" ]; then
    echo "    Downloading extension profile..."
    EXT_B64=$(curl -sf "https://api.appstoreconnect.apple.com/v1/profiles/XUPUW332LB" \
      -H "Authorization: Bearer $JWT" | python3 -c "import sys,json; print(json.load(sys.stdin)['data']['attributes']['profileContent'])")
    echo "$EXT_B64" | base64 -d > "$PROFILES_DIR/$EXT_UUID.mobileprovision"
  fi

  echo "    Profiles installed:"
  ls "$PROFILES_DIR"/*.mobileprovision 2>/dev/null | while read f; do echo "      $(basename "$f")"; done
else
  echo "    WARNING: Cannot download profiles (python3 or asc-jwt.py not found)"
  echo "    Ensure profiles are manually installed in $PROFILES_DIR"
fi
echo ""

# --- Step 2: Build Rust staticlib + UniFFI bindings ---
RUST_TARGET="aarch64-apple-ios"
STATICLIB="$REPO_ROOT/target/$RUST_TARGET/release/libtcfs_file_provider.a"
GENERATED_DIR="$IOS_DIR/Generated"

if [ ! -f "$STATICLIB" ]; then
  echo "==> Step 2: Building Rust staticlib ($RUST_TARGET)..."

  # Resolve paths using system xcrun with Xcode's DEVELOPER_DIR
  # (Nix devShell sets DEVELOPER_DIR to a Nix store path that has no iOS SDK)
  XCRUN="env -u SDKROOT DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer /usr/bin/xcrun"
  IOS_SDK=$($XCRUN --sdk iphoneos --show-sdk-path)
  XCODE_CLANG=$($XCRUN --find clang)
  AR_PATH=$($XCRUN --find ar)

  echo "    iOS SDK: $IOS_SDK"
  echo "    Clang:   $XCODE_CLANG"

  cd "$REPO_ROOT"

  # Build staticlib (release)
  echo "    Running: nix develop -c cargo build -p tcfs-file-provider --target $RUST_TARGET --features uniffi --release"
  if ! nix develop -c bash -c "
    set -euo pipefail
    export SDKROOT='$IOS_SDK'
    export CC_aarch64_apple_ios='$XCODE_CLANG'
    export AR_aarch64_apple_ios='$AR_PATH'
    export CFLAGS_aarch64_apple_ios='--target=arm64-apple-ios -isysroot $IOS_SDK'
    cargo build -p tcfs-file-provider --target $RUST_TARGET --features uniffi --release 2>&1
  "; then
    echo "ERROR: Rust build failed (exit $?)" >&2
    exit 1
  fi

  if [ ! -f "$STATICLIB" ]; then
    echo "ERROR: Staticlib not found after build at $STATICLIB" >&2
    echo "Checking target directory:" >&2
    find "$REPO_ROOT/target" -name "libtcfs_file_provider.a" 2>/dev/null
    exit 1
  fi
  echo "    Staticlib: $STATICLIB ($(du -h "$STATICLIB" | cut -f1))"

  # Generate UniFFI bindings (separate step, uses host cargo)
  echo "    Generating UniFFI Swift bindings..."
  nix develop -c cargo run -p tcfs-file-provider --features uniffi --bin uniffi-bindgen -- \
    generate --library "$STATICLIB" \
    --language swift --out-dir "$GENERATED_DIR"
else
  echo "==> Step 2: Staticlib already built: $(du -h "$STATICLIB" | cut -f1)"
  # Regenerate UniFFI bindings if needed
  if [ ! -f "$GENERATED_DIR/tcfs_file_provider.swift" ]; then
    echo "    Regenerating UniFFI bindings..."
    cd "$REPO_ROOT"
    nix develop -c cargo run -p tcfs-file-provider --features uniffi --bin uniffi-bindgen -- \
      generate --library "$STATICLIB" --language swift --out-dir "$GENERATED_DIR"
  fi
fi

# --- Sanitize environment for Xcode tooling ---
# Nix devShell sets NIX_CC, NIX_LDFLAGS, NIX_CFLAGS_COMPILE, DEVELOPER_DIR, SDKROOT
# which corrupt xcodebuild's linker invocation. Strip ALL Nix vars.
for var in $(env | grep -oE '^NIX_[^=]+'); do unset "$var"; done
unset SDKROOT buildInputs nativeBuildInputs
export DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer
# Restore clean PATH (system + Homebrew, no Nix wrappers for xcode steps)
export PATH="/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin:$HOME/.nix-profile/bin"

# --- Step 3: Generate Xcode project ---
echo "==> Step 3: Generating Xcode project..."
cd "$IOS_DIR"
xcodegen generate

# --- Step 4: Archive ---
echo "==> Step 4: Archiving..."
ARCHIVE_PATH="$IOS_DIR/build/TCFS.xcarchive"
rm -rf "$ARCHIVE_PATH"

env -i \
  HOME="$HOME" \
  PATH="/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin" \
  DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer \
xcodebuild archive \
  -project "$IOS_DIR/TCFS.xcodeproj" \
  -scheme "TCFS" \
  -sdk iphoneos \
  -configuration Release \
  -archivePath "$ARCHIVE_PATH" \
  CODE_SIGN_STYLE=Manual \
  "CODE_SIGN_IDENTITY=Apple Distribution: John Sullivan (QP994XQKNH)" \
  DEVELOPMENT_TEAM=QP994XQKNH \
  ONLY_ACTIVE_ARCH=NO \
  SKIP_INSTALL=NO

echo "    Archive: $ARCHIVE_PATH"

# --- Step 5: Export IPA ---
echo "==> Step 5: Exporting IPA..."
EXPORT_DIR="$IOS_DIR/build/export"
rm -rf "$EXPORT_DIR"

if ! env -i \
  HOME="$HOME" \
  PATH="/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin" \
  DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer \
xcodebuild -exportArchive \
  -archivePath "$ARCHIVE_PATH" \
  -exportPath "$EXPORT_DIR" \
  -exportOptionsPlist "$IOS_DIR/Scripts/ExportOptions.plist" \
  -allowProvisioningUpdates \
  -authenticationKeyPath "$HOME/.private_keys/AuthKey_ZV65L9B864.p8" \
  -authenticationKeyID ZV65L9B864 \
  -authenticationKeyIssuerID d5db1c0a-0a82-4a50-9490-7d86be080506; then
  echo ""
  echo "==> EXPORT FAILED — checking distribution logs..."
  LATEST_LOG=$(ls -td /var/folders/*/T/TCFS_*.xcdistributionlogs 2>/dev/null | head -1)
  if [ -n "$LATEST_LOG" ]; then
    echo "    Log: $LATEST_LOG"
    cat "$LATEST_LOG/IDEDistribution.standard.log" 2>/dev/null | tail -30
  fi
  exit 1
fi

IPA_FILE="$EXPORT_DIR/TCFS.ipa"
echo "    IPA: $IPA_FILE ($(du -h "$IPA_FILE" | cut -f1))"

# --- Step 6: Upload to TestFlight ---
echo "==> Step 6: Uploading to TestFlight..."
/usr/bin/xcrun altool --upload-app \
  -f "$IPA_FILE" \
  -t ios \
  --apiKey ZV65L9B864 \
  --apiIssuer d5db1c0a-0a82-4a50-9490-7d86be080506

echo ""
echo "==> Done! Check App Store Connect for TestFlight processing."
echo "    https://appstoreconnect.apple.com"
