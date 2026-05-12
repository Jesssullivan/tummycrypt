#!/usr/bin/env bash
#
# Regression tests for home-canary-linux-xr-storage-posture.sh.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$REPO_ROOT/scripts/home-canary-linux-xr-storage-posture.sh"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-storage-posture-test.XXXXXX")"
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

SOURCE="$TMPDIR/linux-xr"
SHADOW="$TMPDIR/shadow"
EVIDENCE="$TMPDIR/evidence"
STATE="$TMPDIR/state"
HOME_OK="$TMPDIR/home"
BIN_DIR="$TMPDIR/target/release"
DEBUG_BIN_DIR="$TMPDIR/target/debug"
mkdir -p "$SOURCE/.git/refs/heads" "$SOURCE/src" "$HOME_OK" "$BIN_DIR" "$DEBUG_BIN_DIR"

cat >"$SOURCE/README.md" <<'EOF'
# linux-xr storage posture fixture
EOF
cat >"$SOURCE/src/main.c" <<'EOF'
int main(void) { return 0; }
EOF
cat >"$SOURCE/.git/HEAD" <<'EOF'
ref: refs/heads/main
EOF
cat >"$SOURCE/.git/config" <<'EOF'
[core]
	repositoryformatversion = 0
	filemode = true
	bare = false
EOF
ln -s README.md "$SOURCE/readme-link"

cat >"$BIN_DIR/tcfs" <<'EOF'
#!/usr/bin/env bash
if [[ "${1:-}" == "--version" ]]; then
  printf 'tcfs 0.12.12-test\n'
  exit 0
fi
printf 'unexpected fake tcfs invocation: %s\n' "$*" >&2
exit 64
EOF
chmod +x "$BIN_DIR/tcfs"

RUN_ENV=(env -u AWS_ACCESS_KEY_ID -u AWS_SECRET_ACCESS_KEY -u AWS_SESSION_TOKEN HOME="$HOME_OK")
OUT="$TMPDIR/positive.out"
"${RUN_ENV[@]}" bash "$SCRIPT" \
  --source "$SOURCE" \
  --shadow-root "$SHADOW" \
  --evidence-dir "$EVIDENCE" \
  --state-dir "$STATE" \
  --remote seaweedfs://example.invalid/tcfs/home-canary-linux-xr-storage-posture-test \
  --tcfs-bin "$BIN_DIR/tcfs" \
  --upload-concurrency 7 \
  --progress-every-chunks 19 \
  --chunk-timeout-secs 23 \
  --honey-host honey-test \
  >"$OUT"

assert_contains "$OUT" "home canary evidence:"
assert_contains "$EVIDENCE/storage-posture.env" "posture_claim=not-production-storage-posture"
assert_contains "$EVIDENCE/storage-posture.env" "helper_status=0"
assert_contains "$EVIDENCE/storage-posture.env" "remote_prefix=home-canary-linux-xr-storage-posture-test"
assert_contains "$EVIDENCE/storage-posture.env" "credential_source=unset_or_helper_default"
assert_contains "$EVIDENCE/storage-posture.env" "credential_aws_secret_access_key_present=0"
assert_contains "$EVIDENCE/storage-posture.env" "state_dir=$STATE"
assert_contains "$EVIDENCE/storage-posture.env" "tcfs_binary_profile=cargo-release"
assert_contains "$EVIDENCE/storage-posture.env" "tcfs_version=tcfs 0.12.12-test"
assert_contains "$EVIDENCE/storage-posture.env" "assume_fresh_prefix=1"
assert_contains "$EVIDENCE/storage-posture.env" "upload_concurrency=7"
assert_contains "$EVIDENCE/storage-posture.env" "progress_every_chunks=19"
assert_contains "$EVIDENCE/storage-posture.env" "chunk_timeout_secs=23"
assert_contains "$EVIDENCE/storage-posture.env" "production_storage_posture_claim=0"
assert_contains "$EVIDENCE/storage-posture.md" "production S3 posture claim."
assert_contains "$EVIDENCE/run-metadata.env" "push=0"
assert_contains "$EVIDENCE/tcfs-linux-xr-shadow.toml" "sync_symlinks = true"

cat >"$DEBUG_BIN_DIR/tcfs" <<'EOF'
#!/usr/bin/env bash
if [[ "${1:-}" == "--version" ]]; then
  printf 'tcfs debug-test\n'
  exit 0
fi
exit 64
EOF
chmod +x "$DEBUG_BIN_DIR/tcfs"

assert_fails_contains \
  "storage posture proof requires a release binary" \
  env -u AWS_ACCESS_KEY_ID -u AWS_SECRET_ACCESS_KEY -u AWS_SESSION_TOKEN HOME="$HOME_OK" bash "$SCRIPT" \
    --source "$SOURCE" \
    --shadow-root "$TMPDIR/debug-shadow" \
    --evidence-dir "$TMPDIR/debug-evidence" \
    --state-dir "$TMPDIR/debug-state" \
    --remote seaweedfs://example.invalid/tcfs/home-canary-linux-xr-storage-posture-debug \
    --tcfs-bin "$DEBUG_BIN_DIR/tcfs"

assert_fails_contains \
  "fresh-prefix storage posture proof requires a prefix containing storage-posture" \
  env -u AWS_ACCESS_KEY_ID -u AWS_SECRET_ACCESS_KEY -u AWS_SESSION_TOKEN HOME="$HOME_OK" bash "$SCRIPT" \
    --source "$SOURCE" \
    --shadow-root "$TMPDIR/bad-prefix-shadow" \
    --evidence-dir "$TMPDIR/bad-prefix-evidence" \
    --state-dir "$TMPDIR/bad-prefix-state" \
    --remote seaweedfs://example.invalid/tcfs/not-fresh-enough \
    --tcfs-bin "$BIN_DIR/tcfs"

assert_fails_contains \
  "remote must include a dedicated non-root prefix" \
  env -u AWS_ACCESS_KEY_ID -u AWS_SECRET_ACCESS_KEY -u AWS_SESSION_TOKEN HOME="$HOME_OK" bash "$SCRIPT" \
    --source "$SOURCE" \
    --shadow-root "$TMPDIR/root-prefix-shadow" \
    --evidence-dir "$TMPDIR/root-prefix-evidence" \
    --state-dir "$TMPDIR/root-prefix-state" \
    --remote seaweedfs://example.invalid/tcfs \
    --tcfs-bin "$BIN_DIR/tcfs"

ALLOW_EVIDENCE="$TMPDIR/allow-evidence"
"${RUN_ENV[@]}" bash "$SCRIPT" \
  --source "$SOURCE" \
  --shadow-root "$TMPDIR/allow-shadow" \
  --evidence-dir "$ALLOW_EVIDENCE" \
  --state-dir "$TMPDIR/allow-state" \
  --remote seaweedfs://example.invalid/tcfs/nonstandard-prefix \
  --tcfs-bin "$DEBUG_BIN_DIR/tcfs" \
  --allow-debug-binary \
  --allow-non-posture-prefix \
  --no-assume-fresh-prefix \
  >"$TMPDIR/allow.out"

assert_contains "$ALLOW_EVIDENCE/storage-posture.env" "tcfs_binary_profile=debug"
assert_contains "$ALLOW_EVIDENCE/storage-posture.env" "assume_fresh_prefix=0"
assert_contains "$ALLOW_EVIDENCE/storage-posture.env" "remote_prefix=nonstandard-prefix"

printf 'home canary linux-xr storage posture tests passed\n'
