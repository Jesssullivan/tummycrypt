#!/usr/bin/env bash
#
# Match downloaded Apple Developer ID .cer files against a local Keychain
# signing identity. Useful when the Developer portal shows duplicate certs with
# the same display name or expiry date.
#
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/macos-developer-cert-match.sh [options] <certificate.cer>...

Options:
  --identity <name-or-sha1>   Local signing identity to match
                              (default: Developer ID Application: John Sullivan (QP994XQKNH))
  -h, --help                  Show this help
EOF
}

IDENTITY="${TCFS_CODESIGN_IDENTITY:-Developer ID Application: John Sullivan (QP994XQKNH)}"
CERTS=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --identity)
      IDENTITY="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      CERTS+=("$1")
      shift
      ;;
  esac
done

if [[ "${#CERTS[@]}" -eq 0 ]]; then
  usage >&2
  exit 2
fi

cert_sha1() {
  local cert="$1"

  openssl x509 -inform DER -in "$cert" -noout -fingerprint -sha1 |
    awk -F= '/Fingerprint/ { gsub(":", "", $2); print toupper($2) }'
}

cert_serial() {
  local cert="$1"

  openssl x509 -inform DER -in "$cert" -noout -serial | sed 's/^serial=//'
}

cert_enddate() {
  local cert="$1"

  openssl x509 -inform DER -in "$cert" -noout -enddate | sed 's/^notAfter=//'
}

LOCAL_SHA1="$(
  security find-certificate -p -c "$IDENTITY" |
    openssl x509 -noout -fingerprint -sha1 |
    awk -F= '/Fingerprint/ { gsub(":", "", $2); print toupper($2) }'
)"

if [[ -z "$LOCAL_SHA1" ]]; then
  echo "could not find local certificate for identity: $IDENTITY" >&2
  exit 1
fi

echo "local identity: $IDENTITY"
echo "local sha1: $LOCAL_SHA1"

matched=0
for cert in "${CERTS[@]}"; do
  [[ -f "$cert" ]] || {
    echo "certificate file not found: $cert" >&2
    exit 1
  }

  sha1="$(cert_sha1 "$cert")"
  serial="$(cert_serial "$cert")"
  enddate="$(cert_enddate "$cert")"

  marker=" "
  if [[ "$sha1" == "$LOCAL_SHA1" ]]; then
    marker="*"
    matched=1
  fi

  printf '%s %s\n' "$marker" "$cert"
  printf '  sha1: %s\n' "$sha1"
  printf '  serial: %s\n' "$serial"
  printf '  expires: %s\n' "$enddate"
done

if [[ "$matched" != "1" ]]; then
  echo "no downloaded certificate matched the local identity" >&2
  exit 1
fi
