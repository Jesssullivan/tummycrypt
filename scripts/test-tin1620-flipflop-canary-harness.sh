#!/usr/bin/env bash
#
# Regression tests for tin1620-flipflop-canary-harness.sh.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$REPO_ROOT/scripts/tin1620-flipflop-canary-harness.sh"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-tin1620-harness-test.XXXXXX")"
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

assert_fails_contains() {
  local expected="$1"
  shift

  local out="$TMPDIR/failure.out"
  local err="$TMPDIR/failure.err"

  if "$@" >"$out" 2>"$err"; then
    printf 'expected command to fail: %s\n' "$*" >&2
    exit 1
  fi

  cat "$out" "$err" >"$TMPDIR/failure.combined"
  assert_contains "$TMPDIR/failure.combined" "$expected"
}

HOME_OK="$TMPDIR/home-ok"
EVIDENCE="$TMPDIR/evidence"
OUT="$TMPDIR/plan.out"
mkdir -p "$HOME_OK/git"
CANARY_CANON="$(cd "$HOME_OK/git" && pwd -P)/tcfs-flipflop-canary"

HOME="$HOME_OK" bash "$SCRIPT" \
  --remote seaweedfs://example.invalid/tcfs/tin1620-plan-test \
  --evidence-dir "$EVIDENCE" \
  --honey-host honey-test \
  --honey-existing-mount \
  >"$OUT"

assert_contains "$OUT" "plan-only: no TCFS or SSH commands were run"
assert_contains "$OUT" "run later:"
assert_contains "$EVIDENCE/result.env" "status=plan-only"
assert_contains "$EVIDENCE/result.env" "canary_created=0"
assert_contains "$EVIDENCE/run-metadata.env" "honey_host=honey-test"
assert_contains "$EVIDENCE/run-metadata.env" "execute=0"
assert_contains "$EVIDENCE/README.md" "no tcfs command is executed"
assert_contains "$EVIDENCE/README.md" "storage ready and NATS connected"
assert_contains "$EVIDENCE/run-when-ready.sh" "--execute"
assert_contains "$EVIDENCE/run-when-ready.sh" "--honey-existing-mount"
test ! -e "$HOME_OK/git/tcfs-flipflop-canary"

FAKE_BIN="$TMPDIR/fake-bin"
FAKE_DEMO="$FAKE_BIN/fake-demo.sh"
FAKE_LOG="$TMPDIR/fake-demo.log"
EXEC_EVIDENCE="$TMPDIR/execute-evidence"
mkdir -p "$FAKE_BIN"

cat >"$FAKE_DEMO" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'fake-demo' >>"$TCFS_FAKE_DEMO_LOG"
printf ' %q' "$@" >>"$TCFS_FAKE_DEMO_LOG"
printf '\n' >>"$TCFS_FAKE_DEMO_LOG"
EOF
chmod +x "$FAKE_DEMO"

HOME="$HOME_OK" \
TCFS_TIN1620_DEMO_SCRIPT="$FAKE_DEMO" \
TCFS_FAKE_DEMO_LOG="$FAKE_LOG" \
bash "$SCRIPT" \
  --execute \
  --skip-readiness \
  --remote seaweedfs://example.invalid/tcfs/tin1620-execute-test \
  --evidence-dir "$EXEC_EVIDENCE" \
  --tcfs-bin /tmp/tcfs-current \
  --honey-host honey-run \
  --honey-existing-mount \
  >"$TMPDIR/execute.out"

assert_contains "$FAKE_LOG" "--neo-root"
assert_contains "$FAKE_LOG" "$CANARY_CANON"
assert_contains "$FAKE_LOG" "--push"
assert_contains "$FAKE_LOG" "--run-honey"
assert_contains "$FAKE_LOG" "--tcfs-bin /tmp/tcfs-current"
assert_contains "$FAKE_LOG" "--honey-existing-mount"

BLOCK_EVIDENCE="$TMPDIR/block-evidence"
assert_fails_contains \
  "host load too high" \
  env HOME="$HOME_OK" \
    TCFS_TIN1620_LOAD_1M=99.0 \
    TCFS_TIN1620_DAEMON_UPTIME_SECS=999 \
    TCFS_TIN1620_STATUS_CMD="printf 'storage: ok\nnats: connected\n'" \
    TCFS_TIN1620_DEMO_SCRIPT="$FAKE_DEMO" \
    TCFS_FAKE_DEMO_LOG="$FAKE_LOG.block" \
    bash "$SCRIPT" \
      --execute \
      --remote seaweedfs://example.invalid/tcfs/tin1620-block-test \
      --evidence-dir "$BLOCK_EVIDENCE" \
      --max-load 1.0
assert_contains "$BLOCK_EVIDENCE/result.env" "status=blocked-host-readiness"
test ! -e "$FAKE_LOG.block"

READY_EVIDENCE="$TMPDIR/ready-evidence"
READY_LOG="$TMPDIR/ready-demo.log"
HOME="$HOME_OK" \
TCFS_TIN1620_LOAD_1M=0.1 \
TCFS_TIN1620_DAEMON_UPTIME_SECS=999 \
TCFS_TIN1620_STATUS_CMD="printf 'storage: ok\nnats: connected\n'" \
TCFS_TIN1620_DEMO_SCRIPT="$FAKE_DEMO" \
TCFS_FAKE_DEMO_LOG="$READY_LOG" \
bash "$SCRIPT" \
  --execute \
  --remote seaweedfs://example.invalid/tcfs/tin1620-ready-test \
  --evidence-dir "$READY_EVIDENCE" \
  --honey-existing-mount \
  >"$TMPDIR/ready.out"
assert_contains "$READY_EVIDENCE/readiness/readiness.env" "load_ready=1"
assert_contains "$READY_EVIDENCE/readiness/readiness.env" "daemon_ready=1"
assert_contains "$READY_EVIDENCE/readiness/readiness.env" "storage_ready=1"
assert_contains "$READY_EVIDENCE/readiness/readiness.env" "nats_ready=1"
assert_contains "$READY_LOG" "--run-honey"

assert_fails_contains \
  "refusing to use HOME as canary root" \
  env HOME="$HOME_OK" bash "$SCRIPT" \
    --canary-root "$HOME_OK" \
    --evidence-dir "$TMPDIR/bad-home"

assert_fails_contains \
  "refusing to use ~/git as canary root" \
  env HOME="$HOME_OK" bash "$SCRIPT" \
    --canary-root "$HOME_OK/git" \
    --evidence-dir "$TMPDIR/bad-git"

printf 'tin1620 flipflop canary harness tests passed\n'
