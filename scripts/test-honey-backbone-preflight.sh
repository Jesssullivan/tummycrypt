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
Enrolled devices (1):
  neo.local [active] id=03d8a0bd
OUT
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
  nats:          not connected
OUT
    ;;
  "tcfs device status")
    cat <<'OUT'
This device: honey
  device_id:       d1176e5d-8baa-413e-8d50-68c1dbd36506
  public_key:      age1-device-6b746182
OUT
    ;;
  "tcfs device list")
    cat <<'OUT'
Enrolled devices (1):
  honey [active] id=d1176e5d
OUT
    ;;
  *config.toml*)
    cat <<'OUT'
[storage]
endpoint = "http://10.245.93.143:8333"
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

printf 'honey backbone preflight tests passed\n'
