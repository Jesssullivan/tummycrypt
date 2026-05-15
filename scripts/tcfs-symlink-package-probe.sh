#!/usr/bin/env bash
#
# Probe one or more tcfs binaries for sync_symlinks behavior.
#
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/tcfs-symlink-package-probe.sh [options]

Creates a tiny source tree containing target.txt and link.txt -> target.txt,
runs each candidate tcfs binary with sync_symlinks = true, and archives whether
the candidate preserved, skipped, or failed the symlink push.

Options:
  --candidate <label=path>  Candidate tcfs binary. Repeatable.
  --evidence-dir <path>    Evidence dir. Default: docs/release/evidence/tcfs-symlink-package-probe-<UTC>
  --endpoint <url>         S3 endpoint. Default: TCFS_SYMLINK_PROBE_ENDPOINT or http://100.64.48.53:8333
  --bucket <name>          S3 bucket. Default: TCFS_SYMLINK_PROBE_BUCKET or tcfs
  --prefix-base <prefix>   Remote prefix base. Default: tcfs-symlink-package-probe-<UTC>
  --run-honey-mount        Run mounted parse/target proof on honey for preserved candidates.
  --mount-label <label>    Limit --run-honey-mount to one producer label. Repeatable.
  --honey-host <host>      SSH host label. Default: TCFS_SYMLINK_PROBE_HONEY_HOST or honey.
  --honey-remote-dir <dir> Honey work dir. Default: /tmp/<prefix-base>.
  --honey-mount-root-base <path>
                           Mount root base. Default: <honey-remote-dir>/mount.
  --honey-tcfs-bin <path>  tcfs binary on honey. Default: tcfs.
  --honey-smoke-timeout-secs <n>
                           Mounted smoke timeout. Default: 120.
  --forward-aws-env        Forward AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY to honey.
  --strict                 Exit non-zero unless every candidate preserves symlinks.
  -h, --help               Show this help.

Environment:
  TCFS_SYMLINK_PROBE_ENDPOINT
  TCFS_SYMLINK_PROBE_BUCKET
  TCFS_SYMLINK_PROBE_PREFIX_BASE
  TCFS_SYMLINK_PROBE_EVIDENCE_DIR
  TCFS_SYMLINK_PROBE_STRICT=1
  TCFS_SYMLINK_PROBE_RUST_LOG
  TCFS_SYMLINK_PROBE_RUN_HONEY_MOUNT=1
  TCFS_SYMLINK_PROBE_HONEY_HOST
  TCFS_SYMLINK_PROBE_HONEY_REMOTE_DIR
  TCFS_SYMLINK_PROBE_HONEY_MOUNT_ROOT_BASE
  TCFS_SYMLINK_PROBE_HONEY_TCFS_BIN
  TCFS_SYMLINK_PROBE_HONEY_SMOKE_TIMEOUT_SECS
  TCFS_SYMLINK_PROBE_FORWARD_AWS_ENV=1

If no candidates are provided, the script probes executable local defaults:
Homebrew at /opt/homebrew/opt/tcfs/bin/tcfs and source-built
target/codex-verify/debug/tcfs.
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

sanitize_label() {
  local label="$1"
  label="$(printf '%s' "$label" | tr -c 'A-Za-z0-9_' '_')"
  label="${label##_}"
  label="${label%%_}"
  [[ -n "$label" ]] || label="candidate"
  printf '%s\n' "$label"
}

shell_quote() {
  printf '%q' "$1"
}

label_selected_for_mount() {
  local label="$1"
  local selected

  if [[ "${#mount_labels[@]}" -eq 0 ]]; then
    return 0
  fi

  for selected in "${mount_labels[@]}"; do
    if [[ "$selected" == "$label" ]]; then
      return 0
    fi
  done

  return 1
}

remote_url_for_prefix() {
  local prefix="$1"
  local endpoint_host="$endpoint"

  endpoint_host="${endpoint_host#http://}"
  endpoint_host="${endpoint_host#https://}"
  printf 'seaweedfs://%s/%s/%s\n' "$endpoint_host" "$bucket" "$prefix"
}

write_config() {
  local config_path="$1"
  local socket_path="$2"
  local state_path="$3"
  local sync_root="$4"
  local prefix="$5"

  cat >"$config_path" <<EOF
[daemon]
socket = "$socket_path"

[storage]
endpoint = "$endpoint"
region = "us-east-1"
bucket = "$bucket"
remote_prefix = "$prefix"
enforce_tls = false

[sync]
state_db = "$state_path"
sync_root = "$sync_root"
nats_url = "nats://localhost:4222"
nats_tls = false
sync_git_dirs = true
git_sync_mode = "raw"
sync_hidden_dirs = true
sync_symlinks = true
sync_empty_dirs = true

[crypto]
enabled = false
EOF
}

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
timestamp="$(date -u +%Y%m%dT%H%M%SZ)"

endpoint="${TCFS_SYMLINK_PROBE_ENDPOINT:-http://100.64.48.53:8333}"
bucket="${TCFS_SYMLINK_PROBE_BUCKET:-tcfs}"
prefix_base="${TCFS_SYMLINK_PROBE_PREFIX_BASE:-tcfs-symlink-package-probe-${timestamp}}"
evidence_dir="${TCFS_SYMLINK_PROBE_EVIDENCE_DIR:-$REPO_ROOT/docs/release/evidence/tcfs-symlink-package-probe-${timestamp}}"
strict="$(bool_env TCFS_SYMLINK_PROBE_STRICT "${TCFS_SYMLINK_PROBE_STRICT:-0}")"
rust_log="${TCFS_SYMLINK_PROBE_RUST_LOG:-tcfs_sync=debug,tcfs=info,tcfs_storage=warn,tcfs_secrets=warn}"
run_honey_mount="$(bool_env TCFS_SYMLINK_PROBE_RUN_HONEY_MOUNT "${TCFS_SYMLINK_PROBE_RUN_HONEY_MOUNT:-0}")"
honey_host="${TCFS_SYMLINK_PROBE_HONEY_HOST:-honey}"
honey_remote_dir="${TCFS_SYMLINK_PROBE_HONEY_REMOTE_DIR:-}"
honey_mount_root_base="${TCFS_SYMLINK_PROBE_HONEY_MOUNT_ROOT_BASE:-}"
honey_tcfs_bin="${TCFS_SYMLINK_PROBE_HONEY_TCFS_BIN:-tcfs}"
honey_smoke_timeout_secs="${TCFS_SYMLINK_PROBE_HONEY_SMOKE_TIMEOUT_SECS:-120}"
forward_aws_env="$(bool_env TCFS_SYMLINK_PROBE_FORWARD_AWS_ENV "${TCFS_SYMLINK_PROBE_FORWARD_AWS_ENV:-0}")"

candidate_specs=()
mount_labels=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --candidate)
      [[ $# -ge 2 ]] || fail "--candidate requires a label=path value"
      candidate_specs+=("$2")
      shift 2
      ;;
    --evidence-dir)
      [[ $# -ge 2 ]] || fail "--evidence-dir requires a value"
      evidence_dir="$2"
      shift 2
      ;;
    --endpoint)
      [[ $# -ge 2 ]] || fail "--endpoint requires a value"
      endpoint="$2"
      shift 2
      ;;
    --bucket)
      [[ $# -ge 2 ]] || fail "--bucket requires a value"
      bucket="$2"
      shift 2
      ;;
    --prefix-base)
      [[ $# -ge 2 ]] || fail "--prefix-base requires a value"
      prefix_base="$2"
      shift 2
      ;;
    --run-honey-mount)
      run_honey_mount=1
      shift
      ;;
    --mount-label)
      [[ $# -ge 2 ]] || fail "--mount-label requires a value"
      mount_labels+=("$(sanitize_label "$2")")
      shift 2
      ;;
    --honey-host)
      [[ $# -ge 2 ]] || fail "--honey-host requires a value"
      honey_host="$2"
      shift 2
      ;;
    --honey-remote-dir)
      [[ $# -ge 2 ]] || fail "--honey-remote-dir requires a value"
      honey_remote_dir="$2"
      shift 2
      ;;
    --honey-mount-root-base)
      [[ $# -ge 2 ]] || fail "--honey-mount-root-base requires a value"
      honey_mount_root_base="$2"
      shift 2
      ;;
    --honey-tcfs-bin)
      [[ $# -ge 2 ]] || fail "--honey-tcfs-bin requires a value"
      honey_tcfs_bin="$2"
      shift 2
      ;;
    --honey-smoke-timeout-secs)
      [[ $# -ge 2 ]] || fail "--honey-smoke-timeout-secs requires a value"
      honey_smoke_timeout_secs="$2"
      shift 2
      ;;
    --forward-aws-env)
      forward_aws_env=1
      shift
      ;;
    --strict)
      strict=1
      shift
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

if [[ -z "$honey_remote_dir" ]]; then
  honey_remote_dir="/tmp/${prefix_base}"
fi
if [[ -z "$honey_mount_root_base" ]]; then
  honey_mount_root_base="$honey_remote_dir/mount"
fi

if [[ "$run_honey_mount" == "1" ]]; then
  command -v ssh >/dev/null 2>&1 || fail "--run-honey-mount requires ssh"
  command -v scp >/dev/null 2>&1 || fail "--run-honey-mount requires scp"
  [[ "$honey_smoke_timeout_secs" =~ ^[0-9]+$ ]] || fail "--honey-smoke-timeout-secs must be an integer"
  case "$honey_remote_dir" in
    *[[:space:]]*) fail "--honey-remote-dir must not contain whitespace: $honey_remote_dir" ;;
  esac
  if ! [[ "$honey_remote_dir" =~ ^[A-Za-z0-9_./@%+=:-]+$ ]]; then
    fail "--honey-remote-dir contains unsafe shell characters: $honey_remote_dir"
  fi
  case "$honey_mount_root_base" in
    *[[:space:]]*) fail "--honey-mount-root-base must not contain whitespace: $honey_mount_root_base" ;;
  esac
  if ! [[ "$honey_mount_root_base" =~ ^[A-Za-z0-9_./@%+=:-]+$ ]]; then
    fail "--honey-mount-root-base contains unsafe shell characters: $honey_mount_root_base"
  fi
  if [[ "$forward_aws_env" == "1" ]]; then
    [[ -n "${AWS_ACCESS_KEY_ID:-}" ]] || fail "--forward-aws-env requires AWS_ACCESS_KEY_ID"
    [[ -n "${AWS_SECRET_ACCESS_KEY:-}" ]] || fail "--forward-aws-env requires AWS_SECRET_ACCESS_KEY"
  fi
fi

if [[ "${#candidate_specs[@]}" -eq 0 ]]; then
  if [[ -x /opt/homebrew/opt/tcfs/bin/tcfs ]]; then
    candidate_specs+=("homebrew=/opt/homebrew/opt/tcfs/bin/tcfs")
  fi
  if [[ -x "$REPO_ROOT/target/codex-verify/debug/tcfs" ]]; then
    candidate_specs+=("source_built=$REPO_ROOT/target/codex-verify/debug/tcfs")
  fi
fi

[[ "${#candidate_specs[@]}" -gt 0 ]] || fail "no candidate tcfs binaries found; pass --candidate label=/path/to/tcfs"

mkdir -p "$evidence_dir"
evidence_dir="$(cd "$evidence_dir" && pwd -P)"

tmpdir="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-symlink-package-probe.XXXXXX")"
trap 'rm -rf "$tmpdir"' EXIT

fixture="$tmpdir/source"
mkdir -p "$fixture"
printf 'target\n' >"$fixture/target.txt"
ln -s target.txt "$fixture/link.txt"

{
  printf 'target.txt\tfile\n'
  printf 'link.txt\ttarget.txt\n'
} >"$evidence_dir/fixture.tsv"
printf 'link.txt\ttarget.txt\n' >"$evidence_dir/symlink-targets.tsv"
printf 'target\n' >"$evidence_dir/target.txt.expected"

result_env="$evidence_dir/result.env"
{
  printf 'created_at_utc=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  printf 'endpoint=%s\n' "$endpoint"
  printf 'bucket=%s\n' "$bucket"
  printf 'prefix_base=%s\n' "$prefix_base"
  printf 'sync_symlinks=true\n'
  printf 'candidate_count=%s\n' "${#candidate_specs[@]}"
  printf 'production_claim=0\n'
  printf 'finder_claim=0\n'
  printf 'home_takeover_claim=0\n'
  printf 'run_honey_mount=%s\n' "$run_honey_mount"
  printf 'honey_host=%s\n' "$honey_host"
  printf 'honey_remote_dir=%s\n' "$honey_remote_dir"
  printf 'honey_mount_root_base=%s\n' "$honey_mount_root_base"
  printf 'honey_tcfs_bin=%s\n' "$honey_tcfs_bin"
  printf 'honey_smoke_timeout_secs=%s\n' "$honey_smoke_timeout_secs"
  printf 'forward_aws_env=%s\n' "$forward_aws_env"
} >"$result_env"

candidate_index=0
preserved_count=0
blocked_count=0
unknown_count=0
honey_mount_count=0
honey_mount_failed_count=0

for spec in "${candidate_specs[@]}"; do
  [[ "$spec" == *=* ]] || fail "candidate must be label=path: $spec"
  raw_label="${spec%%=*}"
  bin_path="${spec#*=}"
  label="$(sanitize_label "$raw_label")"
  candidate_index=$((candidate_index + 1))

  [[ -x "$bin_path" ]] || fail "candidate is not executable: $bin_path"
  bin_canon="$(cd "$(dirname "$bin_path")" && pwd -P)/$(basename "$bin_path")"
  prefix="${prefix_base}-${label}"
  config_path="$evidence_dir/${label}.toml"
  log_path="$evidence_dir/${label}.log"
  honey_script_path="$evidence_dir/${label}.honey-mount-run.sh"
  honey_commands_path="$evidence_dir/${label}.honey-mount-commands.txt"
  honey_log_path="$evidence_dir/${label}.honey-mount.log"
  honey_mount_log_path="$evidence_dir/${label}.honey-mount.mount.log"
  version_path="$evidence_dir/${label}.version.txt"
  version_err_path="$evidence_dir/${label}.version.err"
  state_path="$tmpdir/state/${label}.db"
  socket_path="$tmpdir/no-daemon-${label}.sock"
  mkdir -p "$(dirname "$state_path")"

  write_config "$config_path" "$socket_path" "$state_path" "$fixture" "$prefix"

  version_status=0
  if "$bin_canon" --version >"$version_path" 2>"$version_err_path"; then
    version="$(tr '\n' ' ' <"$version_path" | sed 's/[[:space:]]*$//')"
  else
    version_status=$?
    version="version-command-failed"
  fi
  sha256="$(shasum -a 256 "$bin_canon" | awk '{ print $1 }')"

  push_rc=0
  set +e
  RUST_LOG="$rust_log" "$bin_canon" --config "$config_path" push "$fixture" >"$log_path" 2>&1
  push_rc=$?
  set -e

  symlink_result="unknown"
  if [[ "$push_rc" -ne 0 ]]; then
    symlink_result="push_failed"
    blocked_count=$((blocked_count + 1))
  elif grep -Fq "uploaded symlink" "$log_path"; then
    symlink_result="preserved"
    preserved_count=$((preserved_count + 1))
  elif grep -Fq "skipping symlink" "$log_path"; then
    symlink_result="skipped"
    blocked_count=$((blocked_count + 1))
  else
    unknown_count=$((unknown_count + 1))
  fi

  honey_mount_status="not-run"
  honey_mount_rc="not-run"
  honey_mount_remote="$(remote_url_for_prefix "$prefix")"
  honey_label_mount_root="${honey_mount_root_base}-${label}"
  remote_env_file=""

  cat >"$honey_script_path" <<EOF
#!/usr/bin/env bash
set -euo pipefail

REMOTE=$(shell_quote "$honey_mount_remote")
TCFS_BIN=$(shell_quote "$honey_tcfs_bin")
MOUNT_ROOT=$(shell_quote "$honey_label_mount_root")
SMOKE_SCRIPT=$(shell_quote "$honey_remote_dir/lazy-hydration-mounted-smoke.sh")
EXPECTED_CONTENT_FILE=$(shell_quote "$honey_remote_dir/target.txt.expected")
SYMLINK_TARGETS_FILE=$(shell_quote "$honey_remote_dir/symlink-targets.tsv")
MOUNT_LOG=$(shell_quote "$honey_remote_dir/${label}.mount.log")
SMOKE_TIMEOUT_SECS=$(shell_quote "$honey_smoke_timeout_secs")

if [[ -n "\${TCFS_HONEY_ENV_FILE:-}" ]]; then
  # shellcheck disable=SC1090
  source "\$TCFS_HONEY_ENV_FILE"
fi

echo "tcfs binary requested: \$TCFS_BIN"
tcfs_resolved="\$TCFS_BIN"
if command -v "\$TCFS_BIN" >/dev/null 2>&1; then
  tcfs_resolved="\$(command -v "\$TCFS_BIN")"
elif [[ -x "\$TCFS_BIN" ]]; then
  tcfs_resolved="\$TCFS_BIN"
else
  printf 'tcfs binary is not executable or on PATH: %s\n' "\$TCFS_BIN" >&2
  exit 1
fi
echo "tcfs binary resolved: \$tcfs_resolved"
tcfs_version="\$("\$tcfs_resolved" --version 2>&1)" || {
  printf 'failed to run tcfs --version through %s\n' "\$tcfs_resolved" >&2
  printf '%s\n' "\$tcfs_version" >&2
  exit 1
}
echo "tcfs version: \$tcfs_version"
if command -v sha256sum >/dev/null 2>&1; then
  echo "tcfs sha256: \$(sha256sum "\$tcfs_resolved" | awk '{print \$1}')"
elif command -v shasum >/dev/null 2>&1; then
  echo "tcfs sha256: \$(shasum -a 256 "\$tcfs_resolved" | awk '{print \$1}')"
fi

mkdir -p "\$MOUNT_ROOT"
mount_started=0
cleanup_mount() {
  if [[ "\$mount_started" == "1" && "\${TCFS_HONEY_KEEP_MOUNT:-0}" != "1" ]]; then
    "\$tcfs_resolved" unmount "\$MOUNT_ROOT" >/dev/null 2>&1 || fusermount3 -u "\$MOUNT_ROOT" >/dev/null 2>&1 || true
  fi
}
trap cleanup_mount EXIT

nohup "\$tcfs_resolved" mount "\$REMOTE" "\$MOUNT_ROOT" >"\$MOUNT_LOG" 2>&1 &
mount_pid="\$!"
mount_started=1
for _ in {1..300}; do
  if mount | grep -F -- "\$MOUNT_ROOT" >/dev/null 2>&1; then
    break
  fi
  if ! kill -0 "\$mount_pid" 2>/dev/null; then
    tail -n 80 "\$MOUNT_LOG" >&2 || true
    echo "tcfs mount exited before mountpoint became active" >&2
    exit 1
  fi
  if command -v perl >/dev/null 2>&1; then
    perl -e 'select undef, undef, undef, 0.1'
  else
    python3 -c 'import select; select.select([], [], [], 0.1)'
  fi
done

if ! mount | grep -F -- "\$MOUNT_ROOT" >/dev/null 2>&1; then
  tail -n 80 "\$MOUNT_LOG" >&2 || true
  echo "tcfs mount did not become active" >&2
  exit 1
fi

args=(
  --mount-root "\$MOUNT_ROOT"
  --expected-file target.txt
  --expect-entry link.txt
  --expected-content-file "\$EXPECTED_CONTENT_FILE"
  --expected-symlink-targets-file "\$SYMLINK_TARGETS_FILE"
  --max-depth 2
)

if [[ "\$SMOKE_TIMEOUT_SECS" != "0" && "\$SMOKE_TIMEOUT_SECS" =~ ^[0-9]+$ ]] && command -v timeout >/dev/null 2>&1; then
  timeout "\$SMOKE_TIMEOUT_SECS" bash "\$SMOKE_SCRIPT" "\${args[@]}"
else
  bash "\$SMOKE_SCRIPT" "\${args[@]}"
fi
EOF
  chmod +x "$honey_script_path"

  cat >"$honey_commands_path" <<EOF
# Mounted package/current symlink parse proof for producer label: $label
ssh $(shell_quote "$honey_host") 'mkdir -p $(shell_quote "$honey_remote_dir")'
scp $(shell_quote "$REPO_ROOT/scripts/lazy-hydration-mounted-smoke.sh") $(shell_quote "$honey_host"):$(shell_quote "$honey_remote_dir/lazy-hydration-mounted-smoke.sh")
scp $(shell_quote "$evidence_dir/target.txt.expected") $(shell_quote "$honey_host"):$(shell_quote "$honey_remote_dir/target.txt.expected")
scp $(shell_quote "$evidence_dir/symlink-targets.tsv") $(shell_quote "$honey_host"):$(shell_quote "$honey_remote_dir/symlink-targets.tsv")
scp $(shell_quote "$honey_script_path") $(shell_quote "$honey_host"):$(shell_quote "$honey_remote_dir/${label}.honey-mount-run.sh")
ssh $(shell_quote "$honey_host") 'chmod +x $(shell_quote "$honey_remote_dir/lazy-hydration-mounted-smoke.sh") $(shell_quote "$honey_remote_dir/${label}.honey-mount-run.sh")'
ssh $(shell_quote "$honey_host") 'bash $(shell_quote "$honey_remote_dir/${label}.honey-mount-run.sh")'
EOF

  if [[ "$run_honey_mount" == "1" && "$symlink_result" != "preserved" ]]; then
    honey_mount_status="skipped-producer-not-preserved"
    honey_mount_rc="skipped"
  elif [[ "$run_honey_mount" == "1" ]] && ! label_selected_for_mount "$label"; then
    honey_mount_status="skipped-not-selected"
    honey_mount_rc="skipped"
  elif [[ "$run_honey_mount" == "1" ]]; then
    honey_mount_count=$((honey_mount_count + 1))
    # shellcheck disable=SC2029
    ssh "$honey_host" "mkdir -p $(shell_quote "$honey_remote_dir")"
    scp "$REPO_ROOT/scripts/lazy-hydration-mounted-smoke.sh" "$honey_host:$honey_remote_dir/lazy-hydration-mounted-smoke.sh"
    scp "$evidence_dir/target.txt.expected" "$honey_host:$honey_remote_dir/target.txt.expected"
    scp "$evidence_dir/symlink-targets.tsv" "$honey_host:$honey_remote_dir/symlink-targets.tsv"
    scp "$honey_script_path" "$honey_host:$honey_remote_dir/${label}.honey-mount-run.sh"
    # shellcheck disable=SC2029
    ssh "$honey_host" "chmod +x $(shell_quote "$honey_remote_dir/lazy-hydration-mounted-smoke.sh") $(shell_quote "$honey_remote_dir/${label}.honey-mount-run.sh")"

    if [[ "$forward_aws_env" == "1" ]]; then
      remote_env_file="$honey_remote_dir/aws-env.sh"
      aws_env_payload="$(printf 'export AWS_ACCESS_KEY_ID=%q\nexport AWS_SECRET_ACCESS_KEY=%q\n' "$AWS_ACCESS_KEY_ID" "$AWS_SECRET_ACCESS_KEY")"
      # shellcheck disable=SC2029
      ssh "$honey_host" "umask 077; cat > $(shell_quote "$remote_env_file")" <<<"$aws_env_payload"
    fi

    remote_run_cmd="$(printf 'bash %q' "$honey_remote_dir/${label}.honey-mount-run.sh")"
    if [[ -n "$remote_env_file" ]]; then
      remote_run_cmd="$(printf 'TCFS_HONEY_ENV_FILE=%q %s' "$remote_env_file" "$remote_run_cmd")"
    fi

    honey_rc=0
    # shellcheck disable=SC2029
    ssh "$honey_host" "$remote_run_cmd" >"$honey_log_path" 2>&1 || honey_rc=$?
    honey_mount_rc="$honey_rc"
    if [[ "$honey_rc" -eq 0 ]]; then
      honey_mount_status="passed"
    else
      honey_mount_status="failed"
      honey_mount_failed_count=$((honey_mount_failed_count + 1))
    fi

    # shellcheck disable=SC2029
    ssh "$honey_host" "test -f $(shell_quote "$honey_remote_dir/${label}.mount.log") && cat $(shell_quote "$honey_remote_dir/${label}.mount.log")" \
      >"$honey_mount_log_path" 2>/dev/null || true
    if [[ -n "$remote_env_file" ]]; then
      # shellcheck disable=SC2029
      ssh "$honey_host" "rm -f $(shell_quote "$remote_env_file")" >/dev/null 2>&1 || true
    fi
  fi

  {
    printf 'candidate_%s_label=%s\n' "$candidate_index" "$label"
    printf 'candidate_%s_bin=%s\n' "$candidate_index" "$bin_canon"
    printf 'candidate_%s_version=%s\n' "$candidate_index" "$version"
    printf 'candidate_%s_version_status=%s\n' "$candidate_index" "$version_status"
    printf 'candidate_%s_sha256=%s\n' "$candidate_index" "$sha256"
    printf 'candidate_%s_prefix=%s\n' "$candidate_index" "$prefix"
    printf 'candidate_%s_push_rc=%s\n' "$candidate_index" "$push_rc"
    printf 'candidate_%s_symlink_result=%s\n' "$candidate_index" "$symlink_result"
    printf 'candidate_%s_honey_mount_status=%s\n' "$candidate_index" "$honey_mount_status"
    printf 'candidate_%s_honey_mount_rc=%s\n' "$candidate_index" "$honey_mount_rc"
    printf 'candidate_%s_honey_mount_remote=%s\n' "$candidate_index" "$honey_mount_remote"
    printf 'candidate_%s_honey_mount_root=%s\n' "$candidate_index" "$honey_label_mount_root"
    printf 'candidate_%s_honey_mount_log=%s\n' "$candidate_index" "$honey_log_path"
    printf 'candidate_%s_config=%s\n' "$candidate_index" "$config_path"
    printf 'candidate_%s_log=%s\n' "$candidate_index" "$log_path"
    printf 'candidate_%s_label_safe=%s\n' "$candidate_index" "$label"
    printf '%s_bin=%s\n' "$label" "$bin_canon"
    printf '%s_version=%s\n' "$label" "$version"
    printf '%s_sha256=%s\n' "$label" "$sha256"
    printf '%s_prefix=%s\n' "$label" "$prefix"
    printf '%s_push_rc=%s\n' "$label" "$push_rc"
    printf '%s_symlink_result=%s\n' "$label" "$symlink_result"
    printf '%s_honey_mount_status=%s\n' "$label" "$honey_mount_status"
    printf '%s_honey_mount_rc=%s\n' "$label" "$honey_mount_rc"
    printf '%s_honey_mount_remote=%s\n' "$label" "$honey_mount_remote"
  } >>"$result_env"
done

overall_status="passed"
if [[ "$blocked_count" -gt 0 || "$unknown_count" -gt 0 ]]; then
  overall_status="blocked"
fi
if [[ "$run_honey_mount" == "1" && ( "$honey_mount_count" -eq 0 || "$honey_mount_failed_count" -gt 0 ) ]]; then
  overall_status="blocked"
fi

{
  printf 'preserved_count=%s\n' "$preserved_count"
  printf 'blocked_count=%s\n' "$blocked_count"
  printf 'unknown_count=%s\n' "$unknown_count"
  printf 'honey_mount_count=%s\n' "$honey_mount_count"
  printf 'honey_mount_failed_count=%s\n' "$honey_mount_failed_count"
  printf 'overall_status=%s\n' "$overall_status"
} >>"$result_env"

{
  printf '# TCFS Symlink Package Probe\n\n'
  printf "Created: \`%s\`\n\n" "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  printf "This packet probes candidate \`tcfs\` binaries with \`sync_symlinks = true\`\n"
  printf "against a tiny fixture containing \`target.txt\` and \`link.txt -> target.txt\`.\n\n"
  printf 'It is package/runtime drift evidence only. It does not claim production\n'
  printf 'readiness, Finder/FileProvider readiness, broad repo management, or home\n'
  printf 'directory takeover.\n\n'
  printf -- "- Endpoint: \`%s\`\n" "$endpoint"
  printf -- "- Bucket: \`%s\`\n" "$bucket"
  printf -- "- Prefix base: \`%s\`\n" "$prefix_base"
  printf -- "- Overall status: \`%s\`\n\n" "$overall_status"
  printf 'Candidate results:\n\n'
  for i in $(seq 1 "$candidate_index"); do
    label_line="$(awk -F= -v key="candidate_${i}_label" '$1 == key { print $2 }' "$result_env")"
    version_line="$(awk -F= -v key="candidate_${i}_version" '$1 == key { print $2 }' "$result_env")"
    result_line="$(awk -F= -v key="candidate_${i}_symlink_result" '$1 == key { print $2 }' "$result_env")"
    printf -- "- \`%s\`: \`%s\` (\`%s\`)\n" "$label_line" "$result_line" "$version_line"
  done
  printf '\nFiles:\n\n'
  printf -- "- \`result.env\`: machine-readable verdict, binary versions, and SHA-256s.\n"
  printf -- "- \`fixture.tsv\`: fixture shape and expected symlink target.\n"
  printf -- "- \`symlink-targets.tsv\`: mounted smoke symlink target fixture.\n"
  printf -- "- \`<label>.toml\`: per-candidate config with \`sync_symlinks = true\`.\n"
  printf -- "- \`<label>.log\`: per-candidate push output.\n\n"
  if [[ "$run_honey_mount" == "1" ]]; then
    printf "Mounted honey proof ran for \`%s\` preserved candidate(s); failures: \`%s\`.\n\n" "$honey_mount_count" "$honey_mount_failed_count"
    printf 'The mounted proof starts tcfs mount on honey, checks clean-name\n'
    printf 'visibility, cats target.txt, and verifies link.txt -> target.txt.\n\n'
  else
    printf 'Mounted honey proof was not run. Use --run-honey-mount after choosing\n'
    printf 'the candidate binary that should parse the published symlink index.\n\n'
  fi
  printf 'Re-run command shape:\n\n'
  printf '```bash\n'
  printf 'scripts/tcfs-symlink-package-probe.sh \\\n'
  printf '  --endpoint %s \\\n' "$(shell_quote "$endpoint")"
  printf '  --bucket %s \\\n' "$(shell_quote "$bucket")"
  printf '  --prefix-base %s' "$(shell_quote "$prefix_base")"
  for spec in "${candidate_specs[@]}"; do
    printf ' \\\n  --candidate %s' "$(shell_quote "$spec")"
  done
  if [[ "$run_honey_mount" == "1" ]]; then
    printf ' \\\n  --run-honey-mount \\\n  --honey-host %s \\\n  --honey-remote-dir %s \\\n  --honey-mount-root-base %s \\\n  --honey-tcfs-bin %s' \
      "$(shell_quote "$honey_host")" \
      "$(shell_quote "$honey_remote_dir")" \
      "$(shell_quote "$honey_mount_root_base")" \
      "$(shell_quote "$honey_tcfs_bin")"
  fi
  printf '\n```\n'
} >"$evidence_dir/README.md"

printf 'tcfs symlink package probe evidence: %s\n' "$evidence_dir"
printf 'overall status: %s\n' "$overall_status"

if [[ "$strict" == "1" && "$overall_status" != "passed" ]]; then
  exit 1
fi
