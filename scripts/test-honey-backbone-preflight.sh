#!/usr/bin/env bash
# shellcheck disable=SC2016 # Markdown backticks are literal in assertions.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$REPO_ROOT/scripts/honey-backbone-preflight.sh"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-honey-backbone-test.XXXXXX")"
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

assert_fails() {
  if "$@" >/dev/null 2>&1; then
    printf 'expected command to fail: %s\n' "$*" >&2
    exit 1
  fi
}

FAKE_BIN="$TMPDIR/fake-bin"
FAKE_HOME="$TMPDIR/home"
mkdir -p "$FAKE_BIN" "$FAKE_HOME/.config/tcfs"

cat >"$FAKE_HOME/.config/tcfs/config.toml" <<'EOF'
[storage]
endpoint = "http://seaweedfs-tcfs:8333"
enforce_tls = false
secret = "do-not-leak"
[sync]
nats_url = "nats://nats-tcfs:4222"
EOF

cat >"$FAKE_BIN/tcfs" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
case "$*" in
  status)
    cat <<'OUT'
tcfsd v0.12.2
  device:        neo.local (03d8a0bd)
  storage:       http://seaweedfs-tcfs:8333 [ok]
  nats:          connected
OUT
    ;;
  "device status")
    cat <<'OUT'
This device: neo.local
  device_id:       03d8a0bd-36de-4df8-9b88-a923a9dd2c7a
  public_key:      age156vuzu696tp0vm66q2h40zpg6atl3czrcvzepnyv4js84tv4450q6yf6hd
OUT
    ;;
  "device list")
    cat <<'OUT'
Enrolled devices (2):
  neo.local [active] id=03d8a0bd
OUT
    if [[ "${FAKE_LOCAL_INCLUDE_HONEY:-0}" == 1 ]]; then
      printf '  honey [active] id=d1176e5d\n'
    fi
    ;;
  *)
    printf 'unexpected tcfs args: %s\n' "$*" >&2
    exit 9
    ;;
esac
EOF
chmod +x "$FAKE_BIN/tcfs"

cat >"$FAKE_BIN/ssh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

while [[ "${1:-}" == -* ]]; do
  case "$1" in
    -o) shift 2 ;;
    *) shift ;;
  esac
done
host="${1:-}"
shift || true
[[ "$host" == "honey" ]] || exit 7

case "$*" in
  hostname)
    printf 'honey\n'
    ;;
  "tcfs status")
    cat <<'OUT'
tcfsd v0.12.2
  device:        honey (d1176e5d)
  storage:       http://10.245.93.143:8333 [ok]
OUT
    printf '  nats:          %s\n' "${FAKE_HONEY_NATS_STATUS:-not connected}"
    ;;
  "tcfs device status")
    if [[ "${FAKE_HONEY_REAL_KEY:-0}" == 1 ]]; then
      cat <<'OUT'
This device: honey
  device_id:       d1176e5d-8baa-413e-8d50-68c1dbd36506
  public_key:      age13xlyh4d7xcj2qu8xks4x3cdy697cf6g4a9q3z6qtnjy2m6te7g3s4vfpaz
OUT
    else
      cat <<'OUT'
This device: honey
  device_id:       d1176e5d-8baa-413e-8d50-68c1dbd36506
  public_key:      age1-device-6b746182
OUT
    fi
    ;;
  "tcfs device list")
    cat <<'OUT'
Enrolled devices (2):
  honey [active] id=d1176e5d
OUT
    if [[ "${FAKE_HONEY_INCLUDE_NEO:-0}" == 1 ]]; then
      printf '  neo.local [active] id=03d8a0bd\n'
    fi
    ;;
  *config.toml*)
    cat <<'OUT'
[storage]
endpoint = "http://10.245.93.143:8333"
enforce_tls = false
secret = "remote-secret"
[sync]
nats_url = "nats://10.245.131.232:4222"
OUT
    ;;
  python3*)
    target="${*: -1}"
    if [[ "$target" == "10.245.131.232" ]]; then
      printf 'tcp=ok\nnats_info=ok\nINFO {"server_id":"fake"}\n'
    else
      printf 'tcp=fail\nnats_info=TimeoutError:timed out\n'
    fi
    ;;
  curl*)
    target="$*"
    if [[ "$target" == *"10.245.131.232"* ]]; then
      printf '{"status":"ok"}\n'
    else
      printf 'curl: timed out\n'
    fi
    ;;
  *)
    printf 'unexpected ssh command: %s\n' "$*" >&2
    exit 8
    ;;
esac
EOF
chmod +x "$FAKE_BIN/ssh"

cat >"$FAKE_BIN/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$*" == *"nats-tcfs"* ]]; then
  printf '{"status":"ok"}\n'
else
  printf 'curl: timed out\n'
fi
EOF
chmod +x "$FAKE_BIN/curl"

cat >"$FAKE_BIN/python3" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
target="${*: -1}"
if [[ "$target" == "nats-tcfs" ]]; then
  printf 'tcp=ok\nnats_info=ok\nINFO {"server_id":"fake"}\n'
else
  printf 'tcp=fail\nnats_info=TimeoutError:timed out\n'
fi
EOF
chmod +x "$FAKE_BIN/python3"

bash -n "$SCRIPT"

OUT="$TMPDIR/preflight.out"
LOG_DIR="$TMPDIR/evidence"
PATH="$FAKE_BIN:$PATH" HOME="$FAKE_HOME" bash "$SCRIPT" \
  --log-dir "$LOG_DIR" \
  --clear-nats-targets \
  --nats-target nats-tcfs \
  --nats-target 10.245.131.232 \
  >"$OUT"

assert_contains "$OUT" 'Status: `blocked-g2-g3`'
assert_contains "$LOG_DIR/result.env" 'status=0'
assert_contains "$LOG_DIR/result.env" 'proof=blocked-g2-g3'
assert_contains "$LOG_DIR/summary.md" 'honey NATS is not connected'
assert_contains "$LOG_DIR/summary.md" 'neo device registry does not include honey'
assert_contains "$LOG_DIR/summary.md" 'honey device registry does not include neo/neo.local'
assert_contains "$LOG_DIR/summary.md" 'honey device public key is placeholder-shaped'
assert_contains "$LOG_DIR/local-config.redacted.toml" 'secret = <redacted>'
assert_contains "$LOG_DIR/honey-config.redacted.toml" 'secret = <redacted>'
assert_contains "$LOG_DIR/nats-probes.tsv" $'local\tnats-tcfs\t{"status":"ok"}'
assert_contains "$LOG_DIR/nats-probes.tsv" $'honey\t10.245.131.232\t{"status":"ok"}'

STRICT_LOG_DIR="$TMPDIR/strict-evidence"
assert_fails env PATH="$FAKE_BIN:$PATH" HOME="$FAKE_HOME" bash "$SCRIPT" \
  --log-dir "$STRICT_LOG_DIR" \
  --clear-nats-targets \
  --nats-target nats-tcfs \
  --strict
assert_contains "$STRICT_LOG_DIR/result.env" 'status=1'

G3_LOG_DIR="$TMPDIR/g3-evidence"
PATH="$FAKE_BIN:$PATH" HOME="$FAKE_HOME" FAKE_HONEY_NATS_STATUS=connected bash "$SCRIPT" \
  --log-dir "$G3_LOG_DIR" \
  --clear-nats-targets \
  --nats-target nats-tcfs \
  >"$TMPDIR/g3.out"
assert_contains "$TMPDIR/g3.out" 'Status: `blocked-g3`'
assert_contains "$G3_LOG_DIR/result.env" 'proof=blocked-g3'
assert_contains "$G3_LOG_DIR/summary.md" 'neo device registry does not include honey'
assert_contains "$G3_LOG_DIR/summary.md" 'honey device registry does not include neo/neo.local'
if grep -Fq 'honey NATS is not connected' "$G3_LOG_DIR/summary.md"; then
  printf 'did not expect honey NATS blocker in registry-only evidence\n' >&2
  cat "$G3_LOG_DIR/summary.md" >&2
  exit 1
fi

COMPLETE_LOG_DIR="$TMPDIR/complete-evidence"
PATH="$FAKE_BIN:$PATH" \
  HOME="$FAKE_HOME" \
  FAKE_HONEY_NATS_STATUS=connected \
  FAKE_LOCAL_INCLUDE_HONEY=1 \
  FAKE_HONEY_INCLUDE_NEO=1 \
  FAKE_HONEY_REAL_KEY=1 \
  bash "$SCRIPT" \
  --log-dir "$COMPLETE_LOG_DIR" \
  --clear-nats-targets \
  --nats-target nats-tcfs \
  --strict \
  >"$TMPDIR/complete.out"
assert_contains "$TMPDIR/complete.out" 'Status: `honey-backbone-preflight-complete`'
assert_contains "$COMPLETE_LOG_DIR/result.env" 'status=0'
assert_contains "$COMPLETE_LOG_DIR/result.env" 'proof=honey-backbone-preflight-complete'
assert_contains "$COMPLETE_LOG_DIR/summary.md" '| honey registry includes neo/neo.local | yes |'
assert_contains "$COMPLETE_LOG_DIR/summary.md" '| honey public key placeholder-shaped | no |'

printf 'honey backbone preflight tests passed\n'
