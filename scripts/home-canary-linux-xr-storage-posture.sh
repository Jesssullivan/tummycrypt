#!/usr/bin/env bash
#
# Storage-posture wrapper for the linux-xr isolated shadow canary.
#
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/home-canary-linux-xr-storage-posture.sh [options]

Run the linux-xr isolated-shadow helper under storage-proof defaults. This lane
is for S3/SeaweedFS posture evidence, not for broad home-directory takeover or
production storage claims by itself.

Options:
  --source <path>        Source project. Default: /Users/jess/git/linux-xr
  --shadow-root <path>   Shadow copy path. Default: ~/TCFS Pilot/real-canaries/linux-xr-storage-posture-<UTC>
  --evidence-dir <path>  Evidence dir. Default: docs/release/evidence/home-canary-linux-xr-storage-posture-<UTC>
  --remote <url>         seaweedfs://host:port/bucket/prefix disposable remote
  --state-dir <path>     Local TCFS state/config dir. Default: <evidence-dir>/state
  --tcfs-bin <path>      Release tcfs binary. Default: target/release/tcfs
  --push                 Push the shadow to the disposable prefix
  --create-bucket        Best-effort bucket creation before push
  --run-honey            Run honey mounted traversal smoke after push
  --run-linux-lifecycle  Run Linux lifecycle companion on honey after push
  --honey-host <host>    SSH host label. Default: honey
  --honey-mount-root <path>
                          Honey mountpoint. Default: delegated helper default
  --honey-remote-dir <path>
                          Honey work dir. Default: delegated helper default
  --honey-tcfs-bin <path>
                          tcfs binary on honey. Default: tcfs
  --honey-start-mount    With --run-honey, start tcfs mount on honey
  --honey-existing-mount With --run-honey, assume --honey-mount-root is mounted
  --honey-smoke-max-depth <n>
                          Mounted traversal depth. Default: 8
  --honey-smoke-timeout-secs <n>
                          Bound mounted smoke. Default: 900
  --forward-aws-env      Forward AWS env to honey companions
  --upload-concurrency <n>
                          TCFS_UPLOAD_CHUNK_CONCURRENCY. Default: 4
  --file-upload-concurrency <n>
                          TCFS_UPLOAD_FILE_CONCURRENCY. Default: 1
  --progress-every-chunks <n>
                          TCFS_UPLOAD_PROGRESS_EVERY_CHUNKS. Default: 1024
  --chunk-timeout-secs <n>
                          TCFS_UPLOAD_CHUNK_TIMEOUT_SECS. Default: 300; 0 disables
  --progress-heartbeat-secs <n>
                          TCFS_UPLOAD_PROGRESS_HEARTBEAT_SECS. Default: 60; 0 disables
  --s3-connect-timeout-secs <n>
                          storage.s3_connect_timeout_secs. Default: 10
  --s3-pool-idle-timeout-secs <n>
                          storage.s3_pool_idle_timeout_secs. Default: 15
  --s3-pool-max-idle-per-host <n>
                          storage.s3_pool_max_idle_per_host. Default: upload concurrency
  --s3-http1-only         Set storage.s3_http1_only = true
  --socket-sample-interval-secs <n>
                          Sample local tcfs S3 TCP sockets during push. Default: 5; 0 disables
  --no-assume-fresh-prefix
                          Do not set TCFS_UPLOAD_ASSUME_FRESH_PREFIX=1
  --allow-debug-binary   Permit target/debug/cargo-run style binaries
  --allow-non-posture-prefix
                          Permit explicit remote prefixes without "storage-posture"
  -h, --help             Show this help

Environment mirrors:
  TCFS_STORAGE_POSTURE_SOURCE
  TCFS_STORAGE_POSTURE_SHADOW_ROOT
  TCFS_STORAGE_POSTURE_EVIDENCE_DIR
  TCFS_STORAGE_POSTURE_REMOTE
  TCFS_STORAGE_POSTURE_STATE_DIR
  TCFS_STORAGE_POSTURE_PUSH=1
  TCFS_STORAGE_POSTURE_CREATE_BUCKET=1
  TCFS_STORAGE_POSTURE_RUN_HONEY=1
  TCFS_STORAGE_POSTURE_RUN_LINUX_LIFECYCLE=1
  TCFS_STORAGE_POSTURE_ALLOW_DEBUG_BINARY=1
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

canonical_existing_file() {
  local path="$1"
  [[ -f "$path" ]] || fail "file does not exist: $path"
  (cd "$(dirname "$path")" && printf '%s/%s\n' "$(pwd -P)" "$(basename "$path")")
}

canonical_or_path_command() {
  local candidate="$1"
  if [[ "$candidate" == */* ]]; then
    canonical_existing_file "$candidate"
  else
    command -v "$candidate" || fail "command not found: $candidate"
  fi
}

write_sha256() {
  local path="$1"
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$path" | awk '{print $1}'
  elif command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$path" | awk '{print $1}'
  else
    printf 'unavailable'
  fi
}

binary_profile() {
  local path="$1"
  case "$path" in
    */target/debug/*) printf 'debug' ;;
    */target/release/*) printf 'cargo-release' ;;
    *) printf 'external-release-or-packaged' ;;
  esac
}

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
timestamp="$(date -u +%Y%m%dT%H%M%SZ)"
run_id="home-canary-linux-xr-storage-posture-${timestamp}"

source_root="${TCFS_STORAGE_POSTURE_SOURCE:-/Users/jess/git/linux-xr}"
shadow_root="${TCFS_STORAGE_POSTURE_SHADOW_ROOT:-$HOME/TCFS Pilot/real-canaries/linux-xr-storage-posture-${timestamp}}"
evidence_dir="${TCFS_STORAGE_POSTURE_EVIDENCE_DIR:-$REPO_ROOT/docs/release/evidence/${run_id}}"
remote="${TCFS_STORAGE_POSTURE_REMOTE:-seaweedfs://localhost:8333/tcfs/${run_id}}"
state_dir="${TCFS_STORAGE_POSTURE_STATE_DIR:-}"
tcfs_bin="${TCFS_BIN:-${TCFS_STORAGE_POSTURE_TCFS_BIN:-}}"
push_remote="$(bool_env TCFS_STORAGE_POSTURE_PUSH "${TCFS_STORAGE_POSTURE_PUSH:-0}")"
create_bucket="$(bool_env TCFS_STORAGE_POSTURE_CREATE_BUCKET "${TCFS_STORAGE_POSTURE_CREATE_BUCKET:-0}")"
run_honey="$(bool_env TCFS_STORAGE_POSTURE_RUN_HONEY "${TCFS_STORAGE_POSTURE_RUN_HONEY:-0}")"
run_linux_lifecycle="$(bool_env TCFS_STORAGE_POSTURE_RUN_LINUX_LIFECYCLE "${TCFS_STORAGE_POSTURE_RUN_LINUX_LIFECYCLE:-0}")"
allow_debug_binary="$(bool_env TCFS_STORAGE_POSTURE_ALLOW_DEBUG_BINARY "${TCFS_STORAGE_POSTURE_ALLOW_DEBUG_BINARY:-0}")"
allow_non_posture_prefix="$(bool_env TCFS_STORAGE_POSTURE_ALLOW_NON_POSTURE_PREFIX "${TCFS_STORAGE_POSTURE_ALLOW_NON_POSTURE_PREFIX:-0}")"
assume_fresh_prefix="$(bool_env TCFS_STORAGE_POSTURE_ASSUME_FRESH_PREFIX "${TCFS_STORAGE_POSTURE_ASSUME_FRESH_PREFIX:-1}")"
honey_host="${TCFS_HONEY_HOST:-honey}"
honey_mount_root="${TCFS_HONEY_MOUNT_ROOT:-}"
honey_remote_dir="${TCFS_HONEY_REMOTE_DIR:-}"
honey_tcfs_bin="${TCFS_HONEY_TCFS_BIN:-tcfs}"
honey_start_mount="$(bool_env TCFS_HONEY_START_MOUNT "${TCFS_HONEY_START_MOUNT:-0}")"
honey_existing_mount="$(bool_env TCFS_HONEY_EXISTING_MOUNT "${TCFS_HONEY_EXISTING_MOUNT:-0}")"
honey_smoke_max_depth="${TCFS_HONEY_SMOKE_MAX_DEPTH:-8}"
honey_smoke_timeout_secs="${TCFS_HONEY_SMOKE_TIMEOUT_SECS:-900}"
forward_aws_env="$(bool_env TCFS_HONEY_FORWARD_AWS_ENV "${TCFS_HONEY_FORWARD_AWS_ENV:-0}")"
upload_concurrency="${TCFS_UPLOAD_CHUNK_CONCURRENCY:-4}"
file_upload_concurrency="${TCFS_UPLOAD_FILE_CONCURRENCY:-1}"
progress_every_chunks="${TCFS_UPLOAD_PROGRESS_EVERY_CHUNKS:-1024}"
chunk_timeout_secs="${TCFS_UPLOAD_CHUNK_TIMEOUT_SECS:-300}"
progress_heartbeat_secs="${TCFS_UPLOAD_PROGRESS_HEARTBEAT_SECS:-60}"
storage_s3_connect_timeout_secs="${TCFS_STORAGE_S3_CONNECT_TIMEOUT_SECS:-10}"
storage_s3_pool_idle_timeout_secs="${TCFS_STORAGE_S3_POOL_IDLE_TIMEOUT_SECS:-15}"
storage_s3_pool_max_idle_per_host="${TCFS_STORAGE_S3_POOL_MAX_IDLE_PER_HOST:-}"
storage_s3_http1_only="$(bool_env TCFS_STORAGE_S3_HTTP1_ONLY "${TCFS_STORAGE_S3_HTTP1_ONLY:-0}")"
socket_sample_interval_secs="${TCFS_STORAGE_SOCKET_SAMPLE_INTERVAL_SECS:-5}"
storage_object_model="large-sequential-fastcdc-for-git-pack"
git_pack_chunk_profile="min=1MiB avg=4MiB max=16MiB"
git_index_chunk_profile="min=32KiB avg=64KiB max=256KiB"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --source)
      [[ $# -ge 2 ]] || fail "--source requires a value"
      source_root="$2"
      shift 2
      ;;
    --shadow-root)
      [[ $# -ge 2 ]] || fail "--shadow-root requires a value"
      shadow_root="$2"
      shift 2
      ;;
    --evidence-dir)
      [[ $# -ge 2 ]] || fail "--evidence-dir requires a value"
      evidence_dir="$2"
      shift 2
      ;;
    --remote)
      [[ $# -ge 2 ]] || fail "--remote requires a value"
      remote="$2"
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
    --push)
      push_remote=1
      shift
      ;;
    --create-bucket)
      create_bucket=1
      shift
      ;;
    --run-honey)
      run_honey=1
      shift
      ;;
    --run-linux-lifecycle)
      run_linux_lifecycle=1
      shift
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
      shift
      ;;
    --honey-existing-mount)
      honey_existing_mount=1
      honey_start_mount=0
      shift
      ;;
    --honey-smoke-max-depth)
      [[ $# -ge 2 ]] || fail "--honey-smoke-max-depth requires a value"
      honey_smoke_max_depth="$2"
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
    --upload-concurrency)
      [[ $# -ge 2 ]] || fail "--upload-concurrency requires a value"
      upload_concurrency="$2"
      shift 2
      ;;
    --file-upload-concurrency)
      [[ $# -ge 2 ]] || fail "--file-upload-concurrency requires a value"
      file_upload_concurrency="$2"
      shift 2
      ;;
    --progress-every-chunks)
      [[ $# -ge 2 ]] || fail "--progress-every-chunks requires a value"
      progress_every_chunks="$2"
      shift 2
      ;;
    --chunk-timeout-secs)
      [[ $# -ge 2 ]] || fail "--chunk-timeout-secs requires a value"
      chunk_timeout_secs="$2"
      shift 2
      ;;
    --progress-heartbeat-secs)
      [[ $# -ge 2 ]] || fail "--progress-heartbeat-secs requires a value"
      progress_heartbeat_secs="$2"
      shift 2
      ;;
    --s3-connect-timeout-secs)
      [[ $# -ge 2 ]] || fail "--s3-connect-timeout-secs requires a value"
      storage_s3_connect_timeout_secs="$2"
      shift 2
      ;;
    --s3-pool-idle-timeout-secs)
      [[ $# -ge 2 ]] || fail "--s3-pool-idle-timeout-secs requires a value"
      storage_s3_pool_idle_timeout_secs="$2"
      shift 2
      ;;
    --s3-pool-max-idle-per-host)
      [[ $# -ge 2 ]] || fail "--s3-pool-max-idle-per-host requires a value"
      storage_s3_pool_max_idle_per_host="$2"
      shift 2
      ;;
    --s3-http1-only)
      storage_s3_http1_only=1
      shift
      ;;
    --socket-sample-interval-secs)
      [[ $# -ge 2 ]] || fail "--socket-sample-interval-secs requires a value"
      socket_sample_interval_secs="$2"
      shift 2
      ;;
    --no-assume-fresh-prefix)
      assume_fresh_prefix=0
      shift
      ;;
    --allow-debug-binary)
      allow_debug_binary=1
      shift
      ;;
    --allow-non-posture-prefix)
      allow_non_posture_prefix=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      fail "unknown option: $1"
      ;;
  esac
done

[[ "$upload_concurrency" =~ ^[0-9]+$ ]] || fail "--upload-concurrency must be numeric"
[[ "$file_upload_concurrency" =~ ^[0-9]+$ ]] || fail "--file-upload-concurrency must be numeric"
[[ "$progress_every_chunks" =~ ^[0-9]+$ ]] || fail "--progress-every-chunks must be numeric"
[[ "$chunk_timeout_secs" =~ ^[0-9]+$ ]] || fail "--chunk-timeout-secs must be numeric"
[[ "$progress_heartbeat_secs" =~ ^[0-9]+$ ]] || fail "--progress-heartbeat-secs must be numeric"
[[ "$storage_s3_connect_timeout_secs" =~ ^[0-9]+$ ]] || fail "--s3-connect-timeout-secs must be numeric"
[[ "$storage_s3_pool_idle_timeout_secs" =~ ^[0-9]+$ ]] || fail "--s3-pool-idle-timeout-secs must be numeric"
if [[ -z "$storage_s3_pool_max_idle_per_host" ]]; then
  storage_s3_pool_max_idle_per_host="$upload_concurrency"
fi
[[ "$storage_s3_pool_max_idle_per_host" =~ ^[0-9]+$ ]] || fail "--s3-pool-max-idle-per-host must be numeric"
[[ "$socket_sample_interval_secs" =~ ^[0-9]+$ ]] || fail "--socket-sample-interval-secs must be numeric"
[[ "$honey_smoke_max_depth" =~ ^[0-9]+$ ]] || fail "--honey-smoke-max-depth must be numeric"
[[ "$honey_smoke_timeout_secs" =~ ^[0-9]+$ ]] || fail "--honey-smoke-timeout-secs must be numeric"

if [[ -z "$tcfs_bin" ]]; then
  if [[ -x "$REPO_ROOT/target/release/tcfs" ]]; then
    tcfs_bin="$REPO_ROOT/target/release/tcfs"
  else
    fail "set --tcfs-bin or build target/release/tcfs before storage posture proof"
  fi
fi
tcfs_bin="$(canonical_or_path_command "$tcfs_bin")"
tcfs_profile="$(binary_profile "$tcfs_bin")"
if [[ "$allow_debug_binary" != "1" && "$tcfs_profile" == "debug" ]]; then
  fail "storage posture proof requires a release binary; pass --allow-debug-binary only for tests"
fi

remote_no_scheme="${remote#seaweedfs://}"
[[ "$remote_no_scheme" != "$remote" ]] || fail "remote must use seaweedfs://"
remote_host="${remote_no_scheme%%/*}"
[[ -n "$remote_host" ]] || fail "remote host is empty: $remote"
remote_path="${remote_no_scheme#*/}"
[[ "$remote_path" != "$remote_no_scheme" ]] || fail "remote must include bucket and prefix: $remote"
bucket="${remote_path%%/*}"
if [[ "$remote_path" == "$bucket" ]]; then
  prefix=""
else
  prefix="${remote_path#*/}"
  prefix="${prefix%/}"
fi
[[ -n "$bucket" && -n "$prefix" ]] || fail "remote must include a dedicated non-root prefix: $remote"
if [[ "$assume_fresh_prefix" == "1" && "$allow_non_posture_prefix" != "1" && "$prefix" != *storage-posture* ]]; then
  fail "fresh-prefix storage posture proof requires a prefix containing storage-posture"
fi

mkdir -p "$evidence_dir"
if [[ -z "$state_dir" ]]; then
  state_dir="$evidence_dir/state"
fi

tcfs_version="$("$tcfs_bin" --version 2>&1 || true)"
tcfs_sha256="$(write_sha256 "$tcfs_bin")"
tcfs_file_type=""
if command -v file >/dev/null 2>&1; then
  tcfs_file_type="$(file "$tcfs_bin" 2>/dev/null || true)"
fi
if [[ -n "${AWS_ACCESS_KEY_ID:-}" ]]; then
  aws_access_key_id_present=1
else
  aws_access_key_id_present=0
fi
if [[ -n "${AWS_SECRET_ACCESS_KEY:-}" ]]; then
  aws_secret_access_key_present=1
else
  aws_secret_access_key_present=0
fi
if [[ -n "${AWS_SESSION_TOKEN:-}" ]]; then
  aws_session_token_present=1
else
  aws_session_token_present=0
fi
if [[ "$aws_access_key_id_present" == "1" || "$aws_secret_access_key_present" == "1" || "$aws_session_token_present" == "1" ]]; then
  credential_source=aws_env_present
else
  credential_source=unset_or_helper_default
fi

write_posture_metadata() {
  local helper_status="$1"
  cat >"$evidence_dir/storage-posture.env" <<EOF
created_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
run_id=$run_id
helper_status=$helper_status
posture_claim=not-production-storage-posture
source=$source_root
shadow=$shadow_root
remote=$remote
bucket=$bucket
remote_prefix=$prefix
endpoint=http://$remote_host
transport_tls=false
credential_aws_access_key_id_present=$aws_access_key_id_present
credential_aws_secret_access_key_present=$aws_secret_access_key_present
credential_aws_session_token_present=$aws_session_token_present
credential_source=$credential_source
prefix_guard=storage-posture
push=$push_remote
create_bucket=$create_bucket
state_dir=$state_dir
tcfs_bin=$tcfs_bin
tcfs_binary_profile=$tcfs_profile
tcfs_version=$tcfs_version
tcfs_sha256=$tcfs_sha256
assume_fresh_prefix=$assume_fresh_prefix
upload_concurrency=$upload_concurrency
file_upload_concurrency=$file_upload_concurrency
progress_every_chunks=$progress_every_chunks
chunk_timeout_secs=$chunk_timeout_secs
progress_heartbeat_secs=$progress_heartbeat_secs
storage_max_concurrent_ops=$upload_concurrency
storage_s3_connect_timeout_secs=$storage_s3_connect_timeout_secs
storage_s3_pool_idle_timeout_secs=$storage_s3_pool_idle_timeout_secs
storage_s3_pool_max_idle_per_host=$storage_s3_pool_max_idle_per_host
storage_s3_http1_only=$storage_s3_http1_only
storage_object_model=$storage_object_model
git_pack_chunk_profile=$git_pack_chunk_profile
git_index_chunk_profile=$git_index_chunk_profile
socket_sample_interval_secs=$socket_sample_interval_secs
run_honey=$run_honey
run_linux_lifecycle=$run_linux_lifecycle
honey_host=$honey_host
honey_mount_root=$honey_mount_root
honey_remote_dir=$honey_remote_dir
honey_start_mount=$honey_start_mount
honey_existing_mount=$honey_existing_mount
forward_aws_env=$forward_aws_env
production_storage_posture_claim=0
EOF
  if [[ -n "$tcfs_file_type" ]]; then
    printf 'tcfs_file_type=%s\n' "$tcfs_file_type" >>"$evidence_dir/storage-posture.env"
  fi

  cat >"$evidence_dir/storage-posture.md" <<EOF
# TCFS linux-xr S3 Storage Posture Packet

This packet is a storage-facing canary for the isolated \`linux-xr\` shadow.
It is separate from the scoped project-tree correctness claim and is not, by
itself, a production S3 posture claim.

Required claim boundary:

- use a release or packaged \`tcfs\` binary, not an unlabelled debug build
- use a fresh disposable remote prefix
- preserve \`chunk_exists_check=false\` when fresh-prefix mode is enabled
- preserve chunk progress rows, concurrency, retry/warning counts, object
  counts, chunk timeout posture, endpoint posture, S3 HTTP client limits,
  heartbeat rows, and push wall-clock/memory evidence where available
- record the object-model decision: raw Git \`.pack\` files use the large
  sequential FastCDC profile (1MiB minimum, 4MiB average, 16MiB maximum)
  while \`.idx\` files stay on the moderate pack-index profile
- keep production Finder, broad home-directory takeover, and on-prem cutover out
  of this packet

The underlying inventory/shadow/push mechanics are delegated to
\`scripts/home-canary-linux-xr-shadow.sh\`; this wrapper records the
storage-posture defaults in \`storage-posture.env\`.
EOF
}

write_posture_metadata pending

child_args=(
  --source "$source_root"
  --shadow-root "$shadow_root"
  --evidence-dir "$evidence_dir"
  --remote "$remote"
  --tcfs-bin "$tcfs_bin"
)
child_args+=(--state-dir "$state_dir")
if [[ "$push_remote" == "1" ]]; then
  child_args+=(--push)
fi
if [[ "$create_bucket" == "1" ]]; then
  child_args+=(--create-bucket)
fi
if [[ "$run_honey" == "1" ]]; then
  child_args+=(--run-honey)
fi
if [[ "$run_linux_lifecycle" == "1" ]]; then
  child_args+=(--run-linux-lifecycle)
fi
if [[ "$honey_start_mount" == "1" ]]; then
  child_args+=(--honey-start-mount)
fi
if [[ "$honey_existing_mount" == "1" ]]; then
  child_args+=(--honey-existing-mount)
fi
if [[ "$forward_aws_env" == "1" ]]; then
  child_args+=(--forward-aws-env)
fi
if [[ -n "$honey_mount_root" ]]; then
  child_args+=(--honey-mount-root "$honey_mount_root")
fi
if [[ -n "$honey_remote_dir" ]]; then
  child_args+=(--honey-remote-dir "$honey_remote_dir")
fi
child_args+=(
  --honey-host "$honey_host"
  --honey-tcfs-bin "$honey_tcfs_bin"
  --honey-smoke-max-depth "$honey_smoke_max_depth"
  --honey-smoke-timeout-secs "$honey_smoke_timeout_secs"
)

export TCFS_UPLOAD_CHUNK_CONCURRENCY="$upload_concurrency"
export TCFS_UPLOAD_FILE_CONCURRENCY="$file_upload_concurrency"
export TCFS_UPLOAD_PROGRESS_EVERY_CHUNKS="$progress_every_chunks"
export TCFS_UPLOAD_CHUNK_TIMEOUT_SECS="$chunk_timeout_secs"
export TCFS_UPLOAD_PROGRESS_HEARTBEAT_SECS="$progress_heartbeat_secs"
export TCFS_STORAGE_MAX_CONCURRENT_OPS="$upload_concurrency"
export TCFS_STORAGE_S3_CONNECT_TIMEOUT_SECS="$storage_s3_connect_timeout_secs"
export TCFS_STORAGE_S3_POOL_IDLE_TIMEOUT_SECS="$storage_s3_pool_idle_timeout_secs"
export TCFS_STORAGE_S3_POOL_MAX_IDLE_PER_HOST="$storage_s3_pool_max_idle_per_host"
export TCFS_STORAGE_S3_HTTP1_ONLY="$storage_s3_http1_only"
if [[ "$assume_fresh_prefix" == "1" ]]; then
  export TCFS_UPLOAD_ASSUME_FRESH_PREFIX=1
else
  unset TCFS_UPLOAD_ASSUME_FRESH_PREFIX
fi

sample_s3_sockets() {
  local helper_pid="$1"
  local samples_path="$evidence_dir/s3-socket-samples.tsv"
  local summary_path="$evidence_dir/s3-socket-summary.env"
  local highwater=0
  local highwater_pids=""
  local highwater_at=""

  printf 'sampled_at_utc\ttcfs_pids\ts3_established_sockets\thighwater\tlimit\n' >"$samples_path"

  while kill -0 "$helper_pid" >/dev/null 2>&1; do
    local sampled_at
    local pids
    local socket_count=0
    sampled_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    pids="$(
      ps -axo pid=,command= | awk -v bin="$tcfs_bin" -v prefix="$prefix" '
        index($0, bin) > 0 && index($0, " push ") > 0 && index($0, prefix) > 0 {
          print $1
        }
      '
    )"

    if [[ -n "$pids" ]] && command -v lsof >/dev/null 2>&1; then
      local pid
      for pid in $pids; do
        local pid_socket_count
        pid_socket_count="$(
          lsof -nP -a -p "$pid" -iTCP 2>/dev/null | awk -v endpoint="$remote_host" '
            NR > 1 && index($0, endpoint) > 0 && $0 ~ /ESTABLISHED/ {
              count += 1
            }
            END { print count + 0 }
          '
        )"
        socket_count=$((socket_count + pid_socket_count))
      done
    fi

    if (( socket_count > highwater )); then
      highwater="$socket_count"
      highwater_pids="${pids//$'\n'/,}"
      highwater_at="$sampled_at"
    fi

    printf '%s\t%s\t%s\t%s\t%s\n' \
      "$sampled_at" \
      "${pids//$'\n'/,}" \
      "$socket_count" \
      "$highwater" \
      "$upload_concurrency" >>"$samples_path"

    sleep "$socket_sample_interval_secs"
  done

  {
    printf 'socket_sample_interval_secs=%s\n' "$socket_sample_interval_secs"
    printf 'socket_sample_limit=%s\n' "$upload_concurrency"
    printf 'socket_highwater=%s\n' "$highwater"
    printf 'socket_highwater_at_utc=%s\n' "$highwater_at"
    printf 'socket_highwater_pids=%s\n' "$highwater_pids"
    if (( highwater > upload_concurrency )); then
      printf 'socket_highwater_exceeded_upload_concurrency=1\n'
    else
      printf 'socket_highwater_exceeded_upload_concurrency=0\n'
    fi
  } >"$summary_path"
}

set +e
if [[ "$push_remote" == "1" && "$socket_sample_interval_secs" != "0" ]]; then
  "$REPO_ROOT/scripts/home-canary-linux-xr-shadow.sh" "${child_args[@]}" &
  helper_pid=$!
  sample_s3_sockets "$helper_pid" &
  sampler_pid=$!
  wait "$helper_pid"
  helper_rc=$?
  wait "$sampler_pid" >/dev/null 2>&1 || true
else
  "$REPO_ROOT/scripts/home-canary-linux-xr-shadow.sh" "${child_args[@]}"
  helper_rc=$?
fi
set -e

write_posture_metadata "$helper_rc"
exit "$helper_rc"
