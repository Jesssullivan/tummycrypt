#!/usr/bin/env bash
#
# Stage or run the TIN-1620 neo/honey flip-flop proof against a throwaway
# ~/git canary. Default mode is plan-only and does not touch TCFS, SSH, or the
# real canary path.
#
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/tin1620-flipflop-canary-harness.sh [options]

Create a ready-to-run evidence packet for the TIN-1620 daily-driver flip-flop:
neo pushes a canary under ~/git, neo unsyncs it, honey mutates the same fixture,
and neo rehydrates the exact honey bytes.

By default this is plan-only: it writes an evidence packet and run-later script,
but runs no tcfs, ssh, cargo, nix, or daemon commands.

Options:
  --execute
      Run readiness gates, then execute the flip-flop proof.
  --skip-readiness
      With --execute, skip host readiness gates. Use only for controlled tests.
  --remote <seaweedfs://host:port/bucket/prefix>
      Remote prefix. Defaults to a timestamped localhost prefix.
  --canary-root <path>
      Canary sync root. Defaults to "$HOME/git/tcfs-flipflop-canary".
  --evidence-dir <path>
      Evidence output directory. Defaults to docs/release/evidence/<run-id>.
  --state-dir <path>
      Local TCFS helper state directory passed to the lower-level harness.
  --tcfs-bin <path>
      tcfs binary for the lower-level harness.
  --honey-host <host>
      SSH host label for honey. Default: honey.
  --honey-mount-root <path>
      Honey mountpoint. Default: /tmp/tcfs-<run-id>-honey/mount.
  --honey-remote-dir <path>
      Honey temp directory. Default: /tmp/tcfs-<run-id>-honey/run.
  --honey-tcfs-bin <path>
      Remote tcfs binary on honey. Default: tcfs.
  --honey-start-mount
      Ask honey to start a temporary mount during the proof.
  --honey-existing-mount
      Assume --honey-mount-root is already mounted on honey.
  --create-bucket
      Best-effort create the S3 bucket before pushing.
  --forward-aws-env
      Forward current AWS env to honey for a temporary mount.
  --max-load <float>
      Maximum acceptable local 1-minute load before executing. Default: 12.0.
  --min-daemon-uptime-secs <int>
      Minimum tcfsd process age before executing. Default: 120.
  --status-timeout-secs <int>
      Timeout for "tcfs status" readiness probe. Default: 10.
  -h, --help
      Show this help.

Environment mirrors:
  TCFS_TIN1620_EXECUTE=1
  TCFS_TIN1620_SKIP_READINESS=1
  TCFS_TIN1620_REMOTE
  TCFS_TIN1620_CANARY_ROOT
  TCFS_TIN1620_EVIDENCE_DIR
  TCFS_TIN1620_STATE_DIR
  TCFS_TIN1620_MAX_LOAD
  TCFS_TIN1620_MIN_DAEMON_UPTIME_SECS
  TCFS_TIN1620_STATUS_TIMEOUT_SECS
  TCFS_TIN1620_STATUS_CMD
  TCFS_TIN1620_DEMO_SCRIPT
  TCFS_BIN
  HONEY_HOST
  HONEY_MOUNT_ROOT
  HONEY_REMOTE_DIR
  HONEY_TCFS_BIN
EOF
}

fail() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

bool_env() {
  local label="$1"
  local value="$2"

  case "$value" in
    1|true|yes|on) printf '1\n' ;;
    0|false|no|off|"") printf '0\n' ;;
    *) fail "$label must be 0/1, got: $value" ;;
  esac
}

shell_quote() {
  printf '%q' "$1"
}

expand_home() {
  local path="$1"

  case "$path" in
    ~) printf '%s\n' "$HOME" ;;
    ~/*) printf '%s/%s\n' "$HOME" "${path#~/}" ;;
    *) printf '%s\n' "$path" ;;
  esac
}

canonical_future_path() {
  local path="$1"
  local parent
  local base

  if [[ -e "$path" ]]; then
    (cd "$path" && pwd -P)
    return
  fi

  parent="$(dirname "$path")"
  base="$(basename "$path")"
  if [[ -d "$parent" ]]; then
    printf '%s/%s\n' "$(cd "$parent" && pwd -P)" "$base"
    return
  fi

  printf '%s\n' "$path"
}

float_le() {
  awk -v left="$1" -v right="$2" 'BEGIN { exit !(left <= right) }'
}

int_ge() {
  awk -v left="$1" -v right="$2" 'BEGIN { exit !(left >= right) }'
}

read_current_load() {
  if [[ -n "${TCFS_TIN1620_LOAD_1M:-}" ]]; then
    printf '%s\n' "$TCFS_TIN1620_LOAD_1M"
    return
  fi

  uptime | sed -E 's/.*load averages?:[[:space:]]*([0-9.]+).*/\1/'
}

read_daemon_uptime_secs() {
  if [[ -n "${TCFS_TIN1620_DAEMON_UPTIME_SECS:-}" ]]; then
    printf '%s\n' "$TCFS_TIN1620_DAEMON_UPTIME_SECS"
    return
  fi

  if ! command -v pgrep >/dev/null 2>&1; then
    printf '0\n'
    return
  fi

  local pids
  pids="$(pgrep -f 'tcfsd' 2>/dev/null || true)"
  if [[ -z "$pids" ]]; then
    printf '0\n'
    return
  fi

  local max_age=0
  local pid
  local age
  for pid in $pids; do
    age="$(ps -o etimes= -p "$pid" 2>/dev/null | tr -d '[:space:]' || true)"
    if [[ "$age" =~ ^[0-9]+$ && "$age" -gt "$max_age" ]]; then
      max_age="$age"
    fi
  done
  printf '%s\n' "$max_age"
}

run_shell_with_timeout() {
  local timeout_secs="$1"
  local out_path="$2"
  local err_path="$3"
  local command="$4"

  python3 - "$timeout_secs" "$out_path" "$err_path" "$command" <<'PY'
import subprocess
import sys

timeout_secs = float(sys.argv[1])
out_path = sys.argv[2]
err_path = sys.argv[3]
command = sys.argv[4]

try:
    result = subprocess.run(
        command,
        shell=True,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=timeout_secs,
    )
    with open(out_path, "w", encoding="utf-8") as out_file:
        out_file.write(result.stdout)
    with open(err_path, "w", encoding="utf-8") as err_file:
        err_file.write(result.stderr)
    sys.exit(result.returncode)
except subprocess.TimeoutExpired as exc:
    stdout = exc.stdout or ""
    stderr = exc.stderr or ""
    if isinstance(stdout, bytes):
        stdout = stdout.decode("utf-8", "replace")
    if isinstance(stderr, bytes):
        stderr = stderr.decode("utf-8", "replace")
    with open(out_path, "w", encoding="utf-8") as out_file:
        out_file.write(stdout)
    with open(err_path, "w", encoding="utf-8") as err_file:
        err_file.write(stderr)
        err_file.write(f"\ncommand timed out after {timeout_secs:g}s\n")
    sys.exit(124)
PY
}

status_has_storage_ready() {
  grep -Eiq 'storage(_ok)?[^[:alnum:]]+(true|ok)|storage.*\[(ok|OK)\]' "$1"
}

status_has_nats_ready() {
  grep -Eiq 'nats(_ok)?[^[:alnum:]]+(true|ok|connected)|nats.*connected|nats.*\[(ok|OK)\]' "$1"
}

write_readiness_result() {
  local path="$1"
  shift

  {
    printf 'created_at_utc=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    for row in "$@"; do
      printf '%s\n' "$row"
    done
  } >"$path"
}

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
timestamp="$(date -u +%Y%m%dT%H%M%SZ)"
run_id="tin1620-flipflop-canary-${timestamp}-$$"

execute="$(bool_env TCFS_TIN1620_EXECUTE "${TCFS_TIN1620_EXECUTE:-0}")"
skip_readiness="$(bool_env TCFS_TIN1620_SKIP_READINESS "${TCFS_TIN1620_SKIP_READINESS:-0}")"
remote="${TCFS_TIN1620_REMOTE:-seaweedfs://localhost:8333/tcfs/${run_id}}"
canary_root="$(expand_home "${TCFS_TIN1620_CANARY_ROOT:-$HOME/git/tcfs-flipflop-canary}")"
evidence_dir="${TCFS_TIN1620_EVIDENCE_DIR:-$REPO_ROOT/docs/release/evidence/${run_id}}"
state_dir="${TCFS_TIN1620_STATE_DIR:-}"
tcfs_bin="${TCFS_BIN:-}"
honey_host="${HONEY_HOST:-honey}"
honey_mount_root="${HONEY_MOUNT_ROOT:-/tmp/tcfs-${run_id}-honey/mount}"
honey_remote_dir="${HONEY_REMOTE_DIR:-/tmp/tcfs-${run_id}-honey/run}"
honey_tcfs_bin="${HONEY_TCFS_BIN:-tcfs}"
honey_start_mount="$(bool_env HONEY_START_MOUNT "${HONEY_START_MOUNT:-0}")"
honey_existing_mount="$(bool_env HONEY_EXISTING_MOUNT "${HONEY_EXISTING_MOUNT:-0}")"
create_bucket="$(bool_env CREATE_BUCKET "${CREATE_BUCKET:-0}")"
forward_aws_env="$(bool_env FORWARD_AWS_ENV "${FORWARD_AWS_ENV:-0}")"
max_load="${TCFS_TIN1620_MAX_LOAD:-12.0}"
min_daemon_uptime_secs="${TCFS_TIN1620_MIN_DAEMON_UPTIME_SECS:-120}"
status_timeout_secs="${TCFS_TIN1620_STATUS_TIMEOUT_SECS:-10}"
status_cmd="${TCFS_TIN1620_STATUS_CMD:-tcfs status}"
demo_script="${TCFS_TIN1620_DEMO_SCRIPT:-$REPO_ROOT/scripts/neo-honey-unsynced-rehydrate-demo.sh}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --execute)
      execute=1
      shift
      ;;
    --skip-readiness)
      skip_readiness=1
      shift
      ;;
    --remote)
      [[ $# -ge 2 ]] || fail "--remote requires a value"
      remote="$2"
      shift 2
      ;;
    --canary-root)
      [[ $# -ge 2 ]] || fail "--canary-root requires a value"
      canary_root="$(expand_home "$2")"
      shift 2
      ;;
    --evidence-dir)
      [[ $# -ge 2 ]] || fail "--evidence-dir requires a value"
      evidence_dir="$2"
      shift 2
      ;;
    --state-dir)
      [[ $# -ge 2 ]] || fail "--state-dir requires a value"
      state_dir="$2"
      shift 2
      ;;
    --tcfs-bin)
      [[ $# -ge 2 ]] || fail "--tcfs-bin requires a value"
      tcfs_bin="$2"
      shift 2
      ;;
    --honey-host)
      [[ $# -ge 2 ]] || fail "--honey-host requires a value"
      honey_host="$2"
      shift 2
      ;;
    --honey-mount-root)
      [[ $# -ge 2 ]] || fail "--honey-mount-root requires a value"
      honey_mount_root="$2"
      shift 2
      ;;
    --honey-remote-dir)
      [[ $# -ge 2 ]] || fail "--honey-remote-dir requires a value"
      honey_remote_dir="$2"
      shift 2
      ;;
    --honey-tcfs-bin)
      [[ $# -ge 2 ]] || fail "--honey-tcfs-bin requires a value"
      honey_tcfs_bin="$2"
      shift 2
      ;;
    --honey-start-mount)
      honey_start_mount=1
      honey_existing_mount=0
      shift
      ;;
    --honey-existing-mount)
      honey_existing_mount=1
      honey_start_mount=0
      shift
      ;;
    --create-bucket)
      create_bucket=1
      shift
      ;;
    --forward-aws-env)
      forward_aws_env=1
      shift
      ;;
    --max-load)
      [[ $# -ge 2 ]] || fail "--max-load requires a value"
      max_load="$2"
      shift 2
      ;;
    --min-daemon-uptime-secs)
      [[ $# -ge 2 ]] || fail "--min-daemon-uptime-secs requires a value"
      min_daemon_uptime_secs="$2"
      shift 2
      ;;
    --status-timeout-secs)
      [[ $# -ge 2 ]] || fail "--status-timeout-secs requires a value"
      status_timeout_secs="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      printf 'unknown argument: %s\n' "$1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

[[ "$remote" == seaweedfs://* ]] || fail "remote must start with seaweedfs://"
[[ "$max_load" =~ ^[0-9]+([.][0-9]+)?$ ]] || fail "--max-load must be a number"
[[ "$min_daemon_uptime_secs" =~ ^[0-9]+$ ]] || fail "--min-daemon-uptime-secs must be an integer"
[[ "$status_timeout_secs" =~ ^[0-9]+$ ]] || fail "--status-timeout-secs must be an integer"

canary_canon="$(canonical_future_path "$canary_root")"
home_canon="$(canonical_future_path "$HOME")"
git_canon="$(canonical_future_path "$HOME/git")"
[[ "$canary_canon" != "/" ]] || fail "refusing to use filesystem root as canary root"
[[ "$canary_canon" != "$home_canon" ]] || fail "refusing to use HOME as canary root"
[[ "$canary_canon" != "$git_canon" ]] || fail "refusing to use ~/git as canary root"

mkdir -p "$evidence_dir"
readiness_dir="$evidence_dir/readiness"
operator_script="$evidence_dir/run-when-ready.sh"
metadata_env="$evidence_dir/run-metadata.env"
result_env="$evidence_dir/result.env"
readme_path="$evidence_dir/README.md"

write_metadata() {
  cat >"$metadata_env" <<EOF
created_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
run_id=$run_id
remote=$remote
canary_root=$canary_canon
evidence_dir=$evidence_dir
state_dir=$state_dir
honey_host=$honey_host
honey_mount_root=$honey_mount_root
honey_remote_dir=$honey_remote_dir
honey_tcfs_bin=$honey_tcfs_bin
execute=$execute
skip_readiness=$skip_readiness
max_load=$max_load
min_daemon_uptime_secs=$min_daemon_uptime_secs
status_timeout_secs=$status_timeout_secs
status_cmd=$status_cmd
EOF
}

write_operator_script() {
  local args=(
    "$REPO_ROOT/scripts/tin1620-flipflop-canary-harness.sh"
    --execute
    --remote "$remote"
    --canary-root "$canary_canon"
    --evidence-dir "$evidence_dir"
    --honey-host "$honey_host"
    --honey-mount-root "$honey_mount_root"
    --honey-remote-dir "$honey_remote_dir"
    --honey-tcfs-bin "$honey_tcfs_bin"
    --max-load "$max_load"
    --min-daemon-uptime-secs "$min_daemon_uptime_secs"
    --status-timeout-secs "$status_timeout_secs"
  )
  if [[ -n "$state_dir" ]]; then
    args+=(--state-dir "$state_dir")
  fi
  if [[ -n "$tcfs_bin" ]]; then
    args+=(--tcfs-bin "$tcfs_bin")
  fi
  if [[ "$honey_start_mount" == "1" ]]; then
    args+=(--honey-start-mount)
  fi
  if [[ "$honey_existing_mount" == "1" ]]; then
    args+=(--honey-existing-mount)
  fi
  if [[ "$create_bucket" == "1" ]]; then
    args+=(--create-bucket)
  fi
  if [[ "$forward_aws_env" == "1" ]]; then
    args+=(--forward-aws-env)
  fi

  {
    printf '#!/usr/bin/env bash\n'
    printf 'set -euo pipefail\n'
    printf 'cd %s\n' "$(shell_quote "$REPO_ROOT")"
    printf 'exec'
    local arg
    for arg in "${args[@]}"; do
      printf ' %s' "$(shell_quote "$arg")"
    done
    printf '\n'
  } >"$operator_script"
  chmod +x "$operator_script"
}

write_readme() {
  cat >"$readme_path" <<EOF
# TIN-1620 Flip-Flop Canary Evidence

This packet stages the low-load TIN-1620 proof. It is intentionally separate
from live daemon rollout work.

Default behavior is plan-only:

- no tcfs command is executed;
- no ssh or scp command is executed;
- no cargo, nix, or daemon restart is attempted;
- the canary path is not created until the operator runs \`run-when-ready.sh\`.

Execute only when neo is quiet and the readiness gates pass:

- 1-minute load is at or below ${max_load};
- a tcfsd process has been stable for at least ${min_daemon_uptime_secs} seconds;
- \`${status_cmd}\` returns within ${status_timeout_secs} seconds;
- status output shows storage ready and NATS connected.

The executable proof delegates to
\`scripts/neo-honey-unsynced-rehydrate-demo.sh\` with:

- canary root: \`${canary_canon}\`;
- remote: \`${remote}\`;
- honey host: \`${honey_host}\`;
- evidence dir: \`${evidence_dir}\`.

Run:

\`\`\`bash
${operator_script}
\`\`\`
EOF
}

run_readiness_gates() {
  mkdir -p "$readiness_dir"

  local load_1m
  local daemon_uptime_secs
  local status_out="$readiness_dir/tcfs-status.out"
  local status_err="$readiness_dir/tcfs-status.err"
  local status_exit
  local storage_ready=0
  local nats_ready=0
  local load_ready=0
  local daemon_ready=0
  local failures=()

  load_1m="$(read_current_load)"
  if [[ "$load_1m" =~ ^[0-9]+([.][0-9]+)?$ ]] && float_le "$load_1m" "$max_load"; then
    load_ready=1
  else
    failures+=("host load too high: load_1m=$load_1m max_load=$max_load")
  fi

  daemon_uptime_secs="$(read_daemon_uptime_secs)"
  if [[ "$daemon_uptime_secs" =~ ^[0-9]+$ ]] && int_ge "$daemon_uptime_secs" "$min_daemon_uptime_secs"; then
    daemon_ready=1
  else
    failures+=("tcfsd not stable long enough: daemon_uptime_secs=$daemon_uptime_secs min=$min_daemon_uptime_secs")
  fi

  if run_shell_with_timeout "$status_timeout_secs" "$status_out" "$status_err" "$status_cmd"; then
    status_exit=0
  else
    status_exit=$?
    failures+=("tcfs status probe failed: exit=$status_exit")
  fi

  if [[ -f "$status_out" ]] && status_has_storage_ready "$status_out"; then
    storage_ready=1
  else
    failures+=("tcfs status did not show storage ready")
  fi

  if [[ -f "$status_out" ]] && status_has_nats_ready "$status_out"; then
    nats_ready=1
  else
    failures+=("tcfs status did not show nats connected")
  fi

  write_readiness_result "$readiness_dir/readiness.env" \
    "load_1m=$load_1m" \
    "max_load=$max_load" \
    "load_ready=$load_ready" \
    "daemon_uptime_secs=$daemon_uptime_secs" \
    "min_daemon_uptime_secs=$min_daemon_uptime_secs" \
    "daemon_ready=$daemon_ready" \
    "status_cmd=$status_cmd" \
    "status_timeout_secs=$status_timeout_secs" \
    "status_exit=$status_exit" \
    "storage_ready=$storage_ready" \
    "nats_ready=$nats_ready"

  if [[ "${#failures[@]}" -gt 0 ]]; then
    {
      printf 'status=blocked-host-readiness\n'
      printf 'proof=pending-neo-quiet\n'
      printf 'failure_count=%s\n' "${#failures[@]}"
      local failure
      local index=1
      for failure in "${failures[@]}"; do
        printf 'failure_%s=%s\n' "$index" "$failure"
        index=$((index + 1))
      done
    } >"$result_env"
    printf 'readiness blocked TIN-1620 flip-flop:\n' >&2
    printf '  %s\n' "${failures[@]}" >&2
    return 1
  fi
}

write_metadata
write_operator_script
write_readme

if [[ "$execute" != "1" ]]; then
  {
    printf 'status=plan-only\n'
    printf 'proof=pending-host-readiness\n'
    printf 'canary_created=0\n'
  } >"$result_env"
  printf 'plan-only: no TCFS or SSH commands were run\n'
  printf 'evidence: %s\n' "$evidence_dir"
  printf 'run later: %s\n' "$operator_script"
  exit 0
fi

if [[ "$skip_readiness" != "1" ]]; then
  run_readiness_gates
fi

[[ -x "$demo_script" ]] || fail "demo script is not executable: $demo_script"

delegate_args=(
  --remote "$remote"
  --neo-root "$canary_canon"
  --evidence-dir "$evidence_dir"
  --honey-host "$honey_host"
  --honey-mount-root "$honey_mount_root"
  --honey-remote-dir "$honey_remote_dir"
  --honey-tcfs-bin "$honey_tcfs_bin"
  --push
  --run-honey
)
if [[ -n "$state_dir" ]]; then
  delegate_args+=(--state-dir "$state_dir")
fi
if [[ -n "$tcfs_bin" ]]; then
  delegate_args+=(--tcfs-bin "$tcfs_bin")
fi
if [[ "$honey_start_mount" == "1" ]]; then
  delegate_args+=(--honey-start-mount)
fi
if [[ "$honey_existing_mount" == "1" ]]; then
  delegate_args+=(--honey-existing-mount)
fi
if [[ "$create_bucket" == "1" ]]; then
  delegate_args+=(--create-bucket)
fi
if [[ "$forward_aws_env" == "1" ]]; then
  delegate_args+=(--forward-aws-env)
fi

"$demo_script" "${delegate_args[@]}"
