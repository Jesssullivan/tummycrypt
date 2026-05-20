#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$REPO_ROOT/scripts/neo-honey-smoke.sh"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-neo-honey-smoke-test.XXXXXX")"
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

assert_file_exists() {
  local file="$1"
  if [[ ! -f "$file" ]]; then
    printf 'expected file to exist: %s\n' "$file" >&2
    exit 1
  fi
}

assert_fails_contains() {
  local expected="$1"
  shift

  local out="$TMPDIR/failure.out"
  if "$@" >"$out" 2>&1; then
    printf 'expected command to fail: %s\n' "$*" >&2
    exit 1
  fi
  assert_contains "$out" "$expected"
}

FAKE_BIN="$TMPDIR/fake-bin"
mkdir -p "$FAKE_BIN"

cat >"$FAKE_BIN/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

printf 'cargo %s\n' "$*" >> "${TCFS_FAKE_CARGO_LOG:?}"

test_name=""
prev=""
for arg in "$@"; do
  if [[ "$prev" == "--test" ]]; then
    prev=""
    continue
  fi
  if [[ "$arg" == "--test" ]]; then
    prev="--test"
    continue
  fi
  case "$arg" in
    seaweedfs_health_check | nats_connect_and_jetstream | neo_honey_two_device_sync_smoke)
      test_name="$arg"
      ;;
  esac
done

if [[ -n "${TCFS_FAKE_FAIL_TEST:-}" && "$test_name" == "$TCFS_FAKE_FAIL_TEST" ]]; then
  printf 'fake cargo failure for %s\n' "$test_name" >&2
  exit 42
fi

printf 'fake cargo success for %s\n' "$test_name"
EOF
chmod +x "$FAKE_BIN/cargo"

export PATH="$FAKE_BIN:$PATH"
export TCFS_FAKE_CARGO_LOG="$TMPDIR/cargo.log"
: >"$TCFS_FAKE_CARGO_LOG"

bash -n "$SCRIPT"

POSITIVE_LOG_DIR="$TMPDIR/positive-evidence"
POSITIVE_OUT="$TMPDIR/positive.out"
env \
  TCFS_E2E_LIVE=1 \
  TCFS_S3_ENDPOINT="https://access:secret@example.invalid:8333/private" \
  TCFS_S3_BUCKET=tcfs-smoke \
  AWS_ACCESS_KEY_ID=test-access \
  AWS_SECRET_ACCESS_KEY=test-secret \
  TCFS_NATS_URL="nats://token@example.invalid:4222" \
  bash "$SCRIPT" --log-dir "$POSITIVE_LOG_DIR" >"$POSITIVE_OUT" 2>&1

assert_contains "$POSITIVE_OUT" "neo-honey smoke passed"
assert_contains "$POSITIVE_LOG_DIR/result.env" "status=0"
assert_contains "$POSITIVE_LOG_DIR/result.env" "proof=neo-honey-live-fleet-acceptance"
assert_contains "$POSITIVE_LOG_DIR/run-metadata.env" "s3_endpoint=https://example.invalid:8333/private"
assert_contains "$POSITIVE_LOG_DIR/run-metadata.env" "nats_url=nats://example.invalid:4222"
assert_contains "$POSITIVE_LOG_DIR/run-metadata.env" "s3_bucket=tcfs-smoke"
assert_file_exists "$POSITIVE_LOG_DIR/seaweedfs-health.log"
assert_file_exists "$POSITIVE_LOG_DIR/nats-jetstream.log"
assert_file_exists "$POSITIVE_LOG_DIR/neo-honey-two-device-sync.log"
assert_contains "$TCFS_FAKE_CARGO_LOG" "seaweedfs_health_check"
assert_contains "$TCFS_FAKE_CARGO_LOG" "nats_connect_and_jetstream"
assert_contains "$TCFS_FAKE_CARGO_LOG" "neo_honey_two_device_sync_smoke"

MISSING_LOG_DIR="$TMPDIR/missing-evidence"
assert_fails_contains \
  "missing required env: TCFS_E2E_LIVE" \
  env bash "$SCRIPT" --log-dir "$MISSING_LOG_DIR"
assert_contains "$MISSING_LOG_DIR/result.env" "status=2"
assert_contains "$MISSING_LOG_DIR/result.env" "proof=blocked-missing-env"
assert_contains "$MISSING_LOG_DIR/result.env" "failed_step=env:TCFS_E2E_LIVE"

FAIL_LOG_DIR="$TMPDIR/fail-evidence"
assert_fails_contains \
  "fake cargo failure for nats_connect_and_jetstream" \
  env \
    TCFS_E2E_LIVE=1 \
    TCFS_S3_ENDPOINT="https://example.invalid:8333/private" \
    TCFS_S3_BUCKET=tcfs-smoke \
    AWS_ACCESS_KEY_ID=test-access \
    AWS_SECRET_ACCESS_KEY=test-secret \
    TCFS_NATS_URL="nats://example.invalid:4222" \
    TCFS_FAKE_FAIL_TEST=nats_connect_and_jetstream \
    bash "$SCRIPT" --log-dir "$FAIL_LOG_DIR"
assert_contains "$FAIL_LOG_DIR/result.env" "status=42"
assert_contains "$FAIL_LOG_DIR/result.env" "proof=failed"
assert_contains "$FAIL_LOG_DIR/result.env" "failed_step=nats-jetstream"

printf 'neo-honey smoke tests passed\n'
