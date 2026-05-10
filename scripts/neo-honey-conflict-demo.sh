#!/usr/bin/env bash
#
# Prepare or run the same-fixture neo/honey conflict proof packet.
#
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/neo-honey-conflict-demo.sh [options]

Create an isolated neo fixture, optionally push it to a disposable remote
prefix, have honey pull the same file and edit it locally, have neo edit/push a
different version, then have honey push and prove TCFS records a conflict
without overwriting neo's remote bytes or losing honey's local bytes.

Options:
  --remote <seaweedfs://host:port/bucket/prefix>
      Remote prefix to use. Defaults to a timestamped prefix.
  --neo-root <path>
      Local isolated neo root. Defaults to "$HOME/TCFS Pilot/runs/<run-id>/neo".
  --evidence-dir <path>
      Evidence output directory. Defaults to a temp directory.
  --state-dir <path>
      Local helper state directory. Defaults to <evidence-dir>/neo-state.
  --push
      Push the initial neo fixture.
  --create-bucket
      Best-effort remote bucket creation before pushing.
  --tcfs-bin <path>
      Local tcfs binary for neo push/pull.
  --honey-host <host>
      SSH host label for honey. Default: honey.
  --honey-root <path>
      Honey physical sync root. Default: /tmp/tcfs-<run-id>-honey/root.
  --honey-remote-dir <path>
      Honey temp directory. Default: /tmp/tcfs-<run-id>-honey/run.
  --honey-tcfs-bin <path>
      Remote tcfs binary path on honey. Default: tcfs.
  --run-honey
      Copy the honey runner to honey and run the conflict stages.
  --honey-recover-keep-both
      After conflict detection, run a manual keep-both recovery: preserve honey's
      conflicted bytes under a sibling path, rehydrate the original path from
      remote, push the sibling copy, and pull both paths back for exact checks.
  --honey-independent-sibling
      After conflict detection, push an independently edited sibling file from
      honey and prove it reaches remote while the original file remains in
      conflict.
  --honey-resolve-keep-both
      After conflict detection, start an isolated honey tcfsd and attempt
      daemon-backed `tcfs resolve --strategy keep-both`. This is mutually
      exclusive with --honey-recover-keep-both.
  --honey-tcfsd-bin <path>
      Remote tcfsd binary path on honey for --honey-resolve-keep-both.
      Default: tcfsd.
  --forward-aws-env
      With --run-honey, forward current AWS env to honey for this smoke run.
  --allow-real-roots
      Allow direct HOME, ~/Documents, or ~/git neo roots. Not recommended.
  -h, --help
      Show this help.

Environment mirrors:
  TCFS_CONFLICT_REMOTE
  TCFS_CONFLICT_NEO_ROOT
  TCFS_CONFLICT_EVIDENCE_DIR
  TCFS_CONFLICT_STATE_DIR
  TCFS_CONFLICT_PUSH=1
  TCFS_CONFLICT_CREATE_BUCKET=1
  TCFS_CONFLICT_RUN_HONEY=1
  TCFS_BIN
  TCFS_HONEY_HOST
  TCFS_HONEY_ROOT
  TCFS_HONEY_REMOTE_DIR
  TCFS_HONEY_TCFS_BIN
  TCFS_CONFLICT_HONEY_RECOVER_KEEP_BOTH=1
  TCFS_CONFLICT_HONEY_INDEPENDENT_SIBLING=1
  TCFS_CONFLICT_HONEY_RESOLVE_KEEP_BOTH=1
  TCFS_HONEY_TCFSD_BIN
  TCFS_HONEY_FORWARD_AWS_ENV=1
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

single_quote() {
  local value="${1//\'/\'\\\'\'}"
  printf "'%s'" "$value"
}

make_physical_dir() {
  local path="$1"
  mkdir -p "$path"
  (cd "$path" && pwd -P)
}

canonical_path() {
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

  fail "parent directory does not exist for path: $path"
}

assert_safe_remote_path() {
  local label="$1"
  local path="$2"

  case "$path" in
    *[[:space:]]*) fail "$label must not contain whitespace: $path" ;;
  esac
  if ! [[ "$path" =~ ^[A-Za-z0-9_./@%+=:-]+$ ]]; then
    fail "$label contains unsafe shell characters: $path"
  fi
}

write_device_registry() {
  local path="$1"
  cat >"$path" <<'EOF'
{
  "devices": [
    {
      "name": "neo-conflict",
      "device_id": "00000000-0000-4000-8000-0000000000a1",
      "public_key": "age1-device-neo-conflict",
      "signing_key_hash": "neo0000000000000",
      "description": "TCFS conflict helper neo fixture",
      "enrolled_at": 1,
      "revoked": false,
      "last_nats_seq": 0
    },
    {
      "name": "honey-conflict",
      "device_id": "00000000-0000-4000-8000-0000000000b2",
      "public_key": "age1-device-honey-conflict",
      "signing_key_hash": "honey00000000000",
      "description": "TCFS conflict helper honey fixture",
      "enrolled_at": 1,
      "revoked": false,
      "last_nats_seq": 0
    }
  ]
}
EOF
}

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
timestamp="$(date -u +%Y%m%dT%H%M%SZ)"
run_id="neo-honey-conflict-${timestamp}-$$"

remote="${TCFS_CONFLICT_REMOTE:-seaweedfs://localhost:8333/tcfs/${run_id}}"
neo_root="${TCFS_CONFLICT_NEO_ROOT:-$HOME/TCFS Pilot/runs/${run_id}/neo}"
evidence_dir="${TCFS_CONFLICT_EVIDENCE_DIR:-${TMPDIR:-/tmp}/tcfs-${run_id}}"
state_dir="${TCFS_CONFLICT_STATE_DIR:-}"
push_remote="$(bool_env TCFS_CONFLICT_PUSH "${TCFS_CONFLICT_PUSH:-0}")"
create_bucket="$(bool_env TCFS_CONFLICT_CREATE_BUCKET "${TCFS_CONFLICT_CREATE_BUCKET:-0}")"
run_honey="$(bool_env TCFS_CONFLICT_RUN_HONEY "${TCFS_CONFLICT_RUN_HONEY:-0}")"
honey_recover_keep_both="$(bool_env TCFS_CONFLICT_HONEY_RECOVER_KEEP_BOTH "${TCFS_CONFLICT_HONEY_RECOVER_KEEP_BOTH:-0}")"
honey_independent_sibling="$(bool_env TCFS_CONFLICT_HONEY_INDEPENDENT_SIBLING "${TCFS_CONFLICT_HONEY_INDEPENDENT_SIBLING:-0}")"
honey_resolve_keep_both="$(bool_env TCFS_CONFLICT_HONEY_RESOLVE_KEEP_BOTH "${TCFS_CONFLICT_HONEY_RESOLVE_KEEP_BOTH:-0}")"
tcfs_bin="${TCFS_BIN:-}"
honey_host="${TCFS_HONEY_HOST:-honey}"
honey_root="${TCFS_HONEY_ROOT:-/tmp/tcfs-${run_id}-honey/root}"
honey_remote_dir="${TCFS_HONEY_REMOTE_DIR:-/tmp/tcfs-${run_id}-honey/run}"
honey_tcfs_bin="${TCFS_HONEY_TCFS_BIN:-tcfs}"
honey_tcfsd_bin="${TCFS_HONEY_TCFSD_BIN:-tcfsd}"
forward_aws_env="$(bool_env TCFS_HONEY_FORWARD_AWS_ENV "${TCFS_HONEY_FORWARD_AWS_ENV:-0}")"
allow_real_roots=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --remote)
      [[ $# -ge 2 ]] || fail "--remote requires a value"
      remote="$2"
      shift 2
      ;;
    --neo-root)
      [[ $# -ge 2 ]] || fail "--neo-root requires a value"
      neo_root="$2"
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
    --push)
      push_remote=1
      shift
      ;;
    --create-bucket)
      create_bucket=1
      shift
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
    --honey-root)
      [[ $# -ge 2 ]] || fail "--honey-root requires a value"
      honey_root="$2"
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
    --honey-tcfsd-bin)
      [[ $# -ge 2 ]] || fail "--honey-tcfsd-bin requires a value"
      honey_tcfsd_bin="$2"
      shift 2
      ;;
    --run-honey)
      run_honey=1
      shift
      ;;
    --honey-recover-keep-both)
      honey_recover_keep_both=1
      shift
      ;;
    --honey-independent-sibling)
      honey_independent_sibling=1
      shift
      ;;
    --honey-resolve-keep-both)
      honey_resolve_keep_both=1
      shift
      ;;
    --forward-aws-env)
      forward_aws_env=1
      shift
      ;;
    --allow-real-roots)
      allow_real_roots=1
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

if [[ "$honey_recover_keep_both" == "1" && "$honey_resolve_keep_both" == "1" ]]; then
  fail "--honey-recover-keep-both and --honey-resolve-keep-both are mutually exclusive"
fi

[[ "$remote" == seaweedfs://* ]] || fail "remote must start with seaweedfs://"
remote_rest="${remote#seaweedfs://}"
remote_host="${remote_rest%%/*}"
[[ "$remote_rest" != "$remote_host" ]] || fail "remote must include /bucket/prefix: $remote"
remote_path="${remote_rest#*/}"
bucket="${remote_path%%/*}"
[[ -n "$bucket" ]] || fail "remote bucket is empty: $remote"
if [[ "$remote_path" == "$bucket" ]]; then
  prefix=""
else
  prefix="${remote_path#*/}"
  prefix="${prefix%/}"
fi
[[ -n "$prefix" ]] || fail "remote must include a dedicated non-root prefix: $remote"
endpoint="http://${remote_host}"
region="${TCFS_S3_REGION:-us-east-1}"

assert_safe_remote_path "--honey-root" "$honey_root"
assert_safe_remote_path "--honey-remote-dir" "$honey_remote_dir"

neo_canon="$(make_physical_dir "$neo_root")"
home_canon="$(canonical_path "$HOME")"
documents_canon="$(canonical_path "$HOME/Documents")"
git_canon="$(canonical_path "$HOME/git")"

if [[ "$allow_real_roots" != "1" ]]; then
  [[ "$neo_canon" != "/" ]] || fail "refusing to use filesystem root as neo root"
  [[ "$neo_canon" != "$home_canon" ]] || fail "refusing to use HOME as neo root"
  [[ "$neo_canon" != "$documents_canon" ]] || fail "refusing to use real Documents as neo root"
  [[ "$neo_canon" != "$git_canon" ]] || fail "refusing to use real git as neo root"
fi

mkdir -p "$evidence_dir"
if [[ -z "$state_dir" ]]; then
  state_dir="$evidence_dir/neo-state"
fi
mkdir -p "$state_dir"

state_json="$state_dir/state.json"
cache_root="$state_dir/cache"
config_path="$state_dir/tcfs-neo-honey-conflict.toml"
device_registry="$state_dir/device-registry.json"
mc_config_dir="$state_dir/mc"
fixture_file="Projects/shared/conflict-notes.md"
conflict_copy_file="Projects/shared/conflict-notes.conflict-honey.md"
daemon_conflict_copy_file="Projects/shared/conflict-notes.conflict-00000000-0000-4000-8000-0000000000b2.md"
sibling_file="Projects/shared/conflict-independent-sibling.md"
fixture_path="$neo_canon/$fixture_file"
sibling_path="$neo_canon/$sibling_file"
base_content_file="$evidence_dir/base-content.txt"
neo_content_file="$evidence_dir/neo-conflict-content.txt"
honey_content_file="$evidence_dir/honey-conflict-content.txt"
sibling_base_content_file="$evidence_dir/sibling-base-content.txt"
honey_sibling_content_file="$evidence_dir/honey-sibling-content.txt"
remote_after_conflict_file="$evidence_dir/remote-after-conflict.content"
remote_original_after_recovery_file="$evidence_dir/remote-original-after-recovery.content"
remote_conflict_copy_file="$evidence_dir/remote-conflict-copy.content"
remote_original_after_daemon_resolve_file="$evidence_dir/remote-original-after-daemon-resolve.content"
remote_daemon_conflict_copy_file="$evidence_dir/remote-daemon-conflict-copy.content"
remote_sibling_after_progress_file="$evidence_dir/remote-sibling-after-progress.content"
local_tree="$evidence_dir/neo-tree.txt"
neo_initial_push_log="$evidence_dir/neo-initial-push.log"
neo_sibling_initial_push_log="$evidence_dir/neo-sibling-initial-push.log"
neo_conflict_push_log="$evidence_dir/neo-conflict-push.log"
remote_pullback_log="$evidence_dir/remote-after-conflict-pull.log"
remote_original_after_recovery_log="$evidence_dir/remote-original-after-recovery-pull.log"
remote_conflict_copy_log="$evidence_dir/remote-conflict-copy-pull.log"
remote_original_after_daemon_resolve_log="$evidence_dir/remote-original-after-daemon-resolve-pull.log"
remote_daemon_conflict_copy_log="$evidence_dir/remote-daemon-conflict-copy-pull.log"
remote_sibling_after_progress_log="$evidence_dir/remote-sibling-after-progress-pull.log"
honey_script="$evidence_dir/honey-conflict-run.sh"
honey_commands="$evidence_dir/honey-conflict-commands.txt"
honey_prepare_log="$evidence_dir/honey-prepare.log"
honey_conflict_log="$evidence_dir/honey-conflict-push.log"
honey_recovery_log="$evidence_dir/honey-keep-both-recovery.log"
honey_daemon_resolve_log="$evidence_dir/honey-daemon-resolve-keep-both.log"
honey_sibling_log="$evidence_dir/honey-independent-sibling-push.log"
honey_evidence_dir="$evidence_dir/honey-evidence"
result_env="$evidence_dir/result.env"

tcfs_cmd=()
if [[ -n "$tcfs_bin" ]]; then
  [[ -x "$tcfs_bin" ]] || fail "--tcfs-bin is not executable: $tcfs_bin"
  tcfs_cmd=("$tcfs_bin")
elif [[ -x "$REPO_ROOT/target/debug/tcfs" ]]; then
  tcfs_cmd=("$REPO_ROOT/target/debug/tcfs")
else
  tcfs_cmd=(cargo run --quiet -p tcfs-cli --)
fi

if [[ "$push_remote" == "1" ]]; then
  if [[ -z "${AWS_ACCESS_KEY_ID:-}" && "$endpoint" =~ ^http://(localhost|127\.0\.0\.1)(:[0-9]+)?$ ]]; then
    export AWS_ACCESS_KEY_ID=admin
    printf 'using local dev-stack default AWS_ACCESS_KEY_ID=admin\n'
  fi
  if [[ -z "${AWS_SECRET_ACCESS_KEY:-}" && "$endpoint" =~ ^http://(localhost|127\.0\.0\.1)(:[0-9]+)?$ ]]; then
    export AWS_SECRET_ACCESS_KEY=admin
    printf 'using local dev-stack default AWS_SECRET_ACCESS_KEY=admin\n'
  fi
  [[ -n "${AWS_ACCESS_KEY_ID:-}" ]] || fail "set AWS_ACCESS_KEY_ID for $endpoint"
  [[ -n "${AWS_SECRET_ACCESS_KEY:-}" ]] || fail "set AWS_SECRET_ACCESS_KEY for $endpoint"
fi

create_bucket_if_requested() {
  [[ "$create_bucket" == "1" ]] || return 0

  if command -v aws >/dev/null 2>&1; then
    aws --endpoint-url "$endpoint" s3 mb "s3://$bucket" >/dev/null 2>&1 || true
    return 0
  fi
  if command -v s5cmd >/dev/null 2>&1; then
    S3_ENDPOINT_URL="$endpoint" s5cmd mb "s3://$bucket" >/dev/null 2>&1 || true
    return 0
  fi
  if command -v mc >/dev/null 2>&1; then
    mkdir -p "$mc_config_dir"
    mc --config-dir "$mc_config_dir" alias set tcfs-conflict "$endpoint" "$AWS_ACCESS_KEY_ID" "$AWS_SECRET_ACCESS_KEY" >/dev/null
    mc --config-dir "$mc_config_dir" mb --ignore-existing "tcfs-conflict/$bucket" >/dev/null
    return 0
  fi

  fail "--create-bucket requested, but none of aws, s5cmd, or mc was found"
}

mkdir -p "$neo_canon/Projects/shared" "$cache_root" "$honey_evidence_dir"
write_device_registry "$device_registry"

cat >"$base_content_file" <<'EOF'
# Conflict shared TCFS note

version: base
body: both peers start from this content before diverging.
EOF

cat >"$neo_content_file" <<'EOF'
# Conflict shared TCFS note

version: neo-conflict
body: neo pushed this divergent version before honey attempted its push.
EOF

cat >"$honey_content_file" <<'EOF'
# Conflict shared TCFS note

version: honey-conflict
body: honey kept these bytes locally when conflict detection skipped upload.
EOF

cat >"$sibling_base_content_file" <<'EOF'
# Independent sibling TCFS note

version: sibling-base
body: both peers start with this sibling before only honey edits it.
EOF

cat >"$honey_sibling_content_file" <<'EOF'
# Independent sibling TCFS note

version: honey-sibling
body: honey should be able to publish this sibling even while another descendant is conflicted.
EOF

cp "$base_content_file" "$fixture_path"
if [[ "$honey_independent_sibling" == "1" ]]; then
  cp "$sibling_base_content_file" "$sibling_path"
fi
find "$neo_canon" -maxdepth 8 -print | sort >"$local_tree"

cat >"$config_path" <<EOF
[daemon]
socket = "$state_dir/tcfsd.sock"
metrics_addr = "127.0.0.1:0"

[storage]
endpoint = "$endpoint"
region = "$region"
bucket = "$bucket"
remote_prefix = "$prefix"
enforce_tls = false

[sync]
state_db = "$state_dir/state.db"
sync_root = "$neo_canon"
nats_url = "nats://localhost:4222"
nats_tls = false
sync_empty_dirs = true
device_identity = "$device_registry"
device_name = "neo-conflict"
reconcile_interval_secs = 0

[fuse]
cache_dir = "$cache_root"
cache_max_mb = 64
negative_cache_ttl_secs = 1

[crypto]
enabled = false

[auth]
require_session = false
EOF

cat >"$honey_script" <<EOF
#!/usr/bin/env bash
set -euo pipefail

ENDPOINT=$(shell_quote "$endpoint")
REGION=$(shell_quote "$region")
BUCKET=$(shell_quote "$bucket")
PREFIX=$(shell_quote "$prefix")
TCFS_BIN=$(shell_quote "$honey_tcfs_bin")
TCFSD_BIN=$(shell_quote "$honey_tcfsd_bin")
HONEY_ROOT_RAW=$(single_quote "$honey_root")
RUN_DIR=$(shell_quote "$honey_remote_dir")
FIXTURE_FILE=$(shell_quote "$fixture_file")
CONFLICT_COPY_FILE=$(shell_quote "$conflict_copy_file")
DAEMON_CONFLICT_COPY_FILE=$(shell_quote "$daemon_conflict_copy_file")
SIBLING_FILE=$(shell_quote "$sibling_file")
HONEY_INDEPENDENT_SIBLING=$(shell_quote "$honey_independent_sibling")
BASE_CONTENT="\${TCFS_HONEY_BASE_CONTENT_FILE:-$(shell_quote "$honey_remote_dir/base-content.txt")}"
NEO_CONTENT="\${TCFS_HONEY_NEO_CONTENT_FILE:-$(shell_quote "$honey_remote_dir/neo-conflict-content.txt")}"
HONEY_CONTENT="\${TCFS_HONEY_CONFLICT_CONTENT_FILE:-$(shell_quote "$honey_remote_dir/honey-conflict-content.txt")}"
SIBLING_BASE_CONTENT="\${TCFS_HONEY_SIBLING_BASE_CONTENT_FILE:-$(shell_quote "$honey_remote_dir/sibling-base-content.txt")}"
HONEY_SIBLING_CONTENT="\${TCFS_HONEY_SIBLING_CONTENT_FILE:-$(shell_quote "$honey_remote_dir/honey-sibling-content.txt")}"

case "\$HONEY_ROOT_RAW" in
  "~/"*) HONEY_ROOT="\${HOME}/\${HONEY_ROOT_RAW#\\~/}" ;;
  *) HONEY_ROOT="\$HONEY_ROOT_RAW" ;;
esac

if [[ -n "\${TCFS_HONEY_ENV_FILE:-}" ]]; then
  # shellcheck disable=SC1090
  source "\$TCFS_HONEY_ENV_FILE"
fi

mode="\${1:-}"
[[ -n "\$mode" ]] || { echo "mode required: prepare, push-conflict, push-sibling, recover-keep-both, or resolve-keep-both" >&2; exit 2; }

STATE_DIR="\$RUN_DIR/honey-state"
CACHE_ROOT="\$STATE_DIR/cache"
EVIDENCE_DIR="\$RUN_DIR/honey-evidence"
CONFIG_PATH="\$STATE_DIR/tcfs-neo-honey-conflict.toml"
STATE_JSON="\$STATE_DIR/state.json"
DEVICE_REGISTRY="\$RUN_DIR/device-registry.json"
FIXTURE_PATH="\$HONEY_ROOT/\$FIXTURE_FILE"
CONFLICT_COPY_PATH="\$HONEY_ROOT/\$CONFLICT_COPY_FILE"
DAEMON_CONFLICT_COPY_PATH="\$HONEY_ROOT/\$DAEMON_CONFLICT_COPY_FILE"
SIBLING_PATH="\$HONEY_ROOT/\$SIBLING_FILE"

mkdir -p "\$(dirname "\$FIXTURE_PATH")" "\$CACHE_ROOT" "\$EVIDENCE_DIR"

cat >"\$CONFIG_PATH" <<REMOTE_CONFIG
[daemon]
socket = "\$STATE_DIR/tcfsd.sock"
metrics_addr = "127.0.0.1:0"

[storage]
endpoint = "\$ENDPOINT"
region = "\$REGION"
bucket = "\$BUCKET"
remote_prefix = "\$PREFIX"
enforce_tls = false

[sync]
state_db = "\$STATE_DIR/state.db"
sync_root = "\$HONEY_ROOT"
nats_url = "nats://localhost:4222"
nats_tls = false
sync_empty_dirs = true
device_identity = "\$DEVICE_REGISTRY"
device_name = "honey-conflict"
reconcile_interval_secs = 0

[fuse]
cache_dir = "\$CACHE_ROOT"
cache_max_mb = 64
negative_cache_ttl_secs = 1

[crypto]
enabled = false

[auth]
require_session = false
REMOTE_CONFIG

write_daemon_resolve_blocker() {
  local reason="\$1"
  local detail="\${2:-}"
  if [[ -n "\$detail" ]]; then
    printf '%s\n' "\$detail" >"\$EVIDENCE_DIR/honey-daemon-resolve-blocker.txt"
  fi
  {
    echo "daemon_resolve_keep_both=blocked"
    echo "blocker_reason=\$reason"
    echo "daemon_auth_bypass_required=1"
    echo "conflict_copy_path=\$DAEMON_CONFLICT_COPY_FILE"
  } >"\$EVIDENCE_DIR/honey-daemon-resolve-result.env"
  echo "daemon_resolve_keep_both=blocked"
  echo "blocker_reason=\$reason"
  echo "daemon_auth_bypass_required=1"
  echo "conflict_copy_path=\$DAEMON_CONFLICT_COPY_FILE"
}

stop_resolve_daemon() {
  if [[ -n "\${DAEMON_PID:-}" ]]; then
    if kill -0 "\$DAEMON_PID" >/dev/null 2>&1; then
      kill "\$DAEMON_PID" >/dev/null 2>&1 || true
      wait "\$DAEMON_PID" >/dev/null 2>&1 || true
    fi
    DAEMON_PID=""
  fi
}

start_resolve_daemon() {
  if [[ "\$TCFSD_BIN" == */* ]]; then
    [[ -x "\$TCFSD_BIN" ]] || return 10
  else
    command -v "\$TCFSD_BIN" >/dev/null 2>&1 || return 10
  fi

  rm -f "\$STATE_DIR/tcfsd.sock"
  mkdir -p "\$RUN_DIR/xdg-data" "\$RUN_DIR/xdg-state"
  XDG_DATA_HOME="\$RUN_DIR/xdg-data" XDG_STATE_HOME="\$RUN_DIR/xdg-state" "\$TCFSD_BIN" --config "\$CONFIG_PATH" --mode daemon --log debug --log-format text >"\$EVIDENCE_DIR/honey-tcfsd-resolve-keep-both.log" 2>&1 &
  DAEMON_PID="\$!"

  for _ in {1..100}; do
    if [[ -S "\$STATE_DIR/tcfsd.sock" ]]; then
      return 0
    fi
    if ! kill -0 "\$DAEMON_PID" >/dev/null 2>&1; then
      return 11
    fi
    sleep 0.1
  done
  return 12
}

run_with_timeout() {
  local timeout_secs="\$1"
  shift
  "\$@" &
  local child_pid="\$!"
  local waited=0

  while [[ "\$waited" -lt "\$timeout_secs" ]]; do
    if ! kill -0 "\$child_pid" >/dev/null 2>&1; then
      wait "\$child_pid"
      return "\$?"
    fi
    sleep 1
    waited=\$((waited + 1))
  done

  kill "\$child_pid" >/dev/null 2>&1 || true
  wait "\$child_pid" >/dev/null 2>&1 || true
  return 124
}

case "\$mode" in
  prepare)
    "\$TCFS_BIN" --config "\$CONFIG_PATH" pull "\$FIXTURE_FILE" "\$FIXTURE_PATH" --prefix "\$PREFIX" --state "\$STATE_JSON" >"\$EVIDENCE_DIR/honey-initial-pull.log" 2>&1
    cmp -s "\$BASE_CONTENT" "\$FIXTURE_PATH"
    cp "\$HONEY_CONTENT" "\$FIXTURE_PATH"
    "\$TCFS_BIN" --config "\$CONFIG_PATH" sync-status "\$FIXTURE_PATH" >"\$EVIDENCE_DIR/honey-sync-status-before-conflict.out" 2>&1
    if [[ "\$HONEY_INDEPENDENT_SIBLING" == "1" ]]; then
      "\$TCFS_BIN" --config "\$CONFIG_PATH" pull "\$SIBLING_FILE" "\$SIBLING_PATH" --prefix "\$PREFIX" --state "\$STATE_JSON" >"\$EVIDENCE_DIR/honey-sibling-initial-pull.log" 2>&1
      cmp -s "\$SIBLING_BASE_CONTENT" "\$SIBLING_PATH"
      cp "\$HONEY_SIBLING_CONTENT" "\$SIBLING_PATH"
      "\$TCFS_BIN" --config "\$CONFIG_PATH" sync-status "\$SIBLING_PATH" >"\$EVIDENCE_DIR/honey-sibling-sync-status-before-push.out" 2>&1
    fi
    echo "honey conflict prepare ok: \$FIXTURE_FILE"
    ;;
  push-conflict)
    set +e
    "\$TCFS_BIN" --config "\$CONFIG_PATH" push "\$FIXTURE_PATH" --prefix "\$PREFIX" --state "\$STATE_JSON" >"\$EVIDENCE_DIR/honey-conflict-push.log" 2>&1
    push_rc="\$?"
    set -e
    cat "\$EVIDENCE_DIR/honey-conflict-push.log"
    [[ "\$push_rc" -eq 0 ]] || { echo "honey conflict push command failed: \$push_rc" >&2; exit "\$push_rc"; }
    grep -q "CONFLICT:" "\$EVIDENCE_DIR/honey-conflict-push.log"
    grep -q "skipped (unchanged since last sync)" "\$EVIDENCE_DIR/honey-conflict-push.log"
    cmp -s "\$HONEY_CONTENT" "\$FIXTURE_PATH"
    "\$TCFS_BIN" --config "\$CONFIG_PATH" sync-status "\$FIXTURE_PATH" >"\$EVIDENCE_DIR/honey-sync-status-after-conflict.out" 2>&1
    grep -q "sync state: conflict" "\$EVIDENCE_DIR/honey-sync-status-after-conflict.out"
    {
      echo "honey_push_conflict=detected"
      echo "honey_local_content=preserved"
      echo "honey_sync_state=conflict"
    } >"\$EVIDENCE_DIR/honey-conflict-result.env"
    echo "honey conflict push ok: \$FIXTURE_FILE"
    echo "honey_push_conflict=detected"
    echo "honey_local_content=preserved"
    echo "honey_sync_state=conflict"
    ;;
  push-sibling)
    [[ "\$HONEY_INDEPENDENT_SIBLING" == "1" ]] || { echo "independent sibling mode is disabled" >&2; exit 2; }
    "\$TCFS_BIN" --config "\$CONFIG_PATH" push "\$SIBLING_PATH" --prefix "\$PREFIX" --state "\$STATE_JSON" >"\$EVIDENCE_DIR/honey-independent-sibling-push.log" 2>&1
    cat "\$EVIDENCE_DIR/honey-independent-sibling-push.log"
    if grep -q "CONFLICT:" "\$EVIDENCE_DIR/honey-independent-sibling-push.log"; then
      echo "unexpected conflict while pushing independent sibling" >&2
      exit 1
    fi
    cmp -s "\$HONEY_SIBLING_CONTENT" "\$SIBLING_PATH"
    "\$TCFS_BIN" --config "\$CONFIG_PATH" sync-status "\$SIBLING_PATH" >"\$EVIDENCE_DIR/honey-sibling-sync-status-after-push.out" 2>&1
    grep -q "sync state: synced" "\$EVIDENCE_DIR/honey-sibling-sync-status-after-push.out"
    {
      echo "independent_sibling_push=completed"
      echo "independent_sibling_content=honey_bytes"
      echo "independent_sibling_conflict=absent"
    } >"\$EVIDENCE_DIR/honey-independent-sibling-result.env"
    echo "honey independent sibling push ok: \$SIBLING_FILE"
    echo "independent_sibling_push=completed"
    echo "independent_sibling_content=honey_bytes"
    echo "independent_sibling_conflict=absent"
    ;;
  recover-keep-both)
    [[ -f "\$NEO_CONTENT" ]] || { echo "missing neo content fixture: \$NEO_CONTENT" >&2; exit 2; }
    "\$TCFS_BIN" --config "\$CONFIG_PATH" sync-status "\$FIXTURE_PATH" >"\$EVIDENCE_DIR/honey-sync-status-before-recovery.out" 2>&1
    grep -q "sync state: conflict" "\$EVIDENCE_DIR/honey-sync-status-before-recovery.out"
    mkdir -p "\$(dirname "\$CONFLICT_COPY_PATH")"
    cp "\$FIXTURE_PATH" "\$CONFLICT_COPY_PATH"
    cmp -s "\$HONEY_CONTENT" "\$CONFLICT_COPY_PATH"
    "\$TCFS_BIN" --config "\$CONFIG_PATH" pull "\$FIXTURE_FILE" "\$FIXTURE_PATH" --prefix "\$PREFIX" --state "\$STATE_JSON" >"\$EVIDENCE_DIR/honey-recover-original-pull.log" 2>&1
    cmp -s "\$NEO_CONTENT" "\$FIXTURE_PATH"
    "\$TCFS_BIN" --config "\$CONFIG_PATH" sync-status "\$FIXTURE_PATH" >"\$EVIDENCE_DIR/honey-sync-status-after-original-recovery.out" 2>&1
    grep -q "sync state: synced" "\$EVIDENCE_DIR/honey-sync-status-after-original-recovery.out"
    "\$TCFS_BIN" --config "\$CONFIG_PATH" push "\$CONFLICT_COPY_PATH" --prefix "\$PREFIX" --state "\$STATE_JSON" >"\$EVIDENCE_DIR/honey-recover-copy-push.log" 2>&1
    cmp -s "\$HONEY_CONTENT" "\$CONFLICT_COPY_PATH"
    "\$TCFS_BIN" --config "\$CONFIG_PATH" sync-status "\$CONFLICT_COPY_PATH" >"\$EVIDENCE_DIR/honey-sync-status-after-copy-push.out" 2>&1
    grep -q "sync state: synced" "\$EVIDENCE_DIR/honey-sync-status-after-copy-push.out"
    {
      echo "keep_both_recovery=completed"
      echo "original_path_after_recovery=remote_neo_bytes"
      echo "conflict_copy_path=\$CONFLICT_COPY_FILE"
      echo "conflict_copy_content=honey_bytes"
      echo "conflict_copy_pushed=1"
    } >"\$EVIDENCE_DIR/honey-recovery-result.env"
    echo "honey keep-both recovery ok: \$FIXTURE_FILE -> \$CONFLICT_COPY_FILE"
    echo "keep_both_recovery=completed"
    echo "original_path_after_recovery=remote_neo_bytes"
    echo "conflict_copy_path=\$CONFLICT_COPY_FILE"
    echo "conflict_copy_content=honey_bytes"
    echo "conflict_copy_pushed=1"
    ;;
  resolve-keep-both)
    "\$TCFS_BIN" --config "\$CONFIG_PATH" sync-status "\$FIXTURE_PATH" >"\$EVIDENCE_DIR/honey-sync-status-before-daemon-resolve.out" 2>&1 || {
      write_daemon_resolve_blocker "pre_resolve_status_failed" "tcfs sync-status failed before daemon resolve"
      exit 0
    }
    if ! grep -q "sync state: conflict" "\$EVIDENCE_DIR/honey-sync-status-before-daemon-resolve.out"; then
      write_daemon_resolve_blocker "pre_resolve_state_not_conflict" "expected conflict state before daemon resolve"
      exit 0
    fi

    DAEMON_PID=""
    trap stop_resolve_daemon EXIT
    if start_resolve_daemon; then
      :
    else
      daemon_rc="\$?"
      write_daemon_resolve_blocker "daemon_start_or_socket_failed" "tcfsd start/socket wait failed with rc=\$daemon_rc; see honey-tcfsd-resolve-keep-both.log"
      exit 0
    fi

    set +e
    run_with_timeout 30 "\$TCFS_BIN" --config "\$CONFIG_PATH" resolve "\$FIXTURE_PATH" --strategy keep-both >"\$EVIDENCE_DIR/honey-daemon-resolve-keep-both.out" 2>&1
    resolve_rc="\$?"
    set -e
    cat "\$EVIDENCE_DIR/honey-daemon-resolve-keep-both.out"
    stop_resolve_daemon
    trap - EXIT
    if [[ "\$resolve_rc" -eq 124 ]]; then
      write_daemon_resolve_blocker "resolve_command_timeout" "tcfs resolve did not return within 30s after daemon accepted the request"
      exit 0
    fi
    if [[ "\$resolve_rc" -ne 0 ]]; then
      write_daemon_resolve_blocker "resolve_command_failed" "tcfs resolve returned rc=\$resolve_rc"
      exit 0
    fi

    if ! cmp -s "\$NEO_CONTENT" "\$FIXTURE_PATH"; then
      write_daemon_resolve_blocker "original_content_mismatch" "original path did not contain neo remote bytes after daemon resolve"
      exit 0
    fi
    if [[ ! -f "\$DAEMON_CONFLICT_COPY_PATH" ]]; then
      write_daemon_resolve_blocker "conflict_copy_missing" "daemon conflict copy was not created at \$DAEMON_CONFLICT_COPY_FILE"
      exit 0
    fi
    if ! cmp -s "\$HONEY_CONTENT" "\$DAEMON_CONFLICT_COPY_PATH"; then
      write_daemon_resolve_blocker "conflict_copy_content_mismatch" "daemon conflict copy did not preserve honey bytes"
      exit 0
    fi
    "\$TCFS_BIN" --config "\$CONFIG_PATH" sync-status "\$FIXTURE_PATH" >"\$EVIDENCE_DIR/honey-sync-status-after-daemon-resolve-original.out" 2>&1 || {
      write_daemon_resolve_blocker "post_original_status_failed" "tcfs sync-status failed for original after daemon resolve"
      exit 0
    }
    if ! grep -q "sync state: synced" "\$EVIDENCE_DIR/honey-sync-status-after-daemon-resolve-original.out"; then
      write_daemon_resolve_blocker "post_original_not_synced" "original path was not synced after daemon resolve"
      exit 0
    fi
    "\$TCFS_BIN" --config "\$CONFIG_PATH" sync-status "\$DAEMON_CONFLICT_COPY_PATH" >"\$EVIDENCE_DIR/honey-sync-status-after-daemon-resolve-copy.out" 2>&1 || {
      write_daemon_resolve_blocker "post_copy_status_failed" "tcfs sync-status failed for conflict copy after daemon resolve"
      exit 0
    }
    if ! grep -q "sync state: synced" "\$EVIDENCE_DIR/honey-sync-status-after-daemon-resolve-copy.out"; then
      write_daemon_resolve_blocker "post_copy_not_synced" "conflict copy was not synced after daemon resolve"
      exit 0
    fi

    {
      echo "daemon_resolve_keep_both=completed"
      echo "daemon_auth_bypass_required=1"
      echo "original_path_after_resolve=remote_neo_bytes"
      echo "conflict_copy_path=\$DAEMON_CONFLICT_COPY_FILE"
      echo "conflict_copy_content=honey_bytes"
      echo "conflict_copy_pushed=1"
    } >"\$EVIDENCE_DIR/honey-daemon-resolve-result.env"
    echo "honey daemon resolve keep-both ok: \$FIXTURE_FILE -> \$DAEMON_CONFLICT_COPY_FILE"
    echo "daemon_resolve_keep_both=completed"
    echo "daemon_auth_bypass_required=1"
    echo "original_path_after_resolve=remote_neo_bytes"
    echo "conflict_copy_path=\$DAEMON_CONFLICT_COPY_FILE"
    echo "conflict_copy_content=honey_bytes"
    echo "conflict_copy_pushed=1"
    ;;
  *)
    echo "unknown mode: \$mode" >&2
    exit 2
    ;;
esac
EOF
chmod +x "$honey_script"

cat >"$honey_commands" <<EOF
# Prepare remote work directory:
ssh $(shell_quote "$honey_host") 'mkdir -p $(shell_quote "$honey_remote_dir")'

# Copy expected content and the honey conflict runner:
scp $(shell_quote "$base_content_file") $(shell_quote "$honey_host"):$(shell_quote "$honey_remote_dir/base-content.txt")
scp $(shell_quote "$neo_content_file") $(shell_quote "$honey_host"):$(shell_quote "$honey_remote_dir/neo-conflict-content.txt")
scp $(shell_quote "$honey_content_file") $(shell_quote "$honey_host"):$(shell_quote "$honey_remote_dir/honey-conflict-content.txt")
scp $(shell_quote "$sibling_base_content_file") $(shell_quote "$honey_host"):$(shell_quote "$honey_remote_dir/sibling-base-content.txt")
scp $(shell_quote "$honey_sibling_content_file") $(shell_quote "$honey_host"):$(shell_quote "$honey_remote_dir/honey-sibling-content.txt")
scp $(shell_quote "$device_registry") $(shell_quote "$honey_host"):$(shell_quote "$honey_remote_dir/device-registry.json")
scp $(shell_quote "$honey_script") $(shell_quote "$honey_host"):$(shell_quote "$honey_remote_dir/honey-conflict-run.sh")

# Honey pulls base and edits locally:
ssh $(shell_quote "$honey_host") 'TCFS_HONEY_BASE_CONTENT_FILE=$(shell_quote "$honey_remote_dir/base-content.txt") TCFS_HONEY_CONFLICT_CONTENT_FILE=$(shell_quote "$honey_remote_dir/honey-conflict-content.txt") TCFS_HONEY_SIBLING_BASE_CONTENT_FILE=$(shell_quote "$honey_remote_dir/sibling-base-content.txt") TCFS_HONEY_SIBLING_CONTENT_FILE=$(shell_quote "$honey_remote_dir/honey-sibling-content.txt") bash $(shell_quote "$honey_remote_dir/honey-conflict-run.sh") prepare'

# After neo pushes divergent content, honey attempts push and must record conflict:
ssh $(shell_quote "$honey_host") 'TCFS_HONEY_CONFLICT_CONTENT_FILE=$(shell_quote "$honey_remote_dir/honey-conflict-content.txt") bash $(shell_quote "$honey_remote_dir/honey-conflict-run.sh") push-conflict'

# Optional independent sibling push after conflict detection:
ssh $(shell_quote "$honey_host") 'TCFS_HONEY_SIBLING_CONTENT_FILE=$(shell_quote "$honey_remote_dir/honey-sibling-content.txt") bash $(shell_quote "$honey_remote_dir/honey-conflict-run.sh") push-sibling'

# Optional manual keep-both recovery after conflict detection:
ssh $(shell_quote "$honey_host") 'TCFS_HONEY_NEO_CONTENT_FILE=$(shell_quote "$honey_remote_dir/neo-conflict-content.txt") TCFS_HONEY_CONFLICT_CONTENT_FILE=$(shell_quote "$honey_remote_dir/honey-conflict-content.txt") bash $(shell_quote "$honey_remote_dir/honey-conflict-run.sh") recover-keep-both'

# Optional daemon-backed keep-both resolve after conflict detection:
ssh $(shell_quote "$honey_host") 'TCFS_HONEY_NEO_CONTENT_FILE=$(shell_quote "$honey_remote_dir/neo-conflict-content.txt") TCFS_HONEY_CONFLICT_CONTENT_FILE=$(shell_quote "$honey_remote_dir/honey-conflict-content.txt") bash $(shell_quote "$honey_remote_dir/honey-conflict-run.sh") resolve-keep-both'
EOF

cat >"$evidence_dir/run-metadata.env" <<EOF
created_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
run_id=$run_id
remote=$remote
endpoint=$endpoint
bucket=$bucket
prefix=$prefix
neo_root=$neo_canon
fixture_file=$fixture_file
state_dir=$state_dir
honey_host=$honey_host
honey_root=$honey_root
honey_remote_dir=$honey_remote_dir
honey_tcfs_bin=$honey_tcfs_bin
honey_tcfsd_bin=$honey_tcfsd_bin
push=$push_remote
run_honey=$run_honey
honey_recover_keep_both=$honey_recover_keep_both
honey_independent_sibling=$honey_independent_sibling
honey_resolve_keep_both=$honey_resolve_keep_both
conflict_copy_file=$conflict_copy_file
daemon_conflict_copy_file=$daemon_conflict_copy_file
sibling_file=$sibling_file
allow_real_roots=$allow_real_roots
EOF
cp "$config_path" "$evidence_dir/tcfs-neo-honey-conflict.toml"

if [[ "$push_remote" == "1" ]]; then
  create_bucket_if_requested
  printf 'pushing initial neo fixture to %s\n' "$remote"
  "${tcfs_cmd[@]}" --config "$config_path" push "$fixture_path" --prefix "$prefix" --state "$state_json" >"$neo_initial_push_log" 2>&1
  if [[ "$honey_independent_sibling" == "1" ]]; then
    "${tcfs_cmd[@]}" --config "$config_path" push "$sibling_path" --prefix "$prefix" --state "$state_json" >"$neo_sibling_initial_push_log" 2>&1
  fi
else
  printf 'plan-only: conflict fixture created but not pushed. Re-run with --push --run-honey when ready.\n'
fi

if [[ "$run_honey" == "1" ]]; then
  [[ "$push_remote" == "1" ]] || fail "--run-honey requires --push"
  command -v ssh >/dev/null 2>&1 || fail "ssh not found"
  command -v scp >/dev/null 2>&1 || fail "scp not found"

  printf 'running honey conflict prepare on %s\n' "$honey_host"
  # shellcheck disable=SC2029
  ssh "$honey_host" "mkdir -p $(shell_quote "$honey_remote_dir")"
  scp "$base_content_file" "$honey_host:$honey_remote_dir/base-content.txt"
  scp "$neo_content_file" "$honey_host:$honey_remote_dir/neo-conflict-content.txt"
  scp "$honey_content_file" "$honey_host:$honey_remote_dir/honey-conflict-content.txt"
  scp "$sibling_base_content_file" "$honey_host:$honey_remote_dir/sibling-base-content.txt"
  scp "$honey_sibling_content_file" "$honey_host:$honey_remote_dir/honey-sibling-content.txt"
  scp "$device_registry" "$honey_host:$honey_remote_dir/device-registry.json"
  scp "$honey_script" "$honey_host:$honey_remote_dir/honey-conflict-run.sh"

  remote_env_file=""
  cleanup_remote_env() {
    [[ -n "$remote_env_file" ]] || return 0
    # shellcheck disable=SC2029
    ssh "$honey_host" "rm -f $(shell_quote "$remote_env_file")" >/dev/null 2>&1 || true
    remote_env_file=""
  }

  if [[ "$forward_aws_env" == "1" ]]; then
    [[ -n "${AWS_ACCESS_KEY_ID:-}" ]] || fail "--forward-aws-env requires AWS_ACCESS_KEY_ID"
    [[ -n "${AWS_SECRET_ACCESS_KEY:-}" ]] || fail "--forward-aws-env requires AWS_SECRET_ACCESS_KEY"
    remote_env_file="$honey_remote_dir/aws-env.sh"
    aws_env_payload="$(printf 'export AWS_ACCESS_KEY_ID=%q\nexport AWS_SECRET_ACCESS_KEY=%q\n' "$AWS_ACCESS_KEY_ID" "$AWS_SECRET_ACCESS_KEY")"
    # shellcheck disable=SC2029
    ssh "$honey_host" "umask 077; cat > $(shell_quote "$remote_env_file")" <<<"$aws_env_payload"
  fi

  prepare_cmd="$(printf 'TCFS_HONEY_BASE_CONTENT_FILE=%q TCFS_HONEY_CONFLICT_CONTENT_FILE=%q TCFS_HONEY_SIBLING_BASE_CONTENT_FILE=%q TCFS_HONEY_SIBLING_CONTENT_FILE=%q bash %q prepare' \
    "$honey_remote_dir/base-content.txt" \
    "$honey_remote_dir/honey-conflict-content.txt" \
    "$honey_remote_dir/sibling-base-content.txt" \
    "$honey_remote_dir/honey-sibling-content.txt" \
    "$honey_remote_dir/honey-conflict-run.sh")"
  conflict_cmd="$(printf 'TCFS_HONEY_CONFLICT_CONTENT_FILE=%q bash %q push-conflict' \
    "$honey_remote_dir/honey-conflict-content.txt" \
    "$honey_remote_dir/honey-conflict-run.sh")"
  sibling_cmd="$(printf 'TCFS_HONEY_SIBLING_CONTENT_FILE=%q bash %q push-sibling' \
    "$honey_remote_dir/honey-sibling-content.txt" \
    "$honey_remote_dir/honey-conflict-run.sh")"
  recovery_cmd="$(printf 'TCFS_HONEY_NEO_CONTENT_FILE=%q TCFS_HONEY_CONFLICT_CONTENT_FILE=%q bash %q recover-keep-both' \
    "$honey_remote_dir/neo-conflict-content.txt" \
    "$honey_remote_dir/honey-conflict-content.txt" \
    "$honey_remote_dir/honey-conflict-run.sh")"
  resolve_cmd="$(printf 'TCFS_HONEY_NEO_CONTENT_FILE=%q TCFS_HONEY_CONFLICT_CONTENT_FILE=%q bash %q resolve-keep-both' \
    "$honey_remote_dir/neo-conflict-content.txt" \
    "$honey_remote_dir/honey-conflict-content.txt" \
    "$honey_remote_dir/honey-conflict-run.sh")"
  if [[ -n "$remote_env_file" ]]; then
    prepare_cmd="$(printf 'TCFS_HONEY_ENV_FILE=%q %s' "$remote_env_file" "$prepare_cmd")"
    conflict_cmd="$(printf 'TCFS_HONEY_ENV_FILE=%q %s' "$remote_env_file" "$conflict_cmd")"
    sibling_cmd="$(printf 'TCFS_HONEY_ENV_FILE=%q %s' "$remote_env_file" "$sibling_cmd")"
    recovery_cmd="$(printf 'TCFS_HONEY_ENV_FILE=%q %s' "$remote_env_file" "$recovery_cmd")"
    resolve_cmd="$(printf 'TCFS_HONEY_ENV_FILE=%q %s' "$remote_env_file" "$resolve_cmd")"
  fi

  remote_status=0
  # shellcheck disable=SC2029
  ssh "$honey_host" "$prepare_cmd" | tee "$honey_prepare_log" || remote_status=$?
  if [[ "$remote_status" -ne 0 ]]; then
    cleanup_remote_env
    printf 'honey prepare failed; see %s\n' "$honey_prepare_log" >&2
    exit "$remote_status"
  fi

  printf 'mutating and pushing neo divergent fixture\n'
  cp "$neo_content_file" "$fixture_path"
  "${tcfs_cmd[@]}" --config "$config_path" push "$fixture_path" --prefix "$prefix" --state "$state_json" >"$neo_conflict_push_log" 2>&1

  remote_status=0
  # shellcheck disable=SC2029
  ssh "$honey_host" "$conflict_cmd" | tee "$honey_conflict_log" || remote_status=$?
  if [[ "$remote_status" -ne 0 ]]; then
    scp "$honey_host:$honey_remote_dir/honey-evidence/"* "$honey_evidence_dir/" >/dev/null 2>&1 || true
    cleanup_remote_env
    printf 'honey conflict push failed; see %s\n' "$honey_conflict_log" >&2
    exit "$remote_status"
  fi
  grep -q "honey_push_conflict=detected" "$honey_conflict_log" || fail "honey conflict log missing conflict marker"
  grep -q "honey_local_content=preserved" "$honey_conflict_log" || fail "honey conflict log missing local preservation marker"
  grep -q "honey_sync_state=conflict" "$honey_conflict_log" || fail "honey conflict log missing sync state marker"

  "${tcfs_cmd[@]}" --config "$config_path" pull "$fixture_file" "$remote_after_conflict_file" --prefix "$prefix" --state "$state_json.remote-pullback" >"$remote_pullback_log" 2>&1
  cmp -s "$neo_content_file" "$remote_after_conflict_file" || fail "remote bytes were overwritten by honey conflict push"

  if [[ "$honey_independent_sibling" == "1" ]]; then
    remote_status=0
    # shellcheck disable=SC2029
    ssh "$honey_host" "$sibling_cmd" | tee "$honey_sibling_log" || remote_status=$?
    if [[ "$remote_status" -ne 0 ]]; then
      scp "$honey_host:$honey_remote_dir/honey-evidence/"* "$honey_evidence_dir/" >/dev/null 2>&1 || true
      cleanup_remote_env
      printf 'honey independent sibling push failed; see %s\n' "$honey_sibling_log" >&2
      exit "$remote_status"
    fi
    grep -q "independent_sibling_push=completed" "$honey_sibling_log" || fail "honey sibling log missing completion marker"
    grep -q "independent_sibling_conflict=absent" "$honey_sibling_log" || fail "honey sibling log missing no-conflict marker"
    "${tcfs_cmd[@]}" --config "$config_path" pull "$sibling_file" "$remote_sibling_after_progress_file" --prefix "$prefix" --state "$state_json.remote-sibling-progress" >"$remote_sibling_after_progress_log" 2>&1
    cmp -s "$honey_sibling_content_file" "$remote_sibling_after_progress_file" || fail "independent sibling remote bytes do not match honey content"
  fi

  if [[ "$honey_resolve_keep_both" == "1" ]]; then
    remote_status=0
    # shellcheck disable=SC2029
    ssh "$honey_host" "$resolve_cmd" | tee "$honey_daemon_resolve_log" || remote_status=$?
    scp "$honey_host:$honey_remote_dir/honey-evidence/"* "$honey_evidence_dir/" >/dev/null 2>&1 || true
    cleanup_remote_env
    if [[ "$remote_status" -ne 0 ]]; then
      printf 'honey daemon keep-both resolve failed; see %s\n' "$honey_daemon_resolve_log" >&2
      exit "$remote_status"
    fi

    if grep -q "daemon_resolve_keep_both=completed" "$honey_daemon_resolve_log"; then
      grep -q "conflict_copy_pushed=1" "$honey_daemon_resolve_log" || fail "honey daemon resolve log missing copy push marker"

      "${tcfs_cmd[@]}" --config "$config_path" pull "$fixture_file" "$remote_original_after_daemon_resolve_file" --prefix "$prefix" --state "$state_json.remote-daemon-resolve-original" >"$remote_original_after_daemon_resolve_log" 2>&1
      cmp -s "$neo_content_file" "$remote_original_after_daemon_resolve_file" || fail "original remote bytes changed during daemon keep-both resolve"
      "${tcfs_cmd[@]}" --config "$config_path" pull "$daemon_conflict_copy_file" "$remote_daemon_conflict_copy_file" --prefix "$prefix" --state "$state_json.remote-daemon-resolve-copy" >"$remote_daemon_conflict_copy_log" 2>&1
      cmp -s "$honey_content_file" "$remote_daemon_conflict_copy_file" || fail "daemon conflict copy remote bytes do not match honey content"

      cat >"$result_env" <<EOF
status=0
completed_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
proof=cross-host-conflict-daemon-keep-both-current-behavior
honey_push_conflict=detected
honey_sync_state=conflict
honey_local_content=preserved
remote_after_conflict=neo_mutated_preserved
daemon_resolve_keep_both=completed
daemon_auth_bypass_required=1
original_path_after_resolve=neo_mutated_preserved
conflict_copy_path=$daemon_conflict_copy_file
conflict_copy_remote=honey_mutated_preserved
EOF
      if [[ "$honey_independent_sibling" == "1" ]]; then
        cat >>"$result_env" <<EOF
independent_sibling_push=completed
independent_sibling_conflict=absent
independent_sibling_remote=honey_mutated_preserved
EOF
      fi
    elif grep -q "daemon_resolve_keep_both=blocked" "$honey_daemon_resolve_log"; then
      blocker_reason="$(awk -F= '/^blocker_reason=/{print $2; exit}' "$honey_daemon_resolve_log")"
      blocker_reason="${blocker_reason:-unknown}"
      cat >"$result_env" <<EOF
status=blocked
completed_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
proof=cross-host-conflict-daemon-keep-both-blocker
honey_push_conflict=detected
honey_sync_state=conflict
honey_local_content=preserved
remote_after_conflict=neo_mutated_preserved
daemon_resolve_keep_both=blocked
blocker_reason=$blocker_reason
daemon_auth_bypass_required=1
full_daemon_resolve_claim=not-claimed
EOF
      if [[ "$honey_independent_sibling" == "1" ]]; then
        cat >>"$result_env" <<EOF
independent_sibling_push=completed
independent_sibling_conflict=absent
independent_sibling_remote=honey_mutated_preserved
EOF
      fi
    else
      fail "honey daemon resolve log missing completed/blocked marker"
    fi
  elif [[ "$honey_recover_keep_both" == "1" ]]; then
    remote_status=0
    # shellcheck disable=SC2029
    ssh "$honey_host" "$recovery_cmd" | tee "$honey_recovery_log" || remote_status=$?
    scp "$honey_host:$honey_remote_dir/honey-evidence/"* "$honey_evidence_dir/" >/dev/null 2>&1 || true
    cleanup_remote_env
    if [[ "$remote_status" -ne 0 ]]; then
      printf 'honey keep-both recovery failed; see %s\n' "$honey_recovery_log" >&2
      exit "$remote_status"
    fi
    grep -q "keep_both_recovery=completed" "$honey_recovery_log" || fail "honey recovery log missing keep-both marker"
    grep -q "conflict_copy_pushed=1" "$honey_recovery_log" || fail "honey recovery log missing copy push marker"

    "${tcfs_cmd[@]}" --config "$config_path" pull "$fixture_file" "$remote_original_after_recovery_file" --prefix "$prefix" --state "$state_json.remote-recovery-original" >"$remote_original_after_recovery_log" 2>&1
    cmp -s "$neo_content_file" "$remote_original_after_recovery_file" || fail "original remote bytes changed during keep-both recovery"
    "${tcfs_cmd[@]}" --config "$config_path" pull "$conflict_copy_file" "$remote_conflict_copy_file" --prefix "$prefix" --state "$state_json.remote-recovery-copy" >"$remote_conflict_copy_log" 2>&1
    cmp -s "$honey_content_file" "$remote_conflict_copy_file" || fail "conflict copy remote bytes do not match honey content"

    cat >"$result_env" <<EOF
status=0
completed_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
proof=cross-host-conflict-keep-both-current-behavior
honey_push_conflict=detected
honey_sync_state=conflict
honey_local_content=preserved
remote_after_conflict=neo_mutated_preserved
keep_both_recovery=completed
original_path_after_recovery=neo_mutated_preserved
conflict_copy_path=$conflict_copy_file
conflict_copy_remote=honey_mutated_preserved
manual_recovery_only=1
EOF
    if [[ "$honey_independent_sibling" == "1" ]]; then
      cat >>"$result_env" <<EOF
independent_sibling_push=completed
independent_sibling_conflict=absent
independent_sibling_remote=honey_mutated_preserved
EOF
    fi
  else
    scp "$honey_host:$honey_remote_dir/honey-evidence/"* "$honey_evidence_dir/" >/dev/null 2>&1 || true
    cleanup_remote_env
    result_proof="cross-host-conflict-current-behavior"
    if [[ "$honey_independent_sibling" == "1" ]]; then
      result_proof="cross-host-conflict-independent-sibling-current-behavior"
    fi
    cat >"$result_env" <<EOF
status=0
completed_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
proof=$result_proof
honey_push_conflict=detected
honey_sync_state=conflict
honey_local_content=preserved
remote_after_conflict=neo_mutated_preserved
EOF
    if [[ "$honey_independent_sibling" == "1" ]]; then
      cat >>"$result_env" <<EOF
independent_sibling_push=completed
independent_sibling_conflict=absent
independent_sibling_remote=honey_mutated_preserved
EOF
    fi
  fi
else
  if [[ "$push_remote" == "1" ]]; then
    result_status="pushed"
    result_proof="pending-honey-conflict"
  else
    result_status="plan-only"
    result_proof="pending-push-or-honey-conflict"
  fi
  cat >"$result_env" <<EOF
status=$result_status
completed_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
proof=$result_proof
EOF
fi

cat >"$evidence_dir/README.md" <<EOF
# TCFS neo/honey Conflict Evidence

Created: $(date -u +%Y-%m-%dT%H:%M:%SZ)

This packet targets the same-fixture cross-host conflict row:

1. neo pushes \`$fixture_file\` to a disposable prefix.
2. honey pulls that file into a physical sync root and edits it locally.
3. neo edits and pushes a different version of the same relative path.
4. honey attempts to push its divergent local version.
5. TCFS must detect conflict, skip the honey upload, mark honey local state as
   \`conflict\`, preserve honey's local bytes, and leave the remote index at
   neo's last pushed bytes.

When \`--honey-recover-keep-both\` is enabled, the packet then runs a manual
keep-both recovery pattern:

6. honey copies its conflicted local bytes to \`$conflict_copy_file\`.
7. honey pulls the original path back to neo's remote bytes.
8. honey pushes \`$conflict_copy_file\`.
9. neo pulls both paths back and compares exact content.

When \`--honey-resolve-keep-both\` is enabled, the packet instead attempts a
daemon-backed keep-both resolution:

6. honey starts an isolated \`tcfsd\` with the same disposable config/state as
   the CLI conflict lane and \`auth.require_session=false\`.
7. honey runs \`tcfs resolve --strategy keep-both\` for the conflicted path.
8. honey verifies the original path now has neo's remote bytes and the daemon
   conflict copy \`$daemon_conflict_copy_file\` preserves honey's bytes.
9. neo pulls both paths back and compares exact content. If daemon startup or
   resolve wiring blocks the proof, \`result.env\` records \`status=blocked\`.

When \`--honey-independent-sibling\` is enabled, the packet also proves sibling
progress:

6. neo seeds \`$sibling_file\`.
7. honey edits that sibling before the conflict.
8. after the original file is conflicted, honey pushes the sibling.
9. neo pulls the sibling and compares exact honey content while the original path
   remains conflicted on honey.

Remote:

\`\`\`text
$remote
\`\`\`

Important files:

- \`neo-initial-push.log\`: initial neo publish transcript, when pushed
- \`honey-prepare.log\`: honey pull/local edit transcript, when run
- \`neo-conflict-push.log\`: neo divergent push transcript, when run
- \`honey-conflict-push.log\`: honey conflict push transcript, when run
- \`honey-independent-sibling-push.log\`: optional independent sibling push transcript
- \`honey-keep-both-recovery.log\`: optional manual keep-both recovery transcript
- \`honey-daemon-resolve-keep-both.log\`: optional daemon-backed resolve transcript
- \`honey-evidence/\`: detailed remote transcripts, copied back when available
- \`remote-after-conflict.content\`: remote pullback after honey conflict push
- \`remote-sibling-after-progress.content\`: optional independent sibling pullback
- \`remote-original-after-recovery.content\`: optional original-path pullback
- \`remote-conflict-copy.content\`: optional keep-both copy pullback
- \`remote-original-after-daemon-resolve.content\`: optional daemon-resolved original pullback
- \`remote-daemon-conflict-copy.content\`: optional daemon-created conflict copy pullback
- \`result.env\`: pass/plan-only status

Claimability note: this proves current CLI conflict behavior for one
same-fixture cross-host row. Optional keep-both mode proves a manual recovery
pattern, optional daemon keep-both mode proves or blocks the current
\`tcfs resolve\` path under an isolated auth-bypass daemon, and optional sibling
mode proves per-path sibling progress while another path is conflicted. These do
not prove authenticated production daemon resolve, Finder conflict UX, automatic
resolution, keep-synced/pin policy, or production FileProvider status badges.
EOF

printf 'neo/honey conflict evidence: %s\n' "$evidence_dir"
printf 'honey conflict commands: %s\n' "$honey_commands"
