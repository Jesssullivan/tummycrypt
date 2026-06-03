#!/usr/bin/env bash
#
# Regression tests for home-canary-linux-xr-shadow.sh.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$REPO_ROOT/scripts/home-canary-linux-xr-shadow.sh"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-home-canary-test.XXXXXX")"
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
mkdir -p "$SOURCE/.git/refs/heads" "$SOURCE/.hidden" "$SOURCE/src" "$HOME_OK"

cat >"$SOURCE/README.md" <<'EOF'
# linux-xr fixture
EOF
cat >"$SOURCE/src/main.c" <<'EOF'
int main(void) { return 0; }
EOF
cat >"$SOURCE/.hidden/config" <<'EOF'
hidden fixture
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
ln -s missing-target "$SOURCE/broken-link"
SOURCE_CANON="$(cd "$SOURCE" && pwd -P)"

OUT="$TMPDIR/positive.out"
HOME="$HOME_OK" bash "$SCRIPT" \
  --source "$SOURCE" \
  --shadow-root "$SHADOW" \
  --evidence-dir "$EVIDENCE" \
  --state-dir "$STATE" \
  --remote seaweedfs://example.invalid/tcfs/home-canary-test \
  --honey-host honey-test \
  --honey-start-mount \
  >"$OUT"

assert_contains "$OUT" "home canary evidence:"
assert_contains "$OUT" "parity gate: full-project-parity-not-claimed"
assert_contains "$EVIDENCE/README.md" "TCFS Home Canary linux-xr Shadow Evidence"
assert_contains "$EVIDENCE/README.md" "push-run-metadata.env"
assert_contains "$EVIDENCE/run-metadata.env" "source=$SOURCE_CANON"
assert_contains "$EVIDENCE/run-metadata.env" "push=0"
assert_contains "$EVIDENCE/run-metadata.env" "honey_start_mount=1"
assert_contains "$EVIDENCE/run-metadata.env" "honey_smoke_max_depth=8"
assert_contains "$EVIDENCE/run-metadata.env" "honey_smoke_timeout_secs=900"
assert_contains "$EVIDENCE/run-metadata.env" "upload_assume_fresh_prefix=1"
assert_contains "$EVIDENCE/run-metadata.env" "upload_file_concurrency=0"
assert_contains "$EVIDENCE/run-metadata.env" "upload_chunk_concurrency=0"
assert_contains "$EVIDENCE/result.env" "status=plan-only"
assert_contains "$EVIDENCE/result.env" "proof=inventory-shadow-config"
assert_contains "$EVIDENCE/parity-gates.env" "source_symlink_count=2"
assert_contains "$EVIDENCE/parity-gates.env" "shadow_symlink_count=2"
assert_contains "$EVIDENCE/parity-gates.env" "shadow_symlink_targets_match=1"
assert_contains "$EVIDENCE/parity-gates.env" "push_skipped_symlink_count=0"
assert_contains "$EVIDENCE/parity-gates.env" "mounted_symlink_verification=not-run"
assert_contains "$EVIDENCE/parity-gates.env" "mounted_symlink_verification_rc=not-run"
assert_contains "$EVIDENCE/parity-gates.env" "mounted_symlink_verification_status=not-run"
assert_contains "$EVIDENCE/parity-gates.env" "sync_symlinks=true"
assert_contains "$EVIDENCE/source-inventory/symlinks.txt" "readme-link"
assert_contains "$EVIDENCE/source-inventory/symlink-targets.tsv" $'readme-link\tREADME.md'
assert_contains "$EVIDENCE/source-inventory/symlink-targets.tsv" $'broken-link\tmissing-target'
assert_contains "$EVIDENCE/source-inventory/hidden-dirs.txt" ".hidden"
assert_contains "$EVIDENCE/source-inventory/git-summary.env" "git_dir_present=1"
assert_contains "$EVIDENCE/shadow-inventory/symlinks.txt" "readme-link"
assert_contains "$EVIDENCE/shadow-inventory/symlink-targets.tsv" $'readme-link\tREADME.md'
assert_contains "$EVIDENCE/symlink-shadow-compare.log" "source and shadow symlink target manifests match"
assert_contains "$EVIDENCE/tcfs-linux-xr-shadow.toml" "sync_git_dirs = true"
assert_contains "$EVIDENCE/tcfs-linux-xr-shadow.toml" "sync_hidden_dirs = true"
assert_contains "$EVIDENCE/tcfs-linux-xr-shadow.toml" "git_sync_mode = \"raw\""
assert_contains "$EVIDENCE/tcfs-linux-xr-shadow.toml" "sync_symlinks = true"
assert_contains "$EVIDENCE/tcfs-linux-xr-shadow.toml" "sync_empty_dirs = true"
assert_contains "$EVIDENCE/honey-linux-xr-shadow-commands.txt" "ssh honey-test"
assert_contains "$EVIDENCE/honey-linux-xr-shadow-commands.txt" "symlink-targets.tsv"
assert_contains "$EVIDENCE/honey-linux-xr-shadow-commands.txt" "TCFS_HONEY_START_MOUNT=1"
assert_contains "$EVIDENCE/honey-linux-xr-shadow-commands.txt" "TCFS_HONEY_EXPECTED_VERSION_CONTAINS="
assert_contains "$EVIDENCE/honey-linux-xr-shadow-commands.txt" "TCFS_HONEY_EXPECTED_SHA256="
assert_contains "$EVIDENCE/honey-linux-xr-shadow-commands.txt" "TCFS_HONEY_SYMLINK_TARGETS_FILE="
assert_contains "$EVIDENCE/honey-linux-xr-shadow-commands.txt" "TCFS_HONEY_SMOKE_TIMEOUT_SECS=900"
assert_contains "$EVIDENCE/honey-linux-xr-shadow-run.sh" "tcfs binary requested:"
assert_contains "$EVIDENCE/honey-linux-xr-shadow-run.sh" "tcfs_resolved="
assert_contains "$EVIDENCE/honey-linux-xr-shadow-run.sh" "sha256sum \"\$tcfs_resolved\""
assert_contains "$EVIDENCE/honey-linux-xr-shadow-run.sh" "\"\$tcfs_resolved\" mount"
assert_contains "$EVIDENCE/honey-linux-xr-shadow-run.sh" "--version"
assert_contains "$EVIDENCE/honey-linux-xr-shadow-run.sh" "TCFS_HONEY_EXPECTED_VERSION_CONTAINS"
assert_contains "$EVIDENCE/honey-linux-xr-shadow-run.sh" "TCFS_HONEY_EXPECTED_SHA256"
assert_contains "$EVIDENCE/honey-linux-xr-shadow-run.sh" "tcfs sha256:"
assert_contains "$EVIDENCE/honey-linux-xr-shadow-run.sh" "--expected-content-file"
assert_contains "$EVIDENCE/honey-linux-xr-shadow-run.sh" "--expected-symlink-targets-file"
assert_contains "$EVIDENCE/honey-linux-xr-shadow-run.sh" "timeout \"\$SMOKE_TIMEOUT_SECS\""
test -f "$SHADOW/README.md"
test -L "$SHADOW/readme-link"
test -L "$SHADOW/broken-link"
test -f "$STATE/tcfs-linux-xr-shadow.toml"
test -f "$EVIDENCE/selected-hydration-file.content"

NO_ASSUME_EVIDENCE="$TMPDIR/no-assume-evidence"
NO_ASSUME_SHADOW="$TMPDIR/no-assume-shadow"
NO_ASSUME_STATE="$TMPDIR/no-assume-state"
HOME="$HOME_OK" bash "$SCRIPT" \
  --source "$SOURCE" \
  --shadow-root "$NO_ASSUME_SHADOW" \
  --evidence-dir "$NO_ASSUME_EVIDENCE" \
  --state-dir "$NO_ASSUME_STATE" \
  --remote seaweedfs://example.invalid/tcfs/home-canary-test-no-assume \
  --no-assume-fresh-prefix \
  >"$TMPDIR/no-assume.out"

assert_contains "$NO_ASSUME_EVIDENCE/run-metadata.env" "upload_assume_fresh_prefix=0"

RESUME_EVIDENCE="$TMPDIR/resume-evidence"
RESUME_STATE="$TMPDIR/resume-state"
mkdir -p "$RESUME_EVIDENCE" "$RESUME_STATE"
cat >"$RESUME_EVIDENCE/push.log" <<'EOF'
2026-05-11T05:59:58.000000Z  WARN tcfs_sync::engine: skipping symlink (follow_symlinks=false) path=/tmp/shadow/readme-link target=README.md
2026-05-11T05:59:59.000000Z  WARN tcfs_sync::storage: transient object write failure key=tcfs/chunks/demo attempt=1 delay_ms=50
2026-05-11T05:59:59.500000Z  WARN tcfs_sync::engine: chunk upload failed, retrying key=tcfs/chunks/slow chunk=7 bytes=1024 attempt=1 max=3 kind=timeout timeout_ms=300000 elapsed_ms=300001 delay_ms=100 error=chunk upload timed out after 300000 ms
2026-05-11T06:00:00.000000Z  INFO tcfs_sync::engine: chunk upload progress path=/tmp/shadow/.git/objects/pack/pack-demo.idx completed_chunks=5 chunks=10 uploaded_bytes=200 streaming=true
2026-05-11T06:00:00.500000Z  INFO tcfs_sync::engine: chunk upload heartbeat path=/tmp/shadow/.git/objects/pack/pack-demo.idx completed_chunks=5 chunks=10 uploaded_bytes=200 file_elapsed_ms=60000 completed_chunks_per_sec=0.083333 uploaded_bytes_per_sec=3.333333 streaming=true pending_uploads=4 chunk_upload_concurrency=4 wait_elapsed_ms=60000
2026-05-11T06:00:01.000000Z  INFO tcfs_sync::engine: uploaded path=/tmp/shadow/.git/objects/pack/pack-demo.idx hash=111 chunks=10 bytes=1000 uploaded_bytes=400 upload_elapsed_ms=1000 upload_chunks_per_sec=10 upload_bytes_per_sec=400 streaming=true fresh_prefix_publish=true remote_conflict_check=false chunk_upload_concurrency=4 chunk_exists_check=false chunk_write_timeout_secs=300
2026-05-11T06:00:02.000000Z  INFO tcfs_sync::engine: uploaded path=/tmp/shadow/space dir/read me.txt hash=222 chunks=2 bytes=20 uploaded_bytes=20 upload_elapsed_ms=200 upload_chunks_per_sec=10 upload_bytes_per_sec=100 streaming=false fresh_prefix_publish=true remote_conflict_check=false chunk_upload_concurrency=4 chunk_exists_check=false chunk_write_timeout_secs=300
2026-05-11T06:00:03.000000Z  INFO tcfs_sync::engine: uploaded path=/tmp/shadow/empty.txt hash=333 chunks=0 bytes=0 uploaded_bytes=0 upload_elapsed_ms=10 upload_chunks_per_sec=0 upload_bytes_per_sec=0 streaming=false fresh_prefix_publish=false remote_conflict_check=true chunk_upload_concurrency=4 chunk_exists_check=true chunk_write_timeout_secs=300
Push complete:
  uploaded: 2 files (42 B)
EOF
printf '\033[2m2026-05-11T06:00:04.000000Z\033[0m \033[32m INFO\033[0m \033[2mtcfs_sync::engine\033[0m\033[2m:\033[0m uploaded \033[3mpath\033[0m\033[2m=\033[0m/tmp/shadow/.git/objects/pack/pack-demo.pack \033[3mhash\033[0m\033[2m=\033[0m444 \033[3mchunks\033[0m\033[2m=\033[0m1 \033[3mbytes\033[0m\033[2m=\033[0m100 \033[3muploaded_bytes\033[0m\033[2m=\033[0m100 \033[3mupload_elapsed_ms\033[0m\033[2m=\033[0m100 \033[3mupload_chunks_per_sec\033[0m\033[2m=\033[0m10 \033[3mupload_bytes_per_sec\033[0m\033[2m=\033[0m1000 \033[3mstreaming\033[0m\033[2m=\033[0mtrue \033[3mfresh_prefix_publish\033[0m\033[2m=\033[0mtrue \033[3mremote_conflict_check\033[0m\033[2m=\033[0mfalse \033[3mchunk_upload_concurrency\033[0m\033[2m=\033[0m4 \033[3mchunk_exists_check\033[0m\033[2m=\033[0mfalse \033[3mchunk_write_timeout_secs\033[0m\033[2m=\033[0m300\n' >>"$RESUME_EVIDENCE/push.log"
printf '\033[2m2026-05-11T06:00:05.000000Z\033[0m \033[31mERROR\033[0m \033[2mopendal::services\033[0m\033[2m:\033[0m service=s3 path=tcfs/chunks/demo: write close failed Unexpected (temporary) at write => error code: 502\n' >>"$RESUME_EVIDENCE/push.log"
printf '{}\n' >"$RESUME_STATE/push-state.json"

HOME="$HOME_OK" bash "$SCRIPT" \
  --source "$SOURCE" \
  --shadow-root "$SHADOW" \
  --evidence-dir "$RESUME_EVIDENCE" \
  --state-dir "$RESUME_STATE" \
  --remote seaweedfs://example.invalid/tcfs/home-canary-test \
  --resume-after-push \
  --reuse-shadow \
  >"$TMPDIR/resume.out"

assert_contains "$RESUME_EVIDENCE/shadow-copy.log" "reused existing shadow"
assert_contains "$RESUME_EVIDENCE/run-metadata.env" "resume_after_push=1"
assert_contains "$RESUME_EVIDENCE/run-metadata.env" "reuse_shadow=1"
assert_contains "$RESUME_EVIDENCE/run-metadata.env" "upload_assume_fresh_prefix=1"
assert_contains "$RESUME_EVIDENCE/result.env" "status=0"
assert_contains "$RESUME_EVIDENCE/result.env" "proof=shadow-push"
assert_contains "$RESUME_EVIDENCE/result.env" "parity_status=full-project-parity-not-claimed"
assert_contains "$RESUME_EVIDENCE/result.env" "parity_reason=shadow push skipped symlink entries even though this lane requires symlink preservation"
assert_contains "$RESUME_EVIDENCE/result.env" "push_skipped_symlink_count=1"
assert_contains "$RESUME_EVIDENCE/parity-gates.env" "push_skipped_symlink_count=1"
assert_contains "$RESUME_EVIDENCE/README.md" "push-storage-summary.env"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "upload_rows=4"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "chunk_upload_progress_rows=1"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "chunk_upload_heartbeat_rows=1"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "total_file_bytes=1120"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "total_uploaded_bytes=520"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "dedupe_or_existing_bytes=600"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "total_chunks=13"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "total_upload_elapsed_ms=1310"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "max_upload_elapsed_ms=1000"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "max_upload_elapsed_path=/tmp/shadow/.git/objects/pack/pack-demo.idx"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "max_upload_bytes_per_sec=1000"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "max_upload_chunks_per_sec=10"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "streaming_rows=2"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "zero_chunk_rows=1"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "fresh_prefix_publish_true_rows=3"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "fresh_prefix_publish_false_rows=1"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "remote_conflict_check_true_rows=1"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "remote_conflict_check_false_rows=3"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "chunk_exists_check_true_rows=1"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "chunk_exists_check_false_rows=3"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "chunk_upload_concurrency_values=4"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "chunk_write_timeout_secs_values=300"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "warn_rows=3"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "retry_warning_rows=2"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "timeout_retry_warning_rows=1"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "error_rows=1"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "pack_rows=1"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "pack_file_bytes=100"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "pack_index_rows=1"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "pack_index_chunks=10"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "max_chunks=10"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "max_chunks_path=/tmp/shadow/.git/objects/pack/pack-demo.idx"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.md" "TCFS Push Storage Summary"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.md" "Dedupe or existing bytes"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.md" "Chunk heartbeat rows"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.md" "Max upload elapsed ms"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.md" "Fresh-prefix publish true rows"

GZIP_RESUME_EVIDENCE="$TMPDIR/resume-evidence-gzip"
GZIP_RESUME_STATE="$TMPDIR/resume-state-gzip"
mkdir -p "$GZIP_RESUME_EVIDENCE" "$GZIP_RESUME_STATE"
gzip -c "$RESUME_EVIDENCE/push.log" >"$GZIP_RESUME_EVIDENCE/push.log.gz"
printf '{}\n' >"$GZIP_RESUME_STATE/push-state.json"

HOME="$HOME_OK" bash "$SCRIPT" \
  --source "$SOURCE" \
  --shadow-root "$SHADOW" \
  --evidence-dir "$GZIP_RESUME_EVIDENCE" \
  --state-dir "$GZIP_RESUME_STATE" \
  --remote seaweedfs://example.invalid/tcfs/home-canary-test \
  --resume-after-push \
  --reuse-shadow \
  >"$TMPDIR/resume-gzip.out"

assert_contains "$GZIP_RESUME_EVIDENCE/run-metadata.env" "resume_after_push=1"
assert_contains "$GZIP_RESUME_EVIDENCE/result.env" "status=0"
assert_contains "$GZIP_RESUME_EVIDENCE/result.env" "proof=shadow-push"
assert_contains "$GZIP_RESUME_EVIDENCE/result.env" "push_skipped_symlink_count=1"

assert_fails_contains \
  "refusing to canary full HOME" \
  env HOME="$HOME_OK" bash "$SCRIPT" \
    --source "$HOME_OK" \
    --shadow-root "$TMPDIR/bad-shadow" \
    --evidence-dir "$TMPDIR/bad-evidence" \
    --remote seaweedfs://example.invalid/tcfs/home-canary-test

assert_fails_contains \
  "--honey-remote-dir contains unsafe shell characters" \
  env HOME="$HOME_OK" bash "$SCRIPT" \
    --source "$SOURCE" \
    --shadow-root "$TMPDIR/bad-shadow-2" \
    --evidence-dir "$TMPDIR/bad-evidence-2" \
    --remote seaweedfs://example.invalid/tcfs/home-canary-test \
    --honey-remote-dir '/tmp/tcfs;bad'

BAD_RESUME_EVIDENCE="$TMPDIR/bad-resume-evidence"
BAD_RESUME_STATE="$TMPDIR/bad-resume-state"
mkdir -p "$BAD_RESUME_EVIDENCE" "$BAD_RESUME_STATE"
printf 'still running\n' >"$BAD_RESUME_EVIDENCE/push.log"
printf '{}\n' >"$BAD_RESUME_STATE/push-state.json"
assert_fails_contains \
  "--resume-after-push requires push.log or push.log.gz with 'Push complete:'" \
  env HOME="$HOME_OK" bash "$SCRIPT" \
    --source "$SOURCE" \
    --shadow-root "$SHADOW" \
    --evidence-dir "$BAD_RESUME_EVIDENCE" \
    --state-dir "$BAD_RESUME_STATE" \
    --remote seaweedfs://example.invalid/tcfs/home-canary-test \
    --resume-after-push \
    --reuse-shadow

printf 'home canary linux-xr shadow tests passed\n'
