#!/usr/bin/env bash
#
# Regression tests for storage large-restore SLO evaluation.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$REPO_ROOT/scripts/evaluate-storage-large-restore-slo.sh"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-storage-slo-test.XXXXXX")"
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

make_packet() {
  local dir="$1"

  mkdir -p "$dir/restore-proof"
  cat >"$dir/restore-proof/restore-proof.env" <<'EOF'
status=passed
reason=regular files restored exactly
download_chunk_retries=8
execute_elapsed_secs=120
restored_regular_file_bytes=1048576000
restored_regular_file_bytes_per_sec=8738133
EOF
  cat >"$dir/push-storage-summary.env" <<'EOF'
warn_rows=2
retry_warning_rows=1
error_rows=0
EOF
  cat >"$dir/s3-socket-summary.env" <<'EOF'
socket_highwater=4
EOF
  cat >"$dir/restore-proof/reconcile-execute.log" <<'EOF'
2026-05-25T00:00:00Z WARN opendal::services: read failed Unexpected at read => error code: 502
2026-05-25T00:00:01Z WARN tcfs_sync::engine: chunk download failed, retrying key=tcfs/chunks/a attempt=1 max=8
2026-05-25T00:00:02Z WARN opendal::layers::retry: retrying after temporary storage error
EOF
}

bash -n "$SCRIPT"

packet="$TMPDIR/packet"
make_packet "$packet"

"$SCRIPT" --evidence-dir "$packet"
assert_contains "$packet/storage-slo-summary.env" "status=passed"
assert_contains "$packet/storage-slo-summary.env" "http_502_log_lines=1"
assert_contains "$packet/storage-slo-summary.env" "tcfs_chunk_retry_rows=1"

"$SCRIPT" \
  --evidence-dir "$packet" \
  --min-restore-throughput-bps 1000000 \
  --max-restore-elapsed-secs 300 \
  --max-502-log-lines 2 \
  --max-opendal-retry-rows 3 \
  --max-tcfs-chunk-retry-rows 1 \
  --max-push-warn-rows 2 \
  --max-push-error-rows 1 \
  --max-socket-highwater 4
assert_contains "$packet/storage-slo-summary.env" "status=passed"
assert_contains "$packet/storage-slo-summary.env" "max_502_log_lines=2"

if "$SCRIPT" --evidence-dir "$packet" --max-502-log-lines 0; then
  :
else
  printf 'zero max budget should be observe-only, not failure\n' >&2
  exit 1
fi

if "$SCRIPT" --evidence-dir "$packet" --max-502-log-lines 1; then
  :
else
  printf 'expected max-502-log-lines=1 to pass at exactly one observed line\n' >&2
  exit 1
fi

if "$SCRIPT" --evidence-dir "$packet" --max-502-log-lines 2; then
  :
else
  printf 'expected max-502-log-lines=2 to pass\n' >&2
  exit 1
fi

if "$SCRIPT" --evidence-dir "$packet" --max-tcfs-chunk-retry-rows 0; then
  :
else
  printf 'zero chunk retry budget should be observe-only, not failure\n' >&2
  exit 1
fi

if "$SCRIPT" --evidence-dir "$packet" --max-tcfs-chunk-retry-rows 1; then
  :
else
  printf 'expected max-tcfs-chunk-retry-rows=1 to pass at exactly one observed line\n' >&2
  exit 1
fi

if "$SCRIPT" --evidence-dir "$packet" --max-push-warn-rows 1 >"$TMPDIR/fail.log" 2>&1; then
  printf 'expected push warning budget failure\n' >&2
  exit 1
fi
assert_contains "$packet/storage-slo-summary.env" "status=failed"
assert_contains "$packet/storage-slo-summary.env" "push warning rows: observed 2 > budget 1"

if "$SCRIPT" --evidence-dir "$packet" --min-restore-throughput-bps 9000000 >"$TMPDIR/fail-throughput.log" 2>&1; then
  printf 'expected restore throughput budget failure\n' >&2
  exit 1
fi
assert_contains "$packet/storage-slo-summary.env" "restore throughput bytes/sec: observed 8738133 < budget 9000000"

failed_packet="$TMPDIR/failed-packet"
make_packet "$failed_packet"
sed -i.bak 's/status=passed/status=failed/' "$failed_packet/restore-proof/restore-proof.env"
if "$SCRIPT" --evidence-dir "$failed_packet" >"$TMPDIR/fail-status.log" 2>&1; then
  printf 'expected failed restore status to fail SLO evaluation\n' >&2
  exit 1
fi
assert_contains "$failed_packet/storage-slo-summary.env" "restore status: failed"

printf 'storage large restore SLO tests passed\n'
