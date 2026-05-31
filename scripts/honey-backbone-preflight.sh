#!/usr/bin/env bash
# shellcheck disable=SC2016 # Markdown backticks are literal in printf strings.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUN_STAMP="$(date -u '+%Y%m%dT%H%M%SZ')"
LOG_DIR="${TCFS_HONEY_BACKBONE_LOG_DIR:-}"
HONEY_HOST="${TCFS_HONEY_HOST:-honey}"
STRICT=0
NATS_TARGETS=("nats-tcfs" "10.245.131.232")

usage() {
  cat <<'USAGE'
Usage: scripts/honey-backbone-preflight.sh [options]

Run a read-only G2/G3 preflight for the neo/honey TCFS backbone. The helper
captures daemon status, device registries, redacted configs, and NATS endpoint
reachability from both hosts. It does not enroll devices, edit configs, restart
services, or copy user data.

Options:
  --honey-host <host>       SSH host for honey (default: honey)
  --nats-target <host>      Add a NATS host to probe on :4222 and :8222.
                            Can be repeated. Defaults: nats-tcfs, 10.245.131.232
  --clear-nats-targets      Clear default NATS targets before adding explicit ones
  --log-dir <dir>           Evidence directory. Default:
                            docs/release/evidence/honey-backbone-preflight-<UTC>
  --strict                  Exit non-zero when G2/G3 blockers are present
  -h, --help                Show this help
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 2
}

require_value() {
  local flag="$1"
  local value="${2:-}"
  [[ -n "$value" ]] || die "$flag requires a value"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --honey-host)
      require_value "$1" "${2:-}"
      HONEY_HOST="$2"
      shift 2
      ;;
    --nats-target)
      require_value "$1" "${2:-}"
      NATS_TARGETS+=("$2")
      shift 2
      ;;
    --clear-nats-targets)
      NATS_TARGETS=()
      shift
      ;;
    --log-dir)
      require_value "$1" "${2:-}"
      LOG_DIR="$2"
      shift 2
      ;;
    --strict)
      STRICT=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      die "unknown argument: $1"
      ;;
  esac
done

if [[ -z "$LOG_DIR" ]]; then
  LOG_DIR="$ROOT/docs/release/evidence/honey-backbone-preflight-${RUN_STAMP}"
fi
mkdir -p "$LOG_DIR"
LOG_DIR="$(cd "$LOG_DIR" && pwd)"

RESULT_ENV="$LOG_DIR/result.env"
SUMMARY_MD="$LOG_DIR/summary.md"
NATS_TSV="$LOG_DIR/nats-probes.tsv"

redact_config() {
  sed -E 's/(secret|key|token|password|access)[[:space:]]*=.*/\1 = <redacted>/I'
}

run_capture() {
  local label="$1"
  shift
  local out="$LOG_DIR/${label}.out"
  {
    printf '$'
    printf ' %q' "$@"
    printf '\n'
    "$@"
  } >"$out" 2>&1 || true
}

run_ssh_capture() {
  local label="$1"
  shift
  local out="$LOG_DIR/${label}.out"
  {
    printf '$ ssh %q' "$HONEY_HOST"
    printf ' %q' "$@"
    printf '\n'
    ssh -o BatchMode=yes -o ConnectTimeout=5 "$HONEY_HOST" "$@"
  } >"$out" 2>&1 || true
}

status_has_storage_ok() {
  grep -Eq 'storage:[[:space:]].*\[ok\]' "$1"
}

status_has_nats_connected() {
  grep -Eq 'nats:[[:space:]]+connected' "$1"
}

device_list_has_name() {
  local file="$1"
  local name="$2"
  grep -Eq "(^|[[:space:]])${name}([[:space:]]|\\[|$)" "$file"
}

public_key_is_placeholder() {
  local file="$1"
  grep -Eq 'public_key:[[:space:]]+age1-device-' "$file"
}

tcfs_status_device_name() {
  awk -F ': ' '/device:/ {print $2; exit}' "$1" | awk '{print $1}'
}

probe_python() {
  local host="$1"
  python3 - "$host" <<'PY'
import socket
import sys

host = sys.argv[1]
try:
    sock = socket.create_connection((host, 4222), timeout=3)
    sock.settimeout(3)
    data = sock.recv(512).decode("utf-8", "replace").strip()
    sock.close()
    print("tcp=ok")
    if data.startswith("INFO "):
        print("nats_info=ok")
        print(data[:240].replace("\t", " "))
    else:
        print("nats_info=unexpected")
        print(data[:240].replace("\t", " "))
except Exception as exc:
    print("tcp=fail")
    print(f"nats_info={type(exc).__name__}:{exc}")
PY
}

probe_http_health() {
  local host="$1"
  curl -fsS --max-time 3 "http://${host}:8222/healthz" 2>&1 || true
}

probe_remote_python() {
  local host="$1"
  ssh -o BatchMode=yes -o ConnectTimeout=5 "$HONEY_HOST" python3 - "$host" <<'PY'
import socket
import sys

host = sys.argv[1]
try:
    sock = socket.create_connection((host, 4222), timeout=3)
    sock.settimeout(3)
    data = sock.recv(512).decode("utf-8", "replace").strip()
    sock.close()
    print("tcp=ok")
    if data.startswith("INFO "):
        print("nats_info=ok")
        print(data[:240].replace("\t", " "))
    else:
        print("nats_info=unexpected")
        print(data[:240].replace("\t", " "))
except Exception as exc:
    print("tcp=fail")
    print(f"nats_info={type(exc).__name__}:{exc}")
PY
}

probe_remote_http_health() {
  local host="$1"
  ssh -o BatchMode=yes -o ConnectTimeout=5 "$HONEY_HOST" \
    curl -fsS --max-time 3 "http://${host}:8222/healthz" 2>&1 || true
}

write_probe_row() {
  local side="$1"
  local host="$2"
  local health="$3"
  local probe="$4"
  local tcp nats_info
  tcp="$(awk -F= '/^tcp=/ {print $2; exit}' <<<"$probe")"
  nats_info="$(awk -F= '/^nats_info=/ {print $2; exit}' <<<"$probe")"
  printf '%s\t%s\t%s\t%s\t%s\n' "$side" "$host" "${health//$'\n'/ }" "${tcp:-unknown}" "${nats_info:-unknown}" >>"$NATS_TSV"
}

write_result() {
  local status="$1"
  local proof="$2"
  local failed_step="$3"
  {
    printf 'status=%s\n' "$status"
    printf 'proof=%s\n' "$proof"
    printf 'failed_step=%s\n' "$failed_step"
    printf 'started_at=%s\n' "$RUN_STAMP"
    printf 'completed_at=%s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
    printf 'honey_host=%s\n' "$HONEY_HOST"
    printf 'log_dir=%s\n' "$LOG_DIR"
  } >"$RESULT_ENV"
}

cd "$ROOT"

run_capture local-tcfs-status tcfs status
run_capture local-device-status tcfs device status
run_capture local-device-list tcfs device list
if [[ -f "$HOME/.config/tcfs/config.toml" ]]; then
  redact_config <"$HOME/.config/tcfs/config.toml" >"$LOG_DIR/local-config.redacted.toml"
fi

run_ssh_capture honey-hostname hostname
run_ssh_capture honey-tcfs-status tcfs status
run_ssh_capture honey-device-status tcfs device status
run_ssh_capture honey-device-list tcfs device list
ssh -o BatchMode=yes -o ConnectTimeout=5 "$HONEY_HOST" \
  'if [ -f "$HOME/.config/tcfs/config.toml" ]; then cat "$HOME/.config/tcfs/config.toml"; fi' \
  2>/dev/null | redact_config >"$LOG_DIR/honey-config.redacted.toml" || true

printf 'side\thost\thealthz\ttcp\tnats_info\n' >"$NATS_TSV"
for target in "${NATS_TARGETS[@]}"; do
  [[ -n "$target" ]] || continue
  local_health="$(probe_http_health "$target")"
  local_probe="$(probe_python "$target")"
  write_probe_row local "$target" "$local_health" "$local_probe"
  remote_health="$(probe_remote_http_health "$target")"
  remote_probe="$(probe_remote_python "$target")"
  write_probe_row honey "$target" "$remote_health" "$remote_probe"
done

local_status="$LOG_DIR/local-tcfs-status.out"
honey_status="$LOG_DIR/honey-tcfs-status.out"
local_devices="$LOG_DIR/local-device-list.out"
honey_devices="$LOG_DIR/honey-device-list.out"
honey_device_status="$LOG_DIR/honey-device-status.out"

blockers=()
status="0"
proof="honey-backbone-preflight-complete"
failed_step=""

status_has_storage_ok "$local_status" || blockers+=("neo storage is not OK")
status_has_nats_connected "$local_status" || blockers+=("neo NATS is not connected")
status_has_storage_ok "$honey_status" || blockers+=("honey storage is not OK")
status_has_nats_connected "$honey_status" || blockers+=("honey NATS is not connected")
device_list_has_name "$local_devices" honey || blockers+=("neo device registry does not include honey")
device_list_has_name "$honey_devices" neo || blockers+=("honey device registry does not include neo")
if public_key_is_placeholder "$honey_device_status"; then
  blockers+=("honey device public key is placeholder-shaped")
fi

if (( ${#blockers[@]} > 0 )); then
  proof="blocked-g2-g3"
  failed_step="honey-backbone"
  if (( STRICT == 1 )); then
    status="1"
  fi
fi

{
  printf '# TCFS Honey Backbone Preflight - %s\n\n' "$RUN_STAMP"
  printf 'Status: `%s`\n\n' "$proof"
  printf 'Host under test: `%s`\n\n' "$HONEY_HOST"
  printf '## Daemon Gate\n\n'
  printf '| Host | Storage OK | NATS Connected | Device |\n'
  printf '| --- | --- | --- | --- |\n'
  printf '| neo | %s | %s | `%s` |\n' \
    "$(status_has_storage_ok "$local_status" && printf yes || printf no)" \
    "$(status_has_nats_connected "$local_status" && printf yes || printf no)" \
    "$(tcfs_status_device_name "$local_status")"
  printf '| honey | %s | %s | `%s` |\n\n' \
    "$(status_has_storage_ok "$honey_status" && printf yes || printf no)" \
    "$(status_has_nats_connected "$honey_status" && printf yes || printf no)" \
    "$(tcfs_status_device_name "$honey_status")"
  printf '## Registry Gate\n\n'
  printf '| Check | Result |\n'
  printf '| --- | --- |\n'
  printf '| neo registry includes honey | %s |\n' "$(device_list_has_name "$local_devices" honey && printf yes || printf no)"
  printf '| honey registry includes neo | %s |\n' "$(device_list_has_name "$honey_devices" neo && printf yes || printf no)"
  printf '| honey public key placeholder-shaped | %s |\n\n' "$(public_key_is_placeholder "$honey_device_status" && printf yes || printf no)"
  printf '## NATS Endpoint Probes\n\n'
  printf 'See `nats-probes.tsv` for raw read-only probes from both hosts.\n\n'
  if (( ${#blockers[@]} > 0 )); then
    printf '## Blockers\n\n'
    blocker=""
    for blocker in "${blockers[@]}"; do
      printf -- '- %s\n' "$blocker"
    done
    printf '\n'
  else
    printf '## Blockers\n\nNone.\n\n'
  fi
  printf '## Claim Boundary\n\n'
  printf 'This is a read-only preflight. It does not enroll honey, change NATS or S3 endpoints, restart daemons, or move data.\n'
} >"$SUMMARY_MD"

write_result "$status" "$proof" "$failed_step"
cat "$SUMMARY_MD"
exit "$status"
