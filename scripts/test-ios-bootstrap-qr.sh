#!/usr/bin/env bash
set -euo pipefail
umask 077

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$REPO_ROOT/swift/ios/Scripts/gen-bootstrap-qr.sh"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-ios-bootstrap-qr-test.XXXXXX")"
trap 'rm -rf "$TMPDIR"' EXIT

HOME_DIR="$TMPDIR/home"
CONFIG="$HOME_DIR/.config/tcfs/config.toml"
ACCESS_FILE="$TMPDIR/access"
SECRET_FILE="$TMPDIR/secret"
FAKE_BIN="$TMPDIR/fake-bin"
NO_QR_BIN="$TMPDIR/no-qr-bin"
mkdir -p "$(dirname "$CONFIG")" "$FAKE_BIN" "$NO_QR_BIN"
printf 'test-access\n' >"$ACCESS_FILE"
printf 'test-secret\n' >"$SECRET_FILE"

cat >"$FAKE_BIN/qrencode" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
output=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    -o)
      output="$2"
      shift 2
      ;;
    *) shift ;;
  esac
done
[[ -n "$output" ]]
cat >"${TCFS_QR_CAPTURE:?}"
printf 'fake-png\n' >"$output"
EOF
cat >"$FAKE_BIN/open" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "$FAKE_BIN/qrencode" "$FAKE_BIN/open"

for command_name in cat date grep head hostname sed tr; do
  ln -s "$(command -v "$command_name")" "$NO_QR_BIN/$command_name"
done

run_generator() {
  local test_path="${TCFS_TEST_PATH:-$FAKE_BIN:/usr/bin:/bin}"
  # Start from an empty environment so no inherited TCFS/AWS credential
  # channel can influence the fixture-backed generator invocation.
  env -i \
    HOME="$HOME_DIR" \
    PATH="$test_path" \
    TMPDIR="$TMPDIR" \
    TCFS_S3_ACCESS_KEY_FILE="$ACCESS_FILE" \
    TCFS_S3_SECRET_KEY_FILE="$SECRET_FILE" \
    "$@" \
    /bin/bash "$SCRIPT" ios-test
}

file_mode() {
  if stat -f '%Lp' "$1" >/dev/null 2>&1; then
    stat -f '%Lp' "$1"
  else
    stat -c '%a' "$1"
  fi
}

cat >"$CONFIG" <<'EOF'
[storage]
endpoint = "http://example.invalid:8333"
bucket = "tcfs"
EOF
if run_generator >"$TMPDIR/http.out" 2>"$TMPDIR/http.err"; then
  printf 'expected iOS bootstrap generation to reject HTTP\n' >&2
  exit 1
fi
grep -Fq 'must use https://' "$TMPDIR/http.err"

# An explicit HTTPS endpoint is the migration override for a legacy HTTP config.
OVERRIDE_CAPTURE="$TMPDIR/override.json"
if ! run_generator \
    TCFS_S3_ENDPOINT=https://override-storage.example.invalid \
    TCFS_QR_CAPTURE="$OVERRIDE_CAPTURE" \
    >"$TMPDIR/override.out" 2>"$TMPDIR/override.err"; then
  cat "$TMPDIR/override.err" >&2
  exit 1
fi
grep -Fq '"s3_endpoint":"https://override-storage.example.invalid"' "$OVERRIDE_CAPTURE"
grep -Fq 'Endpoint: https://override-storage.example.invalid' "$TMPDIR/override.out"

cat >"$CONFIG" <<'EOF'
[storage]
endpoint = "https://storage.example.invalid"
bucket = "tcfs"
EOF
HTTPS_CAPTURE="$TMPDIR/https.json"
run_generator TCFS_QR_CAPTURE="$HTTPS_CAPTURE" >"$TMPDIR/https.out"
grep -Fq 'Endpoint: https://storage.example.invalid' "$TMPDIR/https.out"
grep -Fq '"s3_endpoint":"https://storage.example.invalid"' "$HTTPS_CAPTURE"
if grep -Fq 'test-secret' "$TMPDIR/https.out"; then
  printf 'credential leaked to generator stdout\n' >&2
  exit 1
fi
ARTIFACT=$(sed -n 's/^==> QR code saved to: //p' "$TMPDIR/https.out")
[[ -f "$ARTIFACT" ]]
[[ "$(file_mode "$ARTIFACT")" == "600" ]]
[[ "$(file_mode "$(dirname "$ARTIFACT")")" == "700" ]]
[[ "$(file_mode "$HTTPS_CAPTURE")" == "600" ]]

cat >"$CONFIG" <<'EOF'
[storage]
endpoint = "https://user:do-not-log@example.invalid"
bucket = "tcfs"
EOF
if run_generator >"$TMPDIR/userinfo.out" 2>"$TMPDIR/userinfo.err"; then
  printf 'expected iOS bootstrap generation to reject URL userinfo\n' >&2
  exit 1
fi
grep -Fq 'without userinfo, query, or fragment' "$TMPDIR/userinfo.err"
if grep -Fq 'do-not-log' "$TMPDIR/userinfo.err"; then
  printf 'credential-bearing endpoint leaked to generator error\n' >&2
  exit 1
fi

rm "$CONFIG"
if run_generator >"$TMPDIR/missing.out" 2>"$TMPDIR/missing.err"; then
  printf 'expected iOS bootstrap generation to require an endpoint\n' >&2
  exit 1
fi
grep -Fq 'an HTTPS storage endpoint is required' "$TMPDIR/missing.err"

ENV_CAPTURE="$TMPDIR/env-https.json"
run_generator \
  TCFS_S3_ENDPOINT=https://env-storage.example.invalid \
  TCFS_QR_CAPTURE="$ENV_CAPTURE" \
  >"$TMPDIR/env-https.out"
grep -Fq 'Endpoint: https://env-storage.example.invalid' "$TMPDIR/env-https.out"

cat >"$CONFIG" <<'EOF'
[storage]
endpoint = "https://storage.example.invalid"
bucket = "tcfs"
EOF
if TCFS_TEST_PATH="$NO_QR_BIN" run_generator \
    >"$TMPDIR/no-qr.out" 2>"$TMPDIR/no-qr.err"; then
  printf 'expected generator to fail safely without qrencode\n' >&2
  exit 1
fi
grep -Fq 'refusing to print credential-bearing bootstrap JSON' "$TMPDIR/no-qr.err"
if grep -Fq 'test-secret' "$TMPDIR/no-qr.out" "$TMPDIR/no-qr.err"; then
  printf 'credential leaked when qrencode was unavailable\n' >&2
  exit 1
fi

printf 'iOS bootstrap QR transport tests passed\n'
