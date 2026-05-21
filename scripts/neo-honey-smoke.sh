#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUN_STAMP="$(date -u '+%Y%m%dT%H%M%SZ')"
LOG_DIR="${TCFS_NEO_HONEY_LOG_DIR:-}"
RESULT_WRITTEN=0

usage() {
  cat <<'USAGE'
Usage: scripts/neo-honey-smoke.sh [options]

Run the named neo/honey live fleet acceptance lane and write an evidence packet.

Options:
  --log-dir <dir>  Evidence directory. Default:
                   docs/release/evidence/neo-honey-smoke-<UTC timestamp>
  -h, --help       Show this help text
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --log-dir)
      [[ $# -ge 2 ]] || {
        echo "--log-dir requires a directory" >&2
        exit 2
      }
      LOG_DIR="$2"
      shift 2
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ -z "$LOG_DIR" ]]; then
  LOG_DIR="$ROOT/docs/release/evidence/neo-honey-smoke-${RUN_STAMP}"
fi
mkdir -p "$LOG_DIR"
LOG_DIR="$(cd "$LOG_DIR" && pwd)"
RESULT_ENV="$LOG_DIR/result.env"
RUN_METADATA="$LOG_DIR/run-metadata.env"

redact_url() {
  local value="$1"
  python3 - "$value" <<'PY'
import sys
import urllib.parse

raw = sys.argv[1]
parsed = urllib.parse.urlparse(raw)
if not parsed.scheme:
    print(raw)
    raise SystemExit
netloc = parsed.hostname or ""
if parsed.port:
    netloc = f"{netloc}:{parsed.port}"
print(urllib.parse.urlunparse((parsed.scheme, netloc, parsed.path, "", "", "")))
PY
}

write_result() {
  local status="$1"
  local proof="$2"
  local failed_step="${3:-}"

  {
    echo "status=$status"
    echo "proof=$proof"
    echo "failed_step=$failed_step"
    echo "started_at=$RUN_STAMP"
    echo "completed_at=$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
    echo "log_dir=$LOG_DIR"
  } >"$RESULT_ENV"
  RESULT_WRITTEN=1
}

write_metadata() {
  local git_sha git_branch endpoint_redacted nats_redacted
  git_sha="$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || true)"
  git_branch="$(git -C "$ROOT" branch --show-current 2>/dev/null || true)"
  endpoint_redacted="$(redact_url "${TCFS_S3_ENDPOINT:-}")"
  nats_redacted="$(redact_url "${TCFS_NATS_URL:-}")"

  {
    echo "run_stamp=$RUN_STAMP"
    echo "repo=$ROOT"
    echo "git_sha=$git_sha"
    echo "git_branch=$git_branch"
    echo "tcfs_e2e_live=${TCFS_E2E_LIVE:-}"
    echo "s3_endpoint=$endpoint_redacted"
    echo "s3_bucket=${TCFS_S3_BUCKET:-}"
    echo "nats_url=$nats_redacted"
    echo "log_dir=$LOG_DIR"
  } >"$RUN_METADATA"
}

fail_with_result() {
  local status="$1"
  local proof="$2"
  local failed_step="$3"
  shift 3
  echo "$*" >&2
  write_metadata
  write_result "$status" "$proof" "$failed_step"
  exit "$status"
}

require_env() {
  local name="$1"
  if [[ -z "${!name:-}" ]]; then
    fail_with_result 2 "blocked-missing-env" "env:$name" "missing required env: $name"
  fi
}

run_cargo_test() {
  local label="$1"
  local test_name="$2"
  local log_file="$LOG_DIR/${label}.log"

  echo "==> proving ${label//-/ }"
  if cargo test -p tcfs-e2e --test fleet_live "$test_name" -- --nocapture \
    >"$log_file" 2>&1; then
    cat "$log_file"
  else
    local status=$?
    cat "$log_file" >&2 || true
    write_result "$status" "failed" "$label"
    exit "$status"
  fi
}

on_exit() {
  local rc=$?
  if [[ "$rc" != "0" && "$RESULT_WRITTEN" == "0" ]]; then
    write_result "$rc" "failed" "unexpected"
  fi
}
trap on_exit EXIT

cd "$ROOT"

echo "==> neo-honey live acceptance smoke"
echo "repo: $ROOT"
echo "evidence: $LOG_DIR"

require_env TCFS_E2E_LIVE
require_env TCFS_S3_ENDPOINT
require_env TCFS_S3_BUCKET
require_env AWS_ACCESS_KEY_ID
require_env AWS_SECRET_ACCESS_KEY
require_env TCFS_NATS_URL

if [[ "${TCFS_E2E_LIVE}" != "1" ]]; then
  fail_with_result 2 "blocked-live-disabled" "env:TCFS_E2E_LIVE" "TCFS_E2E_LIVE must be set to 1"
fi

write_metadata

run_cargo_test "seaweedfs-health" "seaweedfs_health_check"
run_cargo_test "nats-jetstream" "nats_connect_and_jetstream"
run_cargo_test "neo-honey-two-device-sync" "neo_honey_two_device_sync_smoke"

write_result 0 "neo-honey-live-fleet-acceptance" ""
echo "==> neo-honey smoke passed"
echo "result: $RESULT_ENV"
