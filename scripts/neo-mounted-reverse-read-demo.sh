#!/usr/bin/env bash
#
# Prepare or run the M4 mounted reverse-read proof packet.
#
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/neo-mounted-reverse-read-demo.sh [options]

Create an isolated neo physical sync root, have honey publish a fixture to a
disposable remote prefix, pull and unsync that fixture on neo, have honey
publish newer bytes, then prove neo can read the latest bytes through a mounted
clean-name view while the physical neo root remains stub-only.

Options:
  --remote <seaweedfs://host:port/bucket/prefix>
      Remote prefix to use. Defaults to a timestamped prefix.
  --neo-root <path>
      Local isolated neo physical root. Defaults to "$HOME/TCFS Pilot/runs/<run-id>/neo".
  --neo-mount-root <path>
      Local neo mountpoint. Defaults to /tmp/tcfs-<run-id>-neo/mount.
  --evidence-dir <path>
      Evidence output directory. Defaults to a temp directory.
  --state-dir <path>
      Local helper state directory. Defaults to <evidence-dir>/neo-state.
  --push
      Run live remote stages.
  --create-bucket
      Best-effort remote bucket creation before pushing.
  --tcfs-bin <path>
      Local tcfs binary for neo pull/unsync/mount.
  --honey-host <host>
      SSH host label for honey. Default: honey.
  --honey-root <path>
      Honey physical sync root. Default: /tmp/tcfs-<run-id>-honey/root.
  --honey-remote-dir <path>
      Honey temp directory. Default: /tmp/tcfs-<run-id>-honey/run.
  --honey-tcfs-bin <path>
      Remote tcfs binary path on honey. Default: tcfs.
  --run-honey
      Copy the honey runner to honey and run initial/mutated push stages.
  --neo-start-mount
      Start tcfs mount on neo before the mounted read.
  --neo-existing-mount
      Assume --neo-mount-root is already mounted or populated for test harnesses.
  --neo-nfs
      Start neo's mount with the TCFS NFS loopback backend instead of FUSE.
  --forward-aws-env
      With --run-honey, forward current AWS env to honey for this smoke run.
  --allow-real-roots
      Allow direct HOME, ~/Documents, or ~/git roots. Not recommended.
  -h, --help
      Show this help.

Environment mirrors:
  TCFS_MOUNTED_REVERSE_READ_REMOTE
  TCFS_MOUNTED_REVERSE_READ_NEO_ROOT
  TCFS_MOUNTED_REVERSE_READ_NEO_MOUNT_ROOT
  TCFS_MOUNTED_REVERSE_READ_EVIDENCE_DIR
  TCFS_MOUNTED_REVERSE_READ_STATE_DIR
  TCFS_MOUNTED_REVERSE_READ_PUSH=1
  TCFS_MOUNTED_REVERSE_READ_CREATE_BUCKET=1
  TCFS_MOUNTED_REVERSE_READ_RUN_HONEY=1
  TCFS_MOUNTED_REVERSE_READ_NEO_START_MOUNT=1
  TCFS_MOUNTED_REVERSE_READ_NEO_NFS=1
  TCFS_BIN
  TCFS_HONEY_HOST
  TCFS_HONEY_ROOT
  TCFS_HONEY_REMOTE_DIR
  TCFS_HONEY_TCFS_BIN
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

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
timestamp="$(date -u +%Y%m%dT%H%M%SZ)"
run_id="mounted-reverse-read-${timestamp}-$$"

remote="${TCFS_MOUNTED_REVERSE_READ_REMOTE:-seaweedfs://localhost:8333/tcfs/${run_id}}"
neo_root="${TCFS_MOUNTED_REVERSE_READ_NEO_ROOT:-$HOME/TCFS Pilot/runs/${run_id}/neo}"
neo_mount_root="${TCFS_MOUNTED_REVERSE_READ_NEO_MOUNT_ROOT:-/tmp/tcfs-${run_id}-neo/mount}"
evidence_dir="${TCFS_MOUNTED_REVERSE_READ_EVIDENCE_DIR:-${TMPDIR:-/tmp}/tcfs-${run_id}}"
state_dir="${TCFS_MOUNTED_REVERSE_READ_STATE_DIR:-}"
push_remote="$(bool_env TCFS_MOUNTED_REVERSE_READ_PUSH "${TCFS_MOUNTED_REVERSE_READ_PUSH:-0}")"
create_bucket="$(bool_env TCFS_MOUNTED_REVERSE_READ_CREATE_BUCKET "${TCFS_MOUNTED_REVERSE_READ_CREATE_BUCKET:-0}")"
run_honey="$(bool_env TCFS_MOUNTED_REVERSE_READ_RUN_HONEY "${TCFS_MOUNTED_REVERSE_READ_RUN_HONEY:-0}")"
neo_start_mount="$(bool_env TCFS_MOUNTED_REVERSE_READ_NEO_START_MOUNT "${TCFS_MOUNTED_REVERSE_READ_NEO_START_MOUNT:-0}")"
neo_nfs="$(bool_env TCFS_MOUNTED_REVERSE_READ_NEO_NFS "${TCFS_MOUNTED_REVERSE_READ_NEO_NFS:-0}")"
tcfs_bin="${TCFS_BIN:-}"
honey_host="${TCFS_HONEY_HOST:-honey}"
honey_root="${TCFS_HONEY_ROOT:-/tmp/tcfs-${run_id}-honey/root}"
honey_remote_dir="${TCFS_HONEY_REMOTE_DIR:-/tmp/tcfs-${run_id}-honey/run}"
honey_tcfs_bin="${TCFS_HONEY_TCFS_BIN:-tcfs}"
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
    --neo-mount-root)
      [[ $# -ge 2 ]] || fail "--neo-mount-root requires a value"
      neo_mount_root="$2"
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
    --run-honey)
      run_honey=1
      shift
      ;;
    --neo-start-mount)
      neo_start_mount=1
      shift
      ;;
    --neo-existing-mount)
      neo_start_mount=0
      shift
      ;;
    --neo-nfs)
      neo_nfs=1
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

assert_safe_remote_path "--honey-root" "$honey_root"
assert_safe_remote_path "--honey-remote-dir" "$honey_remote_dir"

neo_canon="$(make_physical_dir "$neo_root")"
neo_mount_canon="$(make_physical_dir "$neo_mount_root")"
home_canon="$(canonical_path "$HOME")"
documents_canon="$(canonical_path "$HOME/Documents")"
git_canon="$(canonical_path "$HOME/git")"

if [[ "$allow_real_roots" != "1" ]]; then
  [[ "$neo_canon" != "/" ]] || fail "refusing to use filesystem root as neo root"
  [[ "$neo_canon" != "$home_canon" ]] || fail "refusing to use HOME as neo root"
  [[ "$neo_canon" != "$documents_canon" ]] || fail "refusing to use real Documents as neo root"
  [[ "$neo_canon" != "$git_canon" ]] || fail "refusing to use real git as neo root"
  [[ "$neo_mount_canon" != "/" ]] || fail "refusing to use filesystem root as neo mount root"
  [[ "$neo_mount_canon" != "$home_canon" ]] || fail "refusing to use HOME as neo mount root"
  [[ "$neo_mount_canon" != "$documents_canon" ]] || fail "refusing to use real Documents as neo mount root"
  [[ "$neo_mount_canon" != "$git_canon" ]] || fail "refusing to use real git as neo mount root"
fi

mkdir -p "$evidence_dir"
if [[ -z "$state_dir" ]]; then
  state_dir="$evidence_dir/neo-state"
fi
mkdir -p "$state_dir"

state_json="$state_dir/state.json"
cache_root="$state_dir/cache"
config_path="$state_dir/tcfs-mounted-reverse-read.toml"
mc_config_dir="$state_dir/mc"
fixture_file="Projects/shared/mounted-reverse-notes.md"
fixture_path="$neo_canon/$fixture_file"
stub_path="${fixture_path}.tc"
initial_content_file="$evidence_dir/honey-initial-content.txt"
mutated_content_file="$evidence_dir/honey-mutated-content.txt"
local_tree="$evidence_dir/neo-tree.txt"
pull_initial_log="$evidence_dir/neo-initial-pull.log"
unsync_log="$evidence_dir/neo-unsync.out"
status_after_unsync="$evidence_dir/neo-sync-status-after-unsync.out"
honey_script="$evidence_dir/honey-mounted-reverse-run.sh"
honey_commands="$evidence_dir/honey-mounted-reverse-commands.txt"
honey_initial_log="$evidence_dir/honey-initial-push.log"
honey_mutated_log="$evidence_dir/honey-mutated-push.log"
mounted_read_log="$evidence_dir/neo-mounted-read.log"
neo_mount_log="$evidence_dir/neo-mount.log"
physical_stub_env="$evidence_dir/neo-physical-stub-after-mounted-read.env"
result_env="$evidence_dir/result.env"
current_stage="initializing"

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
    mc --config-dir "$mc_config_dir" alias set tcfs-m4 "$endpoint" "$AWS_ACCESS_KEY_ID" "$AWS_SECRET_ACCESS_KEY" >/dev/null
    mc --config-dir "$mc_config_dir" mb --ignore-existing "tcfs-m4/$bucket" >/dev/null
    return 0
  fi

  fail "--create-bucket requested, but none of aws, s5cmd, or mc was found"
}

mkdir -p "$neo_canon/Projects/shared" "$cache_root"

cat >"$initial_content_file" <<'EOF'
# Mounted reverse TCFS note

version: honey-initial
body: honey published this before neo unsynced its physical copy.
EOF

cat >"$mutated_content_file" <<'EOF'
# Mounted reverse TCFS note

version: honey-mutated
body: honey updated this while neo had only the physical .tc stub.
EOF

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

ENDPOINT=$(shell_quote "$endpoint")
REGION=$(shell_quote "$region")
BUCKET=$(shell_quote "$bucket")
PREFIX=$(shell_quote "$prefix")
TCFS_BIN=$(shell_quote "$honey_tcfs_bin")
HONEY_ROOT_RAW=$(single_quote "$honey_root")
RUN_DIR=$(shell_quote "$honey_remote_dir")
FIXTURE_FILE=$(shell_quote "$fixture_file")
INITIAL_CONTENT="\${TCFS_HONEY_INITIAL_CONTENT_FILE:-$(shell_quote "$honey_remote_dir/honey-initial-content.txt")}"
MUTATED_CONTENT="\${TCFS_HONEY_MUTATED_CONTENT_FILE:-$(shell_quote "$honey_remote_dir/honey-mutated-content.txt")}"

case "\$HONEY_ROOT_RAW" in
  "~/"*) HONEY_ROOT="\${HOME}/\${HONEY_ROOT_RAW#\\~/}" ;;
  *) HONEY_ROOT="\$HONEY_ROOT_RAW" ;;
esac

if [[ -n "\${TCFS_HONEY_ENV_FILE:-}" ]]; then
  # shellcheck disable=SC1090
  source "\$TCFS_HONEY_ENV_FILE"
fi

mode="\${1:-}"
[[ -n "\$mode" ]] || { echo "mode required: push-initial or push-mutated" >&2; exit 2; }

STATE_DIR="\$RUN_DIR/honey-state"
CACHE_ROOT="\$STATE_DIR/cache"
CONFIG_PATH="\$STATE_DIR/tcfs-mounted-reverse-read.toml"
STATE_JSON="\$STATE_DIR/state.json"
FIXTURE_PATH="\$HONEY_ROOT/\$FIXTURE_FILE"

mkdir -p "\$(dirname "\$FIXTURE_PATH")" "\$CACHE_ROOT"

cat >"\$CONFIG_PATH" <<REMOTE_CONFIG
[daemon]
socket = "\$STATE_DIR/no-daemon.sock"

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

[fuse]
cache_dir = "\$CACHE_ROOT"
cache_max_mb = 64
negative_cache_ttl_secs = 1

[crypto]
enabled = false
REMOTE_CONFIG

case "\$mode" in
  push-initial)
    cp "\$INITIAL_CONTENT" "\$FIXTURE_PATH"
    "\$TCFS_BIN" --config "\$CONFIG_PATH" push "\$HONEY_ROOT" --prefix "\$PREFIX" --state "\$STATE_JSON"
    echo "honey mounted reverse initial push ok: \$FIXTURE_FILE"
    ;;
  push-mutated)
    cp "\$MUTATED_CONTENT" "\$FIXTURE_PATH"
    "\$TCFS_BIN" --config "\$CONFIG_PATH" push "\$HONEY_ROOT" --prefix "\$PREFIX" --state "\$STATE_JSON"
    echo "honey mounted reverse mutated push ok: \$FIXTURE_FILE"
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

# Copy expected content and the honey M4 runner:
scp $(shell_quote "$initial_content_file") $(shell_quote "$honey_host"):$(shell_quote "$honey_remote_dir/honey-initial-content.txt")
scp $(shell_quote "$mutated_content_file") $(shell_quote "$honey_host"):$(shell_quote "$honey_remote_dir/honey-mutated-content.txt")
scp $(shell_quote "$honey_script") $(shell_quote "$honey_host"):$(shell_quote "$honey_remote_dir/honey-mounted-reverse-run.sh")

# Honey publishes initial bytes:
ssh $(shell_quote "$honey_host") 'TCFS_HONEY_INITIAL_CONTENT_FILE=$(shell_quote "$honey_remote_dir/honey-initial-content.txt") bash $(shell_quote "$honey_remote_dir/honey-mounted-reverse-run.sh") push-initial'

# After neo pulls and unsyncs, honey publishes newer bytes:
ssh $(shell_quote "$honey_host") 'TCFS_HONEY_MUTATED_CONTENT_FILE=$(shell_quote "$honey_remote_dir/honey-mutated-content.txt") bash $(shell_quote "$honey_remote_dir/honey-mounted-reverse-run.sh") push-mutated'

# Neo then runs mounted read locally:
MOUNT_ROOT=$(shell_quote "$neo_mount_canon") EXPECTED_FILE=$(shell_quote "$fixture_file") EXPECTED_CONTENT_FILE=$(shell_quote "$mutated_content_file") task lazy:mounted-smoke
EOF

write_readme() {
  cat >"$evidence_dir/README.md" <<EOF
# TCFS neo Mounted Reverse-Read Evidence

Created: $(date -u +%Y-%m-%dT%H:%M:%SZ)

This packet targets M4: honey publishes bytes, neo removes its physical copy
with \`tcfs unsync\`, honey publishes newer bytes, then neo reads the same
relative path through a mounted clean-name view. The physical neo sync root
should remain stub-only after the mounted \`cat\`; this proves mounted
on-demand read behavior rather than physical \`tcfs pull\` rehydrate.

Remote:

\`\`\`text
$remote
\`\`\`

Important files:

- \`honey-mounted-reverse-commands.txt\`: manual honey and neo commands
- \`honey-initial-push.log\`: honey initial publish transcript, when run
- \`neo-initial-pull.log\`: neo physical pull transcript, when run
- \`neo-unsync.out\`: neo physical unsync transcript, when run
- \`neo-sync-status-after-unsync.out\`: neo physical status after unsync
- \`honey-mutated-push.log\`: honey updated publish transcript, when run
- \`neo-mount.log\`: neo mount transcript, when mount startup runs
- \`neo-mounted-read.log\`: neo mounted \`ls\`/\`find\`/\`cat\` transcript, when mounted read runs
- \`neo-physical-stub-after-mounted-read.env\`: physical root state after mount read
- \`tcfs-mounted-reverse-read.toml\`: disposable neo config copied from the state dir
- \`result.env\`: pass/plan-only/failure status

Claimability note: this helper stages the M4 mounted reverse-read row. It does
not prove production Finder, broad home-directory management, or clean
delete/rename semantics.
EOF
}

cat >"$evidence_dir/run-metadata.env" <<EOF
created_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
run_id=$run_id
remote=$remote
endpoint=$endpoint
bucket=$bucket
prefix=$prefix
neo_root=$neo_canon
neo_mount_root=$neo_mount_canon
fixture_file=$fixture_file
state_dir=$state_dir
honey_host=$honey_host
honey_root=$honey_root
honey_remote_dir=$honey_remote_dir
honey_tcfs_bin=$honey_tcfs_bin
push=$push_remote
run_honey=$run_honey
neo_start_mount=$neo_start_mount
neo_nfs=$neo_nfs
allow_real_roots=$allow_real_roots
EOF
cp "$config_path" "$evidence_dir/tcfs-mounted-reverse-read.toml"
write_readme

remote_env_file=""
cleanup_remote_env() {
  [[ -n "$remote_env_file" ]] || return 0
  # shellcheck disable=SC2029
  ssh "$honey_host" "rm -f $(shell_quote "$remote_env_file")" >/dev/null 2>&1 || true
  remote_env_file=""
}

local_mount_started=0
cleanup_local_mount() {
  if [[ "$local_mount_started" == "1" && "${TCFS_MOUNTED_REVERSE_READ_KEEP_MOUNT:-0}" != "1" ]]; then
    "${tcfs_cmd[@]}" unmount "$neo_mount_canon" >/dev/null 2>&1 || fusermount3 -u "$neo_mount_canon" >/dev/null 2>&1 || true
  fi
}

on_exit() {
  local status="$?"
  cleanup_local_mount
  if [[ "$status" -ne 0 ]]; then
    if [[ ! -f "$result_env" ]] || ! grep -q '^status=0$' "$result_env" 2>/dev/null; then
      cat >"$result_env" <<EOF
status=failed
exit_status=$status
failed_stage=$current_stage
completed_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
proof=blocked-mounted-reverse-read
EOF
    fi
  fi
  exit "$status"
}
trap on_exit EXIT

start_local_mount_if_requested() {
  [[ "$neo_start_mount" == "1" ]] || return 0

  mount_args=(mount)
  if [[ "$neo_nfs" == "1" ]]; then
    mount_args+=(--nfs)
  fi
  mount_args+=("$remote" "$neo_mount_canon")

  nohup "${tcfs_cmd[@]}" "${mount_args[@]}" >"$neo_mount_log" 2>&1 &
  local mount_pid="$!"
  local_mount_started=1

  for _ in {1..300}; do
    if mount | grep -F -- "$neo_mount_canon" >/dev/null 2>&1; then
      return 0
    fi
    if ! kill -0 "$mount_pid" 2>/dev/null; then
      tail -n 80 "$neo_mount_log" >&2 || true
      fail "tcfs mount exited before mountpoint became active"
    fi
    if command -v perl >/dev/null 2>&1; then
      perl -e 'select undef, undef, undef, 0.1'
    else
      python3 -c 'import select; select.select([], [], [], 0.1)'
    fi
  done

  tail -n 80 "$neo_mount_log" >&2 || true
  fail "timed out waiting for neo mount: $neo_mount_canon"
}

if [[ "$push_remote" == "1" ]]; then
  [[ "$run_honey" == "1" ]] || fail "--push for M4 requires --run-honey"
  create_bucket_if_requested
  command -v ssh >/dev/null 2>&1 || fail "ssh not found"
  command -v scp >/dev/null 2>&1 || fail "scp not found"

  # shellcheck disable=SC2029
  ssh "$honey_host" "mkdir -p $(shell_quote "$honey_remote_dir")"
  scp "$initial_content_file" "$honey_host:$honey_remote_dir/honey-initial-content.txt"
  scp "$mutated_content_file" "$honey_host:$honey_remote_dir/honey-mutated-content.txt"
  scp "$honey_script" "$honey_host:$honey_remote_dir/honey-mounted-reverse-run.sh"

  if [[ "$forward_aws_env" == "1" ]]; then
    [[ -n "${AWS_ACCESS_KEY_ID:-}" ]] || fail "--forward-aws-env requires AWS_ACCESS_KEY_ID"
    [[ -n "${AWS_SECRET_ACCESS_KEY:-}" ]] || fail "--forward-aws-env requires AWS_SECRET_ACCESS_KEY"
    remote_env_file="$honey_remote_dir/aws-env.sh"
    aws_env_payload="$(printf 'export AWS_ACCESS_KEY_ID=%q\nexport AWS_SECRET_ACCESS_KEY=%q\n' "$AWS_ACCESS_KEY_ID" "$AWS_SECRET_ACCESS_KEY")"
    # shellcheck disable=SC2029
    ssh "$honey_host" "umask 077; cat > $(shell_quote "$remote_env_file")" <<<"$aws_env_payload"
  fi

  initial_cmd="$(printf 'TCFS_HONEY_INITIAL_CONTENT_FILE=%q bash %q push-initial' \
    "$honey_remote_dir/honey-initial-content.txt" \
    "$honey_remote_dir/honey-mounted-reverse-run.sh")"
  mutated_cmd="$(printf 'TCFS_HONEY_MUTATED_CONTENT_FILE=%q bash %q push-mutated' \
    "$honey_remote_dir/honey-mutated-content.txt" \
    "$honey_remote_dir/honey-mounted-reverse-run.sh")"
  if [[ -n "$remote_env_file" ]]; then
    initial_cmd="$(printf 'TCFS_HONEY_ENV_FILE=%q %s' "$remote_env_file" "$initial_cmd")"
    mutated_cmd="$(printf 'TCFS_HONEY_ENV_FILE=%q %s' "$remote_env_file" "$mutated_cmd")"
  fi

  current_stage="honey-initial-push"
  remote_status=0
  # shellcheck disable=SC2029
  ssh "$honey_host" "$initial_cmd" | tee "$honey_initial_log" || remote_status=$?
  if [[ "$remote_status" -ne 0 ]]; then
    cleanup_remote_env
    printf 'honey initial push failed; see %s\n' "$honey_initial_log" >&2
    exit "$remote_status"
  fi

  current_stage="neo-initial-pull"
  "${tcfs_cmd[@]}" --config "$config_path" pull "$fixture_file" "$fixture_path" --prefix "$prefix" --state "$state_json" >"$pull_initial_log" 2>&1
  cmp -s "$initial_content_file" "$fixture_path" || fail "neo initial pull did not match honey initial content"
  current_stage="neo-unsync"
  "${tcfs_cmd[@]}" --config "$config_path" unsync "$fixture_path" >"$unsync_log" 2>&1
  [[ ! -f "$fixture_path" ]] || fail "neo physical file still exists after unsync: $fixture_path"
  [[ -f "$stub_path" ]] || fail "neo physical stub missing after unsync: $stub_path"
  "${tcfs_cmd[@]}" --config "$config_path" sync-status "$fixture_path" >"$status_after_unsync" 2>&1
  grep -q "sync state: not_synced" "$status_after_unsync" || fail "neo sync-status after unsync did not report not_synced"

  current_stage="honey-mutated-push"
  remote_status=0
  # shellcheck disable=SC2029
  ssh "$honey_host" "$mutated_cmd" | tee "$honey_mutated_log" || remote_status=$?
  cleanup_remote_env
  if [[ "$remote_status" -ne 0 ]]; then
    printf 'honey mutated push failed; see %s\n' "$honey_mutated_log" >&2
    exit "$remote_status"
  fi

  current_stage="neo-mount"
  start_local_mount_if_requested
  current_stage="neo-mounted-read"
  bash "$REPO_ROOT/scripts/lazy-hydration-mounted-smoke.sh" \
    --mount-root "$neo_mount_canon" \
    --expected-file "$fixture_file" \
    --expected-content-file "$mutated_content_file" \
    --expect-entry Projects \
    --expect-entry Projects/shared \
    --max-depth 8 >"$mounted_read_log" 2>&1

  if [[ -e "$stub_path" && ! -e "$fixture_path" ]]; then
    physical_state="stub_present"
  else
    physical_state="unexpected"
  fi
  cat >"$physical_stub_env" <<EOF
neo_physical_after_mounted_read=$physical_state
stub_path=$stub_path
hydrated_path=$fixture_path
EOF
  [[ "$physical_state" == "stub_present" ]] || fail "neo physical root did not remain stub-only after mounted read"

  cat >"$result_env" <<EOF
status=0
completed_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
proof=mounted-reverse-read-current-behavior
neo_physical_after_mounted_read=$physical_state
EOF
  current_stage="completed"
else
  [[ "$run_honey" != "1" ]] || fail "--run-honey requires --push"
  printf 'plan-only: M4 mounted reverse-read packet created. Re-run with --push --run-honey when ready.\n'
  cat >"$result_env" <<EOF
status=plan-only
completed_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
proof=pending-mounted-reverse-read
EOF
fi
write_readme

printf 'mounted reverse-read evidence: %s\n' "$evidence_dir"
printf 'neo physical root: %s\n' "$neo_canon"
printf 'neo mount root: %s\n' "$neo_mount_canon"
printf 'honey mounted reverse commands: %s\n' "$honey_commands"
