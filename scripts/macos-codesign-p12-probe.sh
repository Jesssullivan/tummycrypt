#!/usr/bin/env bash
#
# Import a p12 into an isolated temporary keychain and prove codesign can use it.
#
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/macos-codesign-p12-probe.sh --p12 <path> [options]

Options:
  --p12 <path>                  p12 identity to import
  --p12-password <value>        p12 password (default: $TCFS_FILEPROVIDER_LAB_P12_PASSWORD or empty)
  --p12-password-file <path>    read p12 password from a local file
  --identity <name-or-sha1>     identity to sign with (default: first Apple/Mac development identity)
  --keychain-password <value>   temporary keychain password (default: generated)
  --keep-keychain               print and keep the temporary keychain for debugging
  -h, --help                    show help
EOF
}

fail() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

P12_PATH=""
P12_PASSWORD="${TCFS_FILEPROVIDER_LAB_P12_PASSWORD:-}"
P12_PASSWORD_FILE=""
IDENTITY=""
KEYCHAIN_PASSWORD=""
KEEP_KEYCHAIN=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --p12)
      [[ $# -ge 2 ]] || fail "--p12 requires a value"
      P12_PATH="$2"
      shift 2
      ;;
    --p12-password)
      [[ $# -ge 2 ]] || fail "--p12-password requires a value"
      P12_PASSWORD="$2"
      shift 2
      ;;
    --p12-password-file)
      [[ $# -ge 2 ]] || fail "--p12-password-file requires a value"
      P12_PASSWORD_FILE="$2"
      shift 2
      ;;
    --identity)
      [[ $# -ge 2 ]] || fail "--identity requires a value"
      IDENTITY="$2"
      shift 2
      ;;
    --keychain-password)
      [[ $# -ge 2 ]] || fail "--keychain-password requires a value"
      KEYCHAIN_PASSWORD="$2"
      shift 2
      ;;
    --keep-keychain)
      KEEP_KEYCHAIN=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      fail "unknown argument: $1"
      ;;
  esac
done

[[ "$(uname -s)" == "Darwin" ]] || fail "macos-codesign-p12-probe.sh only runs on macOS"
[[ -n "$P12_PATH" ]] || fail "--p12 is required"
[[ -f "$P12_PATH" ]] || fail "p12 not found: $P12_PATH"
if [[ -n "$P12_PASSWORD_FILE" ]]; then
  [[ -f "$P12_PASSWORD_FILE" ]] || fail "p12 password file not found: $P12_PASSWORD_FILE"
  P12_PASSWORD="$(<"$P12_PASSWORD_FILE")"
fi

if [[ -z "$KEYCHAIN_PASSWORD" ]]; then
  KEYCHAIN_PASSWORD="$(openssl rand -hex 16)"
fi

KEYCHAIN_PATH="${TMPDIR:-/tmp}/tcfs-codesign-p12-probe.$$.keychain-db"
PROBE_DIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-codesign-p12-probe-bin.XXXXXX")"
ORIGINAL_KEYCHAINS=()

cleanup() {
  rm -rf "$PROBE_DIR"
  if ((${#ORIGINAL_KEYCHAINS[@]} > 0)); then
    security list-keychains -d user -s "${ORIGINAL_KEYCHAINS[@]}" >/dev/null 2>&1 || true
  fi
  if [[ "$KEEP_KEYCHAIN" != "1" && -f "$KEYCHAIN_PATH" ]]; then
    security delete-keychain "$KEYCHAIN_PATH" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

while IFS= read -r keychain_path; do
  [[ -n "$keychain_path" ]] || continue
  ORIGINAL_KEYCHAINS+=("$keychain_path")
done < <(
  security list-keychains -d user 2>/dev/null \
    | sed -E 's/^[[:space:]]*"//; s/"[[:space:]]*$//; s/^[[:space:]]+//; s/[[:space:]]+$//'
)

security create-keychain -p "$KEYCHAIN_PASSWORD" "$KEYCHAIN_PATH"
security unlock-keychain -p "$KEYCHAIN_PASSWORD" "$KEYCHAIN_PATH"
security set-keychain-settings -lut 21600 "$KEYCHAIN_PATH"
if ((${#ORIGINAL_KEYCHAINS[@]} > 0)); then
  security list-keychains -d user -s "$KEYCHAIN_PATH" "${ORIGINAL_KEYCHAINS[@]}"
else
  security list-keychains -d user -s "$KEYCHAIN_PATH"
fi
security import "$P12_PATH" \
  -f pkcs12 \
  -k "$KEYCHAIN_PATH" \
  -P "$P12_PASSWORD" \
  -A \
  -T /usr/bin/codesign \
  -T /usr/bin/security \
  >/dev/null
security set-key-partition-list \
  -S apple-tool:,apple:,codesign: \
  -s \
  -k "$KEYCHAIN_PASSWORD" \
  "$KEYCHAIN_PATH" \
  >/dev/null

IDENTITIES="$(security find-identity -v -p codesigning "$KEYCHAIN_PATH")"
printf '%s\n' "$IDENTITIES"

if [[ -z "$IDENTITY" ]]; then
  IDENTITY="$(
    awk '/Apple Development|Mac Developer|Mac App Development/ {
      if (match($0, /"[^"]+"/)) {
        print substr($0, RSTART + 1, RLENGTH - 2)
        exit
      }
    }' <<<"$IDENTITIES"
  )"
fi
[[ -n "$IDENTITY" ]] || fail "no Apple/Mac development identity found in imported p12"
if ! grep -F "$IDENTITY" <<<"$IDENTITIES" >/dev/null; then
  fail "requested identity is not present in imported p12: $IDENTITY"
fi

PROBE_BIN="$PROBE_DIR/probe"
cp /bin/echo "$PROBE_BIN"
if ! codesign -f -s "$IDENTITY" --keychain "$KEYCHAIN_PATH" --timestamp=none "$PROBE_BIN"; then
  fail "codesign could not use imported identity: $IDENTITY"
fi
codesign -vvv "$PROBE_BIN"

printf 'codesign probe passed with identity: %s\n' "$IDENTITY"
if [[ "$KEEP_KEYCHAIN" == "1" ]]; then
  printf 'kept keychain: %s\n' "$KEYCHAIN_PATH"
fi
