#!/usr/bin/env bash
#
# Evaluate the storage large-restore packet against optional beta SLO budgets.
#
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/evaluate-storage-large-restore-slo.sh --evidence-dir <path> [budgets]

Reads a storage-large-restore evidence packet and writes:

  storage-slo-summary.env
  storage-slo-summary.md

Budgets default to 0, which means "observe only" for that metric. A non-zero
max budget fails when the observed value is greater than the budget. A non-zero
min budget fails when the observed value is lower than the budget.

Options:
  --evidence-dir <path>
  --min-restore-throughput-bps <n>
  --max-restore-elapsed-secs <n>
  --max-502-log-lines <n>
  --max-opendal-retry-rows <n>
  --max-tcfs-chunk-retry-rows <n>
  --max-push-warn-rows <n>
  --max-push-error-rows <n>
  --max-socket-highwater <n>
  -h, --help

Environment mirrors:
  EVIDENCE_DIR
  MIN_RESTORE_THROUGHPUT_BPS
  MAX_RESTORE_ELAPSED_SECS
  MAX_502_LOG_LINES
  MAX_OPENDAL_RETRY_ROWS
  MAX_TCFS_CHUNK_RETRY_ROWS
  MAX_PUSH_WARN_ROWS
  MAX_PUSH_ERROR_ROWS
  MAX_SOCKET_HIGHWATER
EOF
}

fail() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

uint_value() {
  local label="$1"
  local value="$2"

  case "$value" in
    ""|*[!0-9]*) fail "$label must be a non-negative integer, got: $value" ;;
    *) printf '%s\n' "$value" ;;
  esac
}

read_kv() {
  local file="$1"
  local key="$2"

  [[ -f "$file" ]] || return 1
  awk -F= -v key="$key" '
    $1 == key {
      print substr($0, length($1) + 2)
      found = 1
      exit
    }
    END { exit found ? 0 : 1 }
  ' "$file"
}

read_uint_kv_or_zero() {
  local file="$1"
  local key="$2"
  local value

  value="$(read_kv "$file" "$key" || true)"
  if [[ -z "$value" || "$value" == *[!0-9]* ]]; then
    printf '0\n'
  else
    printf '%s\n' "$value"
  fi
}

count_log_rows() {
  local file="$1"
  local pattern="$2"

  if [[ ! -f "$file" ]]; then
    printf '0\n'
    return
  fi

  grep -Eci -- "$pattern" "$file" || true
}

check_max_budget() {
  local label="$1"
  local observed="$2"
  local budget="$3"

  if (( budget > 0 && observed > budget )); then
    budget_failures+=("${label}: observed ${observed} > budget ${budget}")
  fi
}

check_min_budget() {
  local label="$1"
  local observed="$2"
  local budget="$3"

  if (( budget > 0 && observed < budget )); then
    budget_failures+=("${label}: observed ${observed} < budget ${budget}")
  fi
}

evidence_dir="${EVIDENCE_DIR:-}"
min_restore_throughput_bps="${MIN_RESTORE_THROUGHPUT_BPS:-0}"
max_restore_elapsed_secs="${MAX_RESTORE_ELAPSED_SECS:-0}"
max_502_log_lines="${MAX_502_LOG_LINES:-0}"
max_opendal_retry_rows="${MAX_OPENDAL_RETRY_ROWS:-0}"
max_tcfs_chunk_retry_rows="${MAX_TCFS_CHUNK_RETRY_ROWS:-0}"
max_push_warn_rows="${MAX_PUSH_WARN_ROWS:-0}"
max_push_error_rows="${MAX_PUSH_ERROR_ROWS:-0}"
max_socket_highwater="${MAX_SOCKET_HIGHWATER:-0}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --evidence-dir)
      [[ $# -ge 2 ]] || fail "--evidence-dir requires a value"
      evidence_dir="$2"
      shift 2
      ;;
    --min-restore-throughput-bps)
      [[ $# -ge 2 ]] || fail "--min-restore-throughput-bps requires a value"
      min_restore_throughput_bps="$2"
      shift 2
      ;;
    --max-restore-elapsed-secs)
      [[ $# -ge 2 ]] || fail "--max-restore-elapsed-secs requires a value"
      max_restore_elapsed_secs="$2"
      shift 2
      ;;
    --max-502-log-lines)
      [[ $# -ge 2 ]] || fail "--max-502-log-lines requires a value"
      max_502_log_lines="$2"
      shift 2
      ;;
    --max-opendal-retry-rows)
      [[ $# -ge 2 ]] || fail "--max-opendal-retry-rows requires a value"
      max_opendal_retry_rows="$2"
      shift 2
      ;;
    --max-tcfs-chunk-retry-rows)
      [[ $# -ge 2 ]] || fail "--max-tcfs-chunk-retry-rows requires a value"
      max_tcfs_chunk_retry_rows="$2"
      shift 2
      ;;
    --max-push-warn-rows)
      [[ $# -ge 2 ]] || fail "--max-push-warn-rows requires a value"
      max_push_warn_rows="$2"
      shift 2
      ;;
    --max-push-error-rows)
      [[ $# -ge 2 ]] || fail "--max-push-error-rows requires a value"
      max_push_error_rows="$2"
      shift 2
      ;;
    --max-socket-highwater)
      [[ $# -ge 2 ]] || fail "--max-socket-highwater requires a value"
      max_socket_highwater="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      fail "unknown argument: $1"
      ;;
  esac
done

[[ -n "$evidence_dir" ]] || fail "--evidence-dir is required"
[[ -d "$evidence_dir" ]] || fail "evidence dir does not exist: $evidence_dir"

min_restore_throughput_bps="$(uint_value min_restore_throughput_bps "$min_restore_throughput_bps")"
max_restore_elapsed_secs="$(uint_value max_restore_elapsed_secs "$max_restore_elapsed_secs")"
max_502_log_lines="$(uint_value max_502_log_lines "$max_502_log_lines")"
max_opendal_retry_rows="$(uint_value max_opendal_retry_rows "$max_opendal_retry_rows")"
max_tcfs_chunk_retry_rows="$(uint_value max_tcfs_chunk_retry_rows "$max_tcfs_chunk_retry_rows")"
max_push_warn_rows="$(uint_value max_push_warn_rows "$max_push_warn_rows")"
max_push_error_rows="$(uint_value max_push_error_rows "$max_push_error_rows")"
max_socket_highwater="$(uint_value max_socket_highwater "$max_socket_highwater")"

restore_env="$evidence_dir/restore-proof/restore-proof.env"
restore_log="$evidence_dir/restore-proof/reconcile-execute.log"
push_summary="$evidence_dir/push-storage-summary.env"
socket_summary="$evidence_dir/s3-socket-summary.env"
slo_env="$evidence_dir/storage-slo-summary.env"
slo_md="$evidence_dir/storage-slo-summary.md"

[[ -s "$restore_env" ]] || fail "missing restore proof env: $restore_env"

restore_status="$(read_kv "$restore_env" status || true)"
restore_reason="$(read_kv "$restore_env" reason || true)"
execute_elapsed_secs="$(read_uint_kv_or_zero "$restore_env" execute_elapsed_secs)"
restored_regular_file_bytes="$(read_uint_kv_or_zero "$restore_env" restored_regular_file_bytes)"
restored_regular_file_bytes_per_sec="$(read_uint_kv_or_zero "$restore_env" restored_regular_file_bytes_per_sec)"
download_chunk_retries="$(read_kv "$restore_env" download_chunk_retries || true)"

push_warn_rows="$(read_uint_kv_or_zero "$push_summary" warn_rows)"
push_retry_warning_rows="$(read_uint_kv_or_zero "$push_summary" retry_warning_rows)"
push_error_rows="$(read_uint_kv_or_zero "$push_summary" error_rows)"
socket_highwater="$(read_uint_kv_or_zero "$socket_summary" socket_highwater)"

http_502_log_lines="$(count_log_rows "$restore_log" 'error code:[[:space:]]*502|(^|[^0-9])502([^0-9]|$)')"
opendal_retry_rows="$(count_log_rows "$restore_log" 'opendal.*retry|Retryable|retrying after|backoff')"
tcfs_chunk_retry_rows="$(count_log_rows "$restore_log" 'chunk download (failed|timed out|integrity mismatch), retrying')"

budget_failures=()
if [[ "$restore_status" != "passed" ]]; then
  budget_failures+=("restore status: ${restore_status:-missing} (${restore_reason:-no reason})")
fi

check_min_budget "restore throughput bytes/sec" "$restored_regular_file_bytes_per_sec" "$min_restore_throughput_bps"
check_max_budget "restore elapsed seconds" "$execute_elapsed_secs" "$max_restore_elapsed_secs"
check_max_budget "HTTP 502 log lines" "$http_502_log_lines" "$max_502_log_lines"
check_max_budget "OpenDAL retry rows" "$opendal_retry_rows" "$max_opendal_retry_rows"
check_max_budget "TCFS chunk retry rows" "$tcfs_chunk_retry_rows" "$max_tcfs_chunk_retry_rows"
check_max_budget "push warning rows" "$push_warn_rows" "$max_push_warn_rows"
check_max_budget "push error rows" "$push_error_rows" "$max_push_error_rows"
check_max_budget "S3 socket highwater" "$socket_highwater" "$max_socket_highwater"

slo_status=passed
if (( ${#budget_failures[@]} > 0 )); then
  slo_status=failed
fi

{
  printf 'created_at_utc=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  printf 'status=%s\n' "$slo_status"
  printf 'restore_status=%s\n' "${restore_status:-missing}"
  printf 'restore_reason=%s\n' "$restore_reason"
  printf 'restored_regular_file_bytes=%s\n' "$restored_regular_file_bytes"
  printf 'execute_elapsed_secs=%s\n' "$execute_elapsed_secs"
  printf 'restored_regular_file_bytes_per_sec=%s\n' "$restored_regular_file_bytes_per_sec"
  printf 'download_chunk_retries=%s\n' "$download_chunk_retries"
  printf 'http_502_log_lines=%s\n' "$http_502_log_lines"
  printf 'opendal_retry_rows=%s\n' "$opendal_retry_rows"
  printf 'tcfs_chunk_retry_rows=%s\n' "$tcfs_chunk_retry_rows"
  printf 'push_warn_rows=%s\n' "$push_warn_rows"
  printf 'push_retry_warning_rows=%s\n' "$push_retry_warning_rows"
  printf 'push_error_rows=%s\n' "$push_error_rows"
  printf 'socket_highwater=%s\n' "$socket_highwater"
  printf 'min_restore_throughput_bps=%s\n' "$min_restore_throughput_bps"
  printf 'max_restore_elapsed_secs=%s\n' "$max_restore_elapsed_secs"
  printf 'max_502_log_lines=%s\n' "$max_502_log_lines"
  printf 'max_opendal_retry_rows=%s\n' "$max_opendal_retry_rows"
  printf 'max_tcfs_chunk_retry_rows=%s\n' "$max_tcfs_chunk_retry_rows"
  printf 'max_push_warn_rows=%s\n' "$max_push_warn_rows"
  printf 'max_push_error_rows=%s\n' "$max_push_error_rows"
  printf 'max_socket_highwater=%s\n' "$max_socket_highwater"
  printf 'budget_failure_count=%s\n' "${#budget_failures[@]}"
  if (( ${#budget_failures[@]} > 0 )); then
    printf 'budget_failures=%s\n' "$(IFS='; '; printf '%s' "${budget_failures[*]}")"
  else
    printf 'budget_failures=\n'
  fi
} >"$slo_env"

{
  echo "# Storage Large Restore SLO Evaluation"
  echo
  echo "- Status: \`$slo_status\`"
  echo "- Restore status: \`${restore_status:-missing}\`"
  echo "- Restore reason: \`$restore_reason\`"
  echo "- Restored bytes: \`$restored_regular_file_bytes\`"
  echo "- Restore elapsed seconds: \`$execute_elapsed_secs\`"
  echo "- Restore throughput: \`${restored_regular_file_bytes_per_sec} bytes/s\`"
  echo "- HTTP 502 log lines: \`$http_502_log_lines\`"
  echo "- OpenDAL retry rows: \`$opendal_retry_rows\`"
  echo "- TCFS chunk retry rows: \`$tcfs_chunk_retry_rows\`"
  echo "- Push warning rows: \`$push_warn_rows\`"
  echo "- Push retry warning rows: \`$push_retry_warning_rows\`"
  echo "- Push error rows: \`$push_error_rows\`"
  echo "- S3 socket highwater: \`$socket_highwater\`"
  echo
  echo "## Budgets"
  echo
  echo "- Minimum restore throughput: \`${min_restore_throughput_bps} bytes/s\`"
  echo "- Maximum restore elapsed: \`${max_restore_elapsed_secs}s\`"
  echo "- Maximum HTTP 502 log lines: \`$max_502_log_lines\`"
  echo "- Maximum OpenDAL retry rows: \`$max_opendal_retry_rows\`"
  echo "- Maximum TCFS chunk retry rows: \`$max_tcfs_chunk_retry_rows\`"
  echo "- Maximum push warning rows: \`$max_push_warn_rows\`"
  echo "- Maximum push error rows: \`$max_push_error_rows\`"
  echo "- Maximum S3 socket highwater: \`$max_socket_highwater\`"
  if (( ${#budget_failures[@]} > 0 )); then
    echo
    echo "## Failures"
    echo
    for failure in "${budget_failures[@]}"; do
      echo "- $failure"
    done
  fi
} >"$slo_md"

printf 'storage SLO evidence: %s\n' "$slo_env"
printf 'storage SLO status: %s\n' "$slo_status"

if [[ "$slo_status" != "passed" ]]; then
  exit 1
fi
