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
assert_contains "$EVIDENCE/run-metadata.env" "source=$SOURCE_CANON"
assert_contains "$EVIDENCE/run-metadata.env" "push=0"
assert_contains "$EVIDENCE/run-metadata.env" "honey_start_mount=1"
assert_contains "$EVIDENCE/run-metadata.env" "honey_smoke_max_depth=8"
assert_contains "$EVIDENCE/run-metadata.env" "honey_smoke_timeout_secs=900"
assert_contains "$EVIDENCE/result.env" "status=plan-only"
assert_contains "$EVIDENCE/result.env" "proof=inventory-shadow-config"
assert_contains "$EVIDENCE/parity-gates.env" "source_symlink_count=2"
assert_contains "$EVIDENCE/parity-gates.env" "shadow_symlink_count=2"
assert_contains "$EVIDENCE/parity-gates.env" "shadow_symlink_targets_match=1"
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
assert_contains "$EVIDENCE/honey-linux-xr-shadow-run.sh" "tcfs mount"
assert_contains "$EVIDENCE/honey-linux-xr-shadow-run.sh" "tcfs --version"
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

RESUME_EVIDENCE="$TMPDIR/resume-evidence"
RESUME_STATE="$TMPDIR/resume-state"
mkdir -p "$RESUME_EVIDENCE" "$RESUME_STATE"
cat >"$RESUME_EVIDENCE/push.log" <<'EOF'
2026-05-11T05:59:59.000000Z  WARN tcfs_sync::storage: transient object write failure key=tcfs/chunks/demo attempt=1 delay_ms=50
2026-05-11T06:00:00.000000Z  INFO tcfs_sync::engine: chunk upload progress path=/tmp/shadow/.git/objects/pack/pack-demo.idx completed_chunks=5 chunks=10 uploaded_bytes=200 streaming=true
2026-05-11T06:00:01.000000Z  INFO tcfs_sync::engine: uploaded path=/tmp/shadow/.git/objects/pack/pack-demo.idx hash=111 chunks=10 bytes=1000 uploaded_bytes=400 streaming=true chunk_upload_concurrency=4 chunk_exists_check=false
2026-05-11T06:00:02.000000Z  INFO tcfs_sync::engine: uploaded path=/tmp/shadow/space dir/read me.txt hash=222 chunks=2 bytes=20 uploaded_bytes=20 streaming=false chunk_upload_concurrency=4 chunk_exists_check=false
2026-05-11T06:00:03.000000Z  INFO tcfs_sync::engine: uploaded path=/tmp/shadow/empty.txt hash=333 chunks=0 bytes=0 uploaded_bytes=0 streaming=false chunk_upload_concurrency=4 chunk_exists_check=false
Push complete:
  uploaded: 2 files (42 B)
EOF
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
assert_contains "$RESUME_EVIDENCE/result.env" "status=0"
assert_contains "$RESUME_EVIDENCE/result.env" "proof=shadow-push"
assert_contains "$RESUME_EVIDENCE/README.md" "push-storage-summary.env"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "upload_rows=3"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "chunk_upload_progress_rows=1"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "total_file_bytes=1020"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "total_uploaded_bytes=420"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "dedupe_or_existing_bytes=600"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "total_chunks=12"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "streaming_rows=1"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "zero_chunk_rows=1"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "chunk_exists_check_false_rows=3"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "chunk_upload_concurrency_values=4"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "warn_rows=1"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "retry_warning_rows=1"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "error_rows=0"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "pack_index_rows=1"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "pack_index_chunks=10"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "max_chunks=10"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.env" "max_chunks_path=/tmp/shadow/.git/objects/pack/pack-demo.idx"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.md" "TCFS Push Storage Summary"
assert_contains "$RESUME_EVIDENCE/push-storage-summary.md" "Dedupe or existing bytes"

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
  "--resume-after-push requires push.log with 'Push complete:'" \
  env HOME="$HOME_OK" bash "$SCRIPT" \
    --source "$SOURCE" \
    --shadow-root "$SHADOW" \
    --evidence-dir "$BAD_RESUME_EVIDENCE" \
    --state-dir "$BAD_RESUME_STATE" \
    --remote seaweedfs://example.invalid/tcfs/home-canary-test \
    --resume-after-push \
    --reuse-shadow

printf 'home canary linux-xr shadow tests passed\n'
