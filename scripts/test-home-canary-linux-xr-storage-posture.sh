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
for arg in "$@"; do
  if [[ "$arg" == "push" ]]; then
    printf '2026-05-12T00:00:00Z  INFO tcfs_sync::engine: chunk upload heartbeat path=/tmp/linux-xr/.git/objects/pack/pack-test.pack completed_chunks=0 chunks=2 uploaded_bytes=0 file_elapsed_ms=1000 completed_chunks_per_sec=0 uploaded_bytes_per_sec=0 streaming=true pending_uploads=2 chunk_upload_concurrency=%s wait_elapsed_ms=1000\n' "${TCFS_UPLOAD_CHUNK_CONCURRENCY:-0}"
    printf '2026-05-12T00:00:01Z  INFO tcfs_sync::engine: uploaded path=/tmp/linux-xr/.git/objects/pack/pack-test.pack hash=abc chunks=2 bytes=8192 uploaded_bytes=8192 upload_elapsed_ms=1000 upload_chunks_per_sec=2 upload_bytes_per_sec=8192 streaming=true fresh_prefix_publish=true remote_conflict_check=false chunk_upload_concurrency=%s chunk_exists_check=false chunk_write_timeout_secs=%s\n' "${TCFS_UPLOAD_CHUNK_CONCURRENCY:-0}" "${TCFS_UPLOAD_CHUNK_TIMEOUT_SECS:-0}"
    printf 'Push complete: 1 files\n'
    exit 0
  fi
done
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
  --file-upload-concurrency 5 \
  --progress-every-chunks 19 \
  --chunk-timeout-secs 23 \
  --progress-heartbeat-secs 11 \
  --s3-connect-timeout-secs 5 \
  --s3-pool-idle-timeout-secs 13 \
  --s3-pool-max-idle-per-host 7 \
  --s3-http1-only \
  --socket-sample-interval-secs 3 \
  --honey-host honey-test \
  >"$OUT"

assert_contains "$OUT" "home canary evidence:"
assert_contains "$EVIDENCE/storage-posture.env" "posture_claim=not-production-storage-posture"
assert_contains "$EVIDENCE/storage-posture.env" "helper_status=0"
assert_contains "$EVIDENCE/storage-posture.env" "remote_prefix=home-canary-linux-xr-storage-posture-test"
assert_contains "$EVIDENCE/storage-posture.env" "credential_source=unset_or_helper_default"
assert_contains "$EVIDENCE/storage-posture.env" "credential_aws_secret_access_key_present=0"
assert_contains "$EVIDENCE/storage-posture.env" "state_dir=$STATE"
assert_contains "$EVIDENCE/storage-posture.env" "resume_after_push=0"
assert_contains "$EVIDENCE/storage-posture.env" "reuse_shadow=0"
assert_contains "$EVIDENCE/storage-posture.env" "tcfs_binary_profile=cargo-release"
assert_contains "$EVIDENCE/storage-posture.env" "tcfs_version=tcfs 0.12.12-test"
assert_contains "$EVIDENCE/storage-posture.env" "assume_fresh_prefix=1"
assert_contains "$EVIDENCE/storage-posture.env" "upload_concurrency=7"
assert_contains "$EVIDENCE/storage-posture.env" "file_upload_concurrency=5"
assert_contains "$EVIDENCE/storage-posture.env" "progress_every_chunks=19"
assert_contains "$EVIDENCE/storage-posture.env" "chunk_timeout_secs=23"
assert_contains "$EVIDENCE/storage-posture.env" "progress_heartbeat_secs=11"
assert_contains "$EVIDENCE/storage-posture.env" "storage_max_concurrent_ops=7"
assert_contains "$EVIDENCE/storage-posture.env" "storage_s3_connect_timeout_secs=5"
assert_contains "$EVIDENCE/storage-posture.env" "storage_s3_pool_idle_timeout_secs=13"
assert_contains "$EVIDENCE/storage-posture.env" "storage_s3_pool_max_idle_per_host=7"
assert_contains "$EVIDENCE/storage-posture.env" "storage_s3_http1_only=1"
assert_contains "$EVIDENCE/storage-posture.env" "storage_object_model=large-sequential-fastcdc-for-git-pack-and-rev"
assert_contains "$EVIDENCE/storage-posture.env" "git_pack_chunk_profile=min=1MiB avg=4MiB max=16MiB"
assert_contains "$EVIDENCE/storage-posture.env" "git_pack_reverse_index_chunk_profile=min=1MiB avg=4MiB max=16MiB"
assert_contains "$EVIDENCE/storage-posture.env" "git_index_chunk_profile=min=32KiB avg=64KiB max=256KiB"
assert_contains "$EVIDENCE/storage-posture.env" "socket_sample_interval_secs=3"
assert_contains "$EVIDENCE/storage-posture.env" "production_storage_posture_claim=0"
assert_contains "$EVIDENCE/storage-posture.env" "keep_shadow=0"
assert_contains "$EVIDENCE/storage-posture.md" "production S3 posture claim."
assert_contains "$EVIDENCE/storage-posture.md" "raw Git \`.pack\` and \`.rev\` files use the large"
assert_contains "$EVIDENCE/run-metadata.env" "push=0"
assert_contains "$EVIDENCE/run-metadata.env" "storage_max_concurrent_ops=7"
assert_contains "$EVIDENCE/tcfs-linux-xr-shadow.toml" "sync_symlinks = true"
assert_contains "$EVIDENCE/tcfs-linux-xr-shadow.toml" "max_concurrent_ops = 7"
assert_contains "$EVIDENCE/tcfs-linux-xr-shadow.toml" "s3_connect_timeout_secs = 5"
assert_contains "$EVIDENCE/tcfs-linux-xr-shadow.toml" "s3_pool_idle_timeout_secs = 13"
assert_contains "$EVIDENCE/tcfs-linux-xr-shadow.toml" "s3_pool_max_idle_per_host = 7"
assert_contains "$EVIDENCE/tcfs-linux-xr-shadow.toml" "s3_http1_only = true"

PUSH_EVIDENCE="$TMPDIR/push-evidence"
"${RUN_ENV[@]}" bash "$SCRIPT" \
  --source "$SOURCE" \
  --shadow-root "$TMPDIR/push-shadow" \
  --evidence-dir "$PUSH_EVIDENCE" \
  --state-dir "$TMPDIR/push-state" \
  --remote seaweedfs://localhost:8333/tcfs/home-canary-linux-xr-storage-posture-socket-test \
  --tcfs-bin "$BIN_DIR/tcfs" \
  --push \
  --upload-concurrency 2 \
  --file-upload-concurrency 4 \
  --progress-every-chunks 1 \
  --chunk-timeout-secs 17 \
  --progress-heartbeat-secs 9 \
  --socket-sample-interval-secs 1 \
  >"$TMPDIR/push.out"
assert_contains "$PUSH_EVIDENCE/storage-posture.env" "helper_status=0"
assert_contains "$PUSH_EVIDENCE/storage-posture.env" "storage_max_concurrent_ops=2"
assert_contains "$PUSH_EVIDENCE/storage-posture.env" "file_upload_concurrency=4"
assert_contains "$PUSH_EVIDENCE/s3-socket-samples.tsv" $'sampled_at_utc\ttcfs_pids\ts3_established_sockets\thighwater\tlimit'
assert_contains "$PUSH_EVIDENCE/s3-socket-summary.env" "socket_sample_interval_secs=1"
assert_contains "$PUSH_EVIDENCE/s3-socket-summary.env" "socket_sample_limit=2"
assert_contains "$PUSH_EVIDENCE/s3-socket-summary.env" "socket_highwater_exceeded_upload_concurrency=0"
assert_contains "$PUSH_EVIDENCE/push-storage-summary.env" "chunk_upload_heartbeat_rows=1"
assert_contains "$PUSH_EVIDENCE/push-storage-summary.env" "chunk_upload_concurrency_values=2"
assert_contains "$PUSH_EVIDENCE/push-storage-summary.env" "chunk_write_timeout_secs_values=17"
assert_contains "$PUSH_EVIDENCE/push-storage-summary.env" "max_upload_elapsed_ms=1000"
assert_contains "$PUSH_EVIDENCE/push-storage-summary.env" "max_upload_bytes_per_sec=8192"
assert_contains "$PUSH_EVIDENCE/push-storage-summary.env" "fresh_prefix_publish_true_rows=1"
assert_contains "$PUSH_EVIDENCE/push-storage-summary.env" "remote_conflict_check_false_rows=1"

gzip -f "$PUSH_EVIDENCE/push.log"
rm -f "$PUSH_EVIDENCE/push-storage-summary.env" "$PUSH_EVIDENCE/push-storage-summary.md"
printf '{}\n' >"$TMPDIR/push-state/push-state.json"
"${RUN_ENV[@]}" bash "$SCRIPT" \
  --source "$SOURCE" \
  --shadow-root "$TMPDIR/push-shadow" \
  --evidence-dir "$PUSH_EVIDENCE" \
  --state-dir "$TMPDIR/push-state" \
  --remote seaweedfs://localhost:8333/tcfs/home-canary-linux-xr-storage-posture-socket-test \
  --tcfs-bin "$BIN_DIR/tcfs" \
  --resume-after-push \
  --reuse-shadow \
  --keep-shadow \
  >"$TMPDIR/resume.out"
assert_contains "$PUSH_EVIDENCE/storage-posture.env" "helper_status=0"
assert_contains "$PUSH_EVIDENCE/storage-posture.env" "push=0"
assert_contains "$PUSH_EVIDENCE/storage-posture.env" "resume_after_push=1"
assert_contains "$PUSH_EVIDENCE/storage-posture.env" "reuse_shadow=1"
assert_contains "$PUSH_EVIDENCE/storage-posture.env" "keep_shadow=1"
assert_contains "$PUSH_EVIDENCE/run-metadata.env" "push=0"
assert_contains "$PUSH_EVIDENCE/run-metadata.env" "resume_after_push=1"
assert_contains "$PUSH_EVIDENCE/run-metadata.env" "reuse_shadow=1"
assert_contains "$PUSH_EVIDENCE/push-storage-summary.env" "upload_rows=1"
assert_contains "$PUSH_EVIDENCE/push-storage-summary.env" "chunk_upload_concurrency_values=2"
if grep -Fq "shadow cleanup after review" "$TMPDIR/resume.out"; then
  printf 'did not expect cleanup hint when --keep-shadow is set\n' >&2
  cat "$TMPDIR/resume.out" >&2
  exit 1
fi

assert_fails_contains \
  "--push and --resume-after-push are mutually exclusive" \
  env -u AWS_ACCESS_KEY_ID -u AWS_SECRET_ACCESS_KEY -u AWS_SESSION_TOKEN HOME="$HOME_OK" bash "$SCRIPT" \
    --source "$SOURCE" \
    --shadow-root "$TMPDIR/conflict-shadow" \
    --evidence-dir "$TMPDIR/conflict-evidence" \
    --state-dir "$TMPDIR/conflict-state" \
    --remote seaweedfs://example.invalid/tcfs/home-canary-linux-xr-storage-posture-conflict \
    --tcfs-bin "$BIN_DIR/tcfs" \
    --push \
    --resume-after-push

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
assert_contains "$ALLOW_EVIDENCE/storage-posture.env" "file_upload_concurrency=1"
assert_contains "$ALLOW_EVIDENCE/storage-posture.env" "remote_prefix=nonstandard-prefix"

printf 'home canary linux-xr storage posture tests passed\n'
