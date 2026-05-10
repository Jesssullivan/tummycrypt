#!/usr/bin/env bash
#
# Prepare or run the same-fixture neo/honey unsynced rehydrate proof packet.
#
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/neo-honey-unsynced-rehydrate-demo.sh [options]

Create an isolated neo fixture, optionally push it to a disposable remote
prefix, convert neo's local copy to a .tc stub, let honey mutate the same file
through a mounted view, then pull the latest bytes back on neo and verify the
adjacent stub is removed.

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
      Push the initial neo fixture, then unsync it locally.
  --create-bucket
      Best-effort remote bucket creation before pushing.
  --tcfs-bin <path>
      Local tcfs binary for push/unsync/pull.
  --honey-host <host>
      SSH host label for honey. Default: honey.
  --honey-mount-root <path>
      Honey mountpoint. Default: /tmp/tcfs-<run-id>-honey/mount.
  --honey-remote-dir <path>
      Honey temp directory. Default: /tmp/tcfs-<run-id>-honey/run.
  --honey-tcfs-bin <path>
      Remote tcfs binary path on honey. Default: tcfs.
  --run-honey
      Copy helper scripts to honey, mutate the mounted file, then rehydrate
      neo's unsynced copy and verify exact honey content.
  --honey-start-mount
      With --run-honey, start tcfs mount on honey.
  --honey-existing-mount
      With --run-honey, assume --honey-mount-root is already mounted.
  --forward-aws-env
      With --run-honey, forward current AWS env to honey for this smoke run.
  --allow-real-roots
      Allow direct HOME, ~/Documents, or ~/git neo roots. Not recommended.
  -h, --help
      Show this help.

Environment mirrors:
  TCFS_UNSYNCED_REHYDRATE_REMOTE
  TCFS_UNSYNCED_REHYDRATE_NEO_ROOT
  TCFS_UNSYNCED_REHYDRATE_EVIDENCE_DIR
  TCFS_UNSYNCED_REHYDRATE_STATE_DIR
  TCFS_UNSYNCED_REHYDRATE_PUSH=1
  TCFS_UNSYNCED_REHYDRATE_CREATE_BUCKET=1
  TCFS_UNSYNCED_REHYDRATE_RUN_HONEY=1
  TCFS_BIN
  TCFS_HONEY_HOST
  TCFS_HONEY_MOUNT_ROOT
  TCFS_HONEY_REMOTE_DIR
  TCFS_HONEY_TCFS_BIN
  TCFS_HONEY_START_MOUNT=1
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

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
timestamp="$(date -u +%Y%m%dT%H%M%SZ)"
run_id="unsynced-rehydrate-${timestamp}-$$"

remote="${TCFS_UNSYNCED_REHYDRATE_REMOTE:-seaweedfs://localhost:8333/tcfs/${run_id}}"
neo_root="${TCFS_UNSYNCED_REHYDRATE_NEO_ROOT:-$HOME/TCFS Pilot/runs/${run_id}/neo}"
evidence_dir="${TCFS_UNSYNCED_REHYDRATE_EVIDENCE_DIR:-${TMPDIR:-/tmp}/tcfs-${run_id}}"
state_dir="${TCFS_UNSYNCED_REHYDRATE_STATE_DIR:-}"
push_remote="$(bool_env TCFS_UNSYNCED_REHYDRATE_PUSH "${TCFS_UNSYNCED_REHYDRATE_PUSH:-0}")"
create_bucket="$(bool_env TCFS_UNSYNCED_REHYDRATE_CREATE_BUCKET "${TCFS_UNSYNCED_REHYDRATE_CREATE_BUCKET:-0}")"
run_honey="$(bool_env TCFS_UNSYNCED_REHYDRATE_RUN_HONEY "${TCFS_UNSYNCED_REHYDRATE_RUN_HONEY:-0}")"
tcfs_bin="${TCFS_BIN:-}"
honey_host="${TCFS_HONEY_HOST:-honey}"
honey_mount_root="${TCFS_HONEY_MOUNT_ROOT:-/tmp/tcfs-${run_id}-honey/mount}"
honey_remote_dir="${TCFS_HONEY_REMOTE_DIR:-/tmp/tcfs-${run_id}-honey/run}"
honey_tcfs_bin="${TCFS_HONEY_TCFS_BIN:-tcfs}"
honey_start_mount="$(bool_env TCFS_HONEY_START_MOUNT "${TCFS_HONEY_START_MOUNT:-0}")"
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
    --run-honey)
      run_honey=1
      shift
      ;;
    --honey-start-mount)
      honey_start_mount=1
      shift
      ;;
    --honey-existing-mount)
      honey_start_mount=0
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

case "$honey_remote_dir" in
  *[[:space:]]*) fail "--honey-remote-dir must not contain whitespace: $honey_remote_dir" ;;
esac
if ! [[ "$honey_remote_dir" =~ ^[A-Za-z0-9_./@%+=:-]+$ ]]; then
  fail "--honey-remote-dir contains unsafe shell characters: $honey_remote_dir"
fi

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
config_path="$state_dir/tcfs-unsynced-rehydrate.toml"
mc_config_dir="$state_dir/mc"
fixture_file="Projects/shared/notes.md"
fixture_path="$neo_canon/$fixture_file"
stub_path="${fixture_path}.tc"
initial_content_file="$evidence_dir/neo-initial-content.txt"
honey_content_file="$evidence_dir/honey-mutated-content.txt"
local_tree="$evidence_dir/neo-tree.txt"
push_log="$evidence_dir/push.log"
unsync_log="$evidence_dir/unsync.out"
status_after_unsync="$evidence_dir/sync-status-after-unsync.out"
rehydrate_log="$evidence_dir/rehydrate-pull.log"
status_after_rehydrate="$evidence_dir/sync-status-after-rehydrate.out"
honey_script="$evidence_dir/honey-mutator-run.sh"
honey_commands="$evidence_dir/honey-mutator-commands.txt"
honey_run_log="$evidence_dir/honey-mutator.log"
honey_mount_log="$evidence_dir/honey-mount.log"
result_env="$evidence_dir/result.env"
stub_status_env="$evidence_dir/stub-status.env"

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
    mc --config-dir "$mc_config_dir" alias set tcfs-unsynced "$endpoint" "$AWS_ACCESS_KEY_ID" "$AWS_SECRET_ACCESS_KEY" >/dev/null
    mc --config-dir "$mc_config_dir" mb --ignore-existing "tcfs-unsynced/$bucket" >/dev/null
    return 0
  fi

  fail "--create-bucket requested, but none of aws, s5cmd, or mc was found"
}

mkdir -p "$neo_canon/Projects/shared" "$cache_root"

cat >"$fixture_path" <<'EOF'
# Shared TCFS note

version: neo-initial
body: this file starts on neo and will be removed locally before honey edits it.
EOF

cat >"$honey_content_file" <<'EOF'
# Shared TCFS note

version: honey-mutated
body: honey edited this while neo had only the .tc stub.
EOF

cp "$fixture_path" "$initial_content_file"
find "$neo_canon" -maxdepth 8 -print | sort >"$local_tree"

cat >"$config_path" <<EOF
[daemon]
socket = "$state_dir/no-daemon.sock"

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

[fuse]
cache_dir = "$cache_root"
cache_max_mb = 64
negative_cache_ttl_secs = 1

[crypto]
enabled = false
EOF

cat >"$honey_script" <<EOF
#!/usr/bin/env bash
set -euo pipefail

REMOTE=$(shell_quote "$remote")
TCFS_BIN=$(shell_quote "$honey_tcfs_bin")
MOUNT_ROOT_RAW=$(single_quote "$honey_mount_root")
case "\$MOUNT_ROOT_RAW" in
  "~/"*) MOUNT_ROOT="\${HOME}/\${MOUNT_ROOT_RAW#\\~/}" ;;
  *) MOUNT_ROOT="\$MOUNT_ROOT_RAW" ;;
esac
FIXTURE_FILE=$(shell_quote "$fixture_file")
EXPECTED_INITIAL="\${TCFS_HONEY_INITIAL_CONTENT_FILE:-$(shell_quote "$honey_remote_dir/neo-initial-content.txt")}"
EXPECTED_MUTATED="\${TCFS_HONEY_MUTATED_CONTENT_FILE:-$(shell_quote "$honey_remote_dir/honey-mutated-content.txt")}"
SMOKE_SCRIPT="\${TCFS_HONEY_SMOKE_SCRIPT:-$(shell_quote "$honey_remote_dir/lazy-hydration-mounted-smoke.sh")}"
MOUNT_LOG="\${TCFS_HONEY_MOUNT_LOG:-$(shell_quote "$honey_remote_dir/mount.log")}"

if [[ -n "\${TCFS_HONEY_ENV_FILE:-}" ]]; then
  # shellcheck disable=SC1090
  source "\$TCFS_HONEY_ENV_FILE"
fi

mkdir -p "\$MOUNT_ROOT"
mount_started=0
cleanup_mount() {
  if [[ "\$mount_started" == "1" && "\${TCFS_HONEY_KEEP_MOUNT:-0}" != "1" ]]; then
    "\$TCFS_BIN" unmount "\$MOUNT_ROOT" >/dev/null 2>&1 || fusermount3 -u "\$MOUNT_ROOT" >/dev/null 2>&1 || true
  fi
}
trap cleanup_mount EXIT

if [[ "\${TCFS_HONEY_START_MOUNT:-0}" == "1" ]]; then
  nohup "\$TCFS_BIN" mount "\$REMOTE" "\$MOUNT_ROOT" >"\$MOUNT_LOG" 2>&1 &
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
fi

bash "\$SMOKE_SCRIPT" \\
  --mount-root "\$MOUNT_ROOT" \\
  --expected-file "\$FIXTURE_FILE" \\
  --expected-content-file "\$EXPECTED_INITIAL" \\
  --expect-entry Projects \\
  --expect-entry Projects/shared \\
  --max-depth 8

fixture_path="\$MOUNT_ROOT/\$FIXTURE_FILE"
if [[ ! -f "\$fixture_path" ]]; then
  echo "expected mounted fixture missing before honey mutation: \$fixture_path" >&2
  exit 1
fi
cp "\$EXPECTED_MUTATED" "\$fixture_path"
cmp -s "\$EXPECTED_MUTATED" "\$fixture_path"
echo "honey mounted mutation wrote exact content: \$FIXTURE_FILE"
EOF
chmod +x "$honey_script"

cat >"$honey_commands" <<EOF
# Prepare remote work directory:
ssh $(shell_quote "$honey_host") 'mkdir -p $(shell_quote "$honey_remote_dir")'

# Copy the mounted smoke helper, expected content, and honey mutator:
scp $(shell_quote "$REPO_ROOT/scripts/lazy-hydration-mounted-smoke.sh") $(shell_quote "$honey_host"):$(shell_quote "$honey_remote_dir/lazy-hydration-mounted-smoke.sh")
scp $(shell_quote "$initial_content_file") $(shell_quote "$honey_host"):$(shell_quote "$honey_remote_dir/neo-initial-content.txt")
scp $(shell_quote "$honey_content_file") $(shell_quote "$honey_host"):$(shell_quote "$honey_remote_dir/honey-mutated-content.txt")
scp $(shell_quote "$honey_script") $(shell_quote "$honey_host"):$(shell_quote "$honey_remote_dir/honey-mutator-run.sh")

# Start a mount on honey and mutate the same fixture:
ssh $(shell_quote "$honey_host") 'TCFS_HONEY_START_MOUNT=1 TCFS_HONEY_SMOKE_SCRIPT=$(shell_quote "$honey_remote_dir/lazy-hydration-mounted-smoke.sh") TCFS_HONEY_INITIAL_CONTENT_FILE=$(shell_quote "$honey_remote_dir/neo-initial-content.txt") TCFS_HONEY_MUTATED_CONTENT_FILE=$(shell_quote "$honey_remote_dir/honey-mutated-content.txt") TCFS_HONEY_MOUNT_LOG=$(shell_quote "$honey_remote_dir/mount.log") bash $(shell_quote "$honey_remote_dir/honey-mutator-run.sh")'

# If the mount is already active on honey, omit TCFS_HONEY_START_MOUNT:
ssh $(shell_quote "$honey_host") 'TCFS_HONEY_SMOKE_SCRIPT=$(shell_quote "$honey_remote_dir/lazy-hydration-mounted-smoke.sh") TCFS_HONEY_INITIAL_CONTENT_FILE=$(shell_quote "$honey_remote_dir/neo-initial-content.txt") TCFS_HONEY_MUTATED_CONTENT_FILE=$(shell_quote "$honey_remote_dir/honey-mutated-content.txt") TCFS_HONEY_MOUNT_LOG=$(shell_quote "$honey_remote_dir/mount.log") bash $(shell_quote "$honey_remote_dir/honey-mutator-run.sh")'
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
honey_mount_root=$honey_mount_root
honey_remote_dir=$honey_remote_dir
honey_tcfs_bin=$honey_tcfs_bin
push=$push_remote
run_honey=$run_honey
honey_start_mount=$honey_start_mount
allow_real_roots=$allow_real_roots
EOF
cp "$config_path" "$evidence_dir/tcfs-unsynced-rehydrate.toml"

if [[ "$push_remote" == "1" ]]; then
  create_bucket_if_requested
  printf 'pushing neo fixture to %s\n' "$remote"
  "${tcfs_cmd[@]}" --config "$config_path" push "$neo_canon" --prefix "$prefix" --state "$state_json" >"$push_log" 2>&1

  printf 'unsyncing neo fixture: %s\n' "$fixture_path"
  "${tcfs_cmd[@]}" --config "$config_path" unsync "$fixture_path" >"$unsync_log" 2>&1
  [[ ! -f "$fixture_path" ]] || fail "neo hydrated file still exists after unsync: $fixture_path"
  [[ -f "$stub_path" ]] || fail "neo stub missing after unsync: $stub_path"
  "${tcfs_cmd[@]}" --config "$config_path" sync-status "$fixture_path" >"$status_after_unsync" 2>&1
  grep -q "sync state: not_synced" "$status_after_unsync" || fail "neo sync-status after unsync did not report not_synced"
else
  printf 'plan-only: fixture created but not pushed. Re-run with --push when ready.\n'
fi

if [[ "$run_honey" == "1" ]]; then
  [[ "$push_remote" == "1" ]] || fail "--run-honey requires --push"
  command -v ssh >/dev/null 2>&1 || fail "ssh not found"
  command -v scp >/dev/null 2>&1 || fail "scp not found"

  printf 'running honey same-fixture mutation on %s\n' "$honey_host"
  if [[ "$forward_aws_env" == "1" && "$honey_start_mount" == "1" ]]; then
    printf 'warning: forwarded AWS credentials are inherited by the honey mount process; unmount after the smoke unless honey has its own credential source\n' >&2
  fi

  # shellcheck disable=SC2029
  ssh "$honey_host" "mkdir -p $(shell_quote "$honey_remote_dir")"
  scp "$REPO_ROOT/scripts/lazy-hydration-mounted-smoke.sh" "$honey_host:$honey_remote_dir/lazy-hydration-mounted-smoke.sh"
  scp "$initial_content_file" "$honey_host:$honey_remote_dir/neo-initial-content.txt"
  scp "$honey_content_file" "$honey_host:$honey_remote_dir/honey-mutated-content.txt"
  scp "$honey_script" "$honey_host:$honey_remote_dir/honey-mutator-run.sh"

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

  remote_run_cmd="$(printf 'TCFS_HONEY_START_MOUNT=%q TCFS_HONEY_SMOKE_SCRIPT=%q TCFS_HONEY_INITIAL_CONTENT_FILE=%q TCFS_HONEY_MUTATED_CONTENT_FILE=%q TCFS_HONEY_MOUNT_LOG=%q bash %q' \
    "$honey_start_mount" \
    "$honey_remote_dir/lazy-hydration-mounted-smoke.sh" \
    "$honey_remote_dir/neo-initial-content.txt" \
    "$honey_remote_dir/honey-mutated-content.txt" \
    "$honey_remote_dir/mount.log" \
    "$honey_remote_dir/honey-mutator-run.sh")"
  if [[ -n "$remote_env_file" ]]; then
    remote_run_cmd="$(printf 'TCFS_HONEY_ENV_FILE=%q %s' "$remote_env_file" "$remote_run_cmd")"
  fi

  remote_status=0
  # shellcheck disable=SC2029
  ssh "$honey_host" "$remote_run_cmd" | tee "$honey_run_log" || remote_status=$?
  # shellcheck disable=SC2029
  ssh "$honey_host" "test -f $(shell_quote "$honey_remote_dir/mount.log") && cat $(shell_quote "$honey_remote_dir/mount.log")" >"$honey_mount_log" 2>/dev/null || true
  cleanup_remote_env
  if [[ "$remote_status" -ne 0 ]]; then
    printf 'honey mutation failed; see %s\n' "$honey_run_log" >&2
    exit "$remote_status"
  fi

  printf 'rehydrating neo fixture from latest remote index\n'
  "${tcfs_cmd[@]}" --config "$config_path" pull "$fixture_file" "$fixture_path" --prefix "$prefix" --state "$state_json" >"$rehydrate_log" 2>&1
  cmp -s "$honey_content_file" "$fixture_path" || fail "neo rehydrated content does not match honey mutation"
  "${tcfs_cmd[@]}" --config "$config_path" sync-status "$fixture_path" >"$status_after_rehydrate" 2>&1
  grep -q "sync state: synced" "$status_after_rehydrate" || fail "neo sync-status after rehydrate did not report synced"

  if [[ -e "$stub_path" ]]; then
    cat >"$stub_status_env" <<EOF
stub_after_pull=present
stub_path=$stub_path
EOF
    fail "stale stub still present after rehydrate pull: $stub_path"
  fi

  cat >"$stub_status_env" <<EOF
stub_after_pull=absent
stub_path=$stub_path
EOF
  cat >"$result_env" <<EOF
status=0
completed_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
proof=same-fixture-unsynced-rehydrate
EOF
else
  if [[ "$push_remote" == "1" ]]; then
    result_status="pushed-unsynced"
    result_proof="pending-honey"
  else
    result_status="plan-only"
    result_proof="pending-push-or-honey"
  fi
  cat >"$result_env" <<EOF
status=$result_status
completed_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
proof=$result_proof
EOF
fi

cat >"$evidence_dir/README.md" <<EOF
# TCFS neo/honey Unsynced Rehydrate Evidence

Created: $(date -u +%Y-%m-%dT%H:%M:%SZ)

This packet targets the same-fixture permutation:

1. neo creates and pushes \`$fixture_file\` to a disposable prefix.
2. neo runs \`tcfs unsync\` so the local file becomes \`$fixture_file.tc\`.
3. honey opens the same remote file through a mounted view and writes new bytes.
4. neo runs \`tcfs pull $fixture_file\` and must receive honey's exact content.
5. the adjacent \`.tc\` stub must be gone after rehydrate.

Remote:

\`\`\`text
$remote
\`\`\`

Important files:

- \`neo-tree.txt\`: isolated neo fixture tree
- \`tcfs-unsynced-rehydrate.toml\`: disposable config copied from the state dir
- \`honey-mutator-commands.txt\`: manual honey commands
- \`honey-mutator.log\`: honey mounted traversal and mutation transcript, when run
- \`unsync.out\`: neo \`tcfs unsync\` transcript, when pushed
- \`sync-status-after-unsync.out\`: neo status after local remove
- \`rehydrate-pull.log\`: neo pull transcript, when honey was run
- \`sync-status-after-rehydrate.out\`: neo status after rehydrate
- \`stub-status.env\`: whether the stale \`.tc\` stub remained
- \`result.env\`: pass/plan-only status

This helper uses an isolated root under \`$neo_canon\`; it does not target real
\`~/Documents\`, \`~/git\`, dotfiles, or broad home-directory paths unless
\`--allow-real-roots\` is explicitly supplied.
EOF

printf 'neo root: %s\n' "$neo_canon"
printf 'remote prefix: %s\n' "$remote"
printf 'evidence dir: %s\n' "$evidence_dir"
printf 'honey mutator commands: %s\n' "$honey_commands"
printf 'result: %s\n' "$result_env"
