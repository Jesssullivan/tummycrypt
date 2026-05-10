#!/usr/bin/env bash
#
# Prepare or run the reverse same-fixture neo/honey unsynced rehydrate packet.
#
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/neo-honey-reverse-unsynced-rehydrate-demo.sh [options]

Create an isolated neo fixture, optionally push it to a disposable remote
prefix, pull and unsync the same file in a honey physical root, mutate/push the
file on neo, then rehydrate honey and verify exact neo content plus stale .tc
cleanup.

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
      Local tcfs binary for neo push.
  --honey-host <host>
      SSH host label for honey. Default: honey.
  --honey-root <path>
      Honey physical sync root. Default: /tmp/tcfs-<run-id>-honey/root.
  --honey-remote-dir <path>
      Honey temp directory. Default: /tmp/tcfs-<run-id>-honey/run.
  --honey-tcfs-bin <path>
      Remote tcfs binary path on honey. Default: tcfs.
  --run-honey
      Copy the honey runner to honey, pull/unsync there, mutate on neo, then
      rehydrate honey.
  --honey-mounted-read
      After honey is unsynced and neo mutates, prove exact bytes through
      honey's mounted clean-name view instead of physical tcfs pull.
  --honey-mount-root <path>
      Honey mountpoint for --honey-mounted-read. Default:
      /tmp/tcfs-<run-id>-honey/mount.
  --honey-start-mount
      Start tcfs mount on honey for --honey-mounted-read.
  --forward-aws-env
      With --run-honey, forward current AWS env to honey for this smoke run.
  --allow-real-roots
      Allow direct HOME, ~/Documents, or ~/git neo roots. Not recommended.
  -h, --help
      Show this help.

Environment mirrors:
  TCFS_REVERSE_UNSYNCED_REHYDRATE_REMOTE
  TCFS_REVERSE_UNSYNCED_REHYDRATE_NEO_ROOT
  TCFS_REVERSE_UNSYNCED_REHYDRATE_EVIDENCE_DIR
  TCFS_REVERSE_UNSYNCED_REHYDRATE_STATE_DIR
  TCFS_REVERSE_UNSYNCED_REHYDRATE_PUSH=1
  TCFS_REVERSE_UNSYNCED_REHYDRATE_CREATE_BUCKET=1
  TCFS_REVERSE_UNSYNCED_REHYDRATE_RUN_HONEY=1
  TCFS_REVERSE_UNSYNCED_REHYDRATE_HONEY_MOUNTED_READ=1
  TCFS_REVERSE_UNSYNCED_REHYDRATE_HONEY_MOUNT_ROOT
  TCFS_REVERSE_UNSYNCED_REHYDRATE_HONEY_START_MOUNT=1
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
run_id="reverse-unsynced-rehydrate-${timestamp}-$$"

remote="${TCFS_REVERSE_UNSYNCED_REHYDRATE_REMOTE:-seaweedfs://localhost:8333/tcfs/${run_id}}"
neo_root="${TCFS_REVERSE_UNSYNCED_REHYDRATE_NEO_ROOT:-$HOME/TCFS Pilot/runs/${run_id}/neo}"
evidence_dir="${TCFS_REVERSE_UNSYNCED_REHYDRATE_EVIDENCE_DIR:-${TMPDIR:-/tmp}/tcfs-${run_id}}"
state_dir="${TCFS_REVERSE_UNSYNCED_REHYDRATE_STATE_DIR:-}"
push_remote="$(bool_env TCFS_REVERSE_UNSYNCED_REHYDRATE_PUSH "${TCFS_REVERSE_UNSYNCED_REHYDRATE_PUSH:-0}")"
create_bucket="$(bool_env TCFS_REVERSE_UNSYNCED_REHYDRATE_CREATE_BUCKET "${TCFS_REVERSE_UNSYNCED_REHYDRATE_CREATE_BUCKET:-0}")"
run_honey="$(bool_env TCFS_REVERSE_UNSYNCED_REHYDRATE_RUN_HONEY "${TCFS_REVERSE_UNSYNCED_REHYDRATE_RUN_HONEY:-0}")"
honey_mounted_read="$(bool_env TCFS_REVERSE_UNSYNCED_REHYDRATE_HONEY_MOUNTED_READ "${TCFS_REVERSE_UNSYNCED_REHYDRATE_HONEY_MOUNTED_READ:-0}")"
honey_mount_root="${TCFS_REVERSE_UNSYNCED_REHYDRATE_HONEY_MOUNT_ROOT:-/tmp/tcfs-${run_id}-honey/mount}"
honey_start_mount="$(bool_env TCFS_REVERSE_UNSYNCED_REHYDRATE_HONEY_START_MOUNT "${TCFS_REVERSE_UNSYNCED_REHYDRATE_HONEY_START_MOUNT:-0}")"
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
    --honey-mounted-read)
      honey_mounted_read=1
      shift
      ;;
    --honey-mount-root)
      [[ $# -ge 2 ]] || fail "--honey-mount-root requires a value"
      honey_mount_root="$2"
      shift 2
      ;;
    --honey-start-mount)
      honey_start_mount=1
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
assert_safe_remote_path "--honey-mount-root" "$honey_mount_root"
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
config_path="$state_dir/tcfs-reverse-unsynced-rehydrate.toml"
mc_config_dir="$state_dir/mc"
fixture_file="Projects/shared/reverse-notes.md"
fixture_path="$neo_canon/$fixture_file"
initial_content_file="$evidence_dir/neo-initial-content.txt"
mutated_content_file="$evidence_dir/neo-mutated-content.txt"
local_tree="$evidence_dir/neo-tree.txt"
push_initial_log="$evidence_dir/neo-initial-push.log"
push_mutated_log="$evidence_dir/neo-mutated-push.log"
honey_script="$evidence_dir/honey-reverse-run.sh"
honey_commands="$evidence_dir/honey-reverse-commands.txt"
honey_prepare_log="$evidence_dir/honey-prepare-unsync.log"
honey_rehydrate_log="$evidence_dir/honey-rehydrate.log"
honey_mounted_log="$evidence_dir/honey-mounted-read.log"
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
    mc --config-dir "$mc_config_dir" alias set tcfs-reverse "$endpoint" "$AWS_ACCESS_KEY_ID" "$AWS_SECRET_ACCESS_KEY" >/dev/null
    mc --config-dir "$mc_config_dir" mb --ignore-existing "tcfs-reverse/$bucket" >/dev/null
    return 0
  fi

  fail "--create-bucket requested, but none of aws, s5cmd, or mc was found"
}

mkdir -p "$neo_canon/Projects/shared" "$cache_root" "$honey_evidence_dir"

cat >"$fixture_path" <<'EOF'
# Reverse shared TCFS note

version: neo-initial
body: honey will remove this local copy before neo changes it.
EOF

cat >"$mutated_content_file" <<'EOF'
# Reverse shared TCFS note

version: neo-mutated
body: neo changed this while honey had only the .tc stub.
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
ENDPOINT=$(shell_quote "$endpoint")
REGION=$(shell_quote "$region")
BUCKET=$(shell_quote "$bucket")
PREFIX=$(shell_quote "$prefix")
TCFS_BIN=$(shell_quote "$honey_tcfs_bin")
HONEY_ROOT_RAW=$(single_quote "$honey_root")
MOUNT_ROOT_RAW=$(single_quote "$honey_mount_root")
RUN_DIR=$(shell_quote "$honey_remote_dir")
FIXTURE_FILE=$(shell_quote "$fixture_file")
INITIAL_CONTENT="\${TCFS_HONEY_INITIAL_CONTENT_FILE:-$(shell_quote "$honey_remote_dir/neo-initial-content.txt")}"
MUTATED_CONTENT="\${TCFS_HONEY_MUTATED_CONTENT_FILE:-$(shell_quote "$honey_remote_dir/neo-mutated-content.txt")}"
SMOKE_SCRIPT="\${TCFS_HONEY_SMOKE_SCRIPT:-$(shell_quote "$honey_remote_dir/lazy-hydration-mounted-smoke.sh")}"
START_MOUNT="\${TCFS_HONEY_START_MOUNT:-0}"
MOUNT_LOG="\${TCFS_HONEY_MOUNT_LOG:-$(shell_quote "$honey_remote_dir/honey-mount.log")}"

case "\$HONEY_ROOT_RAW" in
  "~/"*) HONEY_ROOT="\${HOME}/\${HONEY_ROOT_RAW#\\~/}" ;;
  *) HONEY_ROOT="\$HONEY_ROOT_RAW" ;;
esac
case "\$MOUNT_ROOT_RAW" in
  "~/"*) MOUNT_ROOT="\${HOME}/\${MOUNT_ROOT_RAW#\\~/}" ;;
  *) MOUNT_ROOT="\$MOUNT_ROOT_RAW" ;;
esac

if [[ -n "\${TCFS_HONEY_ENV_FILE:-}" ]]; then
  # shellcheck disable=SC1090
  source "\$TCFS_HONEY_ENV_FILE"
fi
if [[ -n "\${TCFS_HONEY_MOUNT_ROOT:-}" ]]; then
  MOUNT_ROOT_RAW="\$TCFS_HONEY_MOUNT_ROOT"
fi
case "\$MOUNT_ROOT_RAW" in
  "~/"*) MOUNT_ROOT="\${HOME}/\${MOUNT_ROOT_RAW#\\~/}" ;;
  *) MOUNT_ROOT="\$MOUNT_ROOT_RAW" ;;
esac

mode="\${1:-}"
[[ -n "\$mode" ]] || { echo "mode required: prepare-unsync, rehydrate, or mounted-read" >&2; exit 2; }

STATE_DIR="\$RUN_DIR/honey-state"
CACHE_ROOT="\$STATE_DIR/cache"
EVIDENCE_DIR="\$RUN_DIR/honey-evidence"
CONFIG_PATH="\$STATE_DIR/tcfs-reverse-unsynced-rehydrate.toml"
STATE_JSON="\$STATE_DIR/state.json"
FIXTURE_PATH="\$HONEY_ROOT/\$FIXTURE_FILE"
STUB_PATH="\${FIXTURE_PATH}.tc"
MOUNT_STARTED=0

cleanup_mount() {
  if [[ "\$MOUNT_STARTED" == "1" && "\${TCFS_HONEY_KEEP_MOUNT:-0}" != "1" ]]; then
    "\$TCFS_BIN" unmount "\$MOUNT_ROOT" >/dev/null 2>&1 || fusermount3 -u "\$MOUNT_ROOT" >/dev/null 2>&1 || true
  fi
}
trap cleanup_mount EXIT

mkdir -p "\$(dirname "\$FIXTURE_PATH")" "\$CACHE_ROOT" "\$EVIDENCE_DIR" "\$MOUNT_ROOT"

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
  prepare-unsync)
    "\$TCFS_BIN" --config "\$CONFIG_PATH" pull "\$FIXTURE_FILE" "\$FIXTURE_PATH" --prefix "\$PREFIX" --state "\$STATE_JSON" >"\$EVIDENCE_DIR/honey-initial-pull.log" 2>&1
    cmp -s "\$INITIAL_CONTENT" "\$FIXTURE_PATH"
    "\$TCFS_BIN" --config "\$CONFIG_PATH" unsync "\$FIXTURE_PATH" >"\$EVIDENCE_DIR/honey-unsync.out" 2>&1
    [[ ! -f "\$FIXTURE_PATH" ]] || { echo "honey hydrated file still exists after unsync: \$FIXTURE_PATH" >&2; exit 1; }
    [[ -f "\$STUB_PATH" ]] || { echo "honey stub missing after unsync: \$STUB_PATH" >&2; exit 1; }
    "\$TCFS_BIN" --config "\$CONFIG_PATH" sync-status "\$FIXTURE_PATH" >"\$EVIDENCE_DIR/honey-sync-status-after-unsync.out" 2>&1
    grep -q "sync state: not_synced" "\$EVIDENCE_DIR/honey-sync-status-after-unsync.out"
    echo "honey reverse prepare unsync ok: \$FIXTURE_FILE"
    ;;
  rehydrate)
    "\$TCFS_BIN" --config "\$CONFIG_PATH" pull "\$FIXTURE_FILE" "\$FIXTURE_PATH" --prefix "\$PREFIX" --state "\$STATE_JSON" >"\$EVIDENCE_DIR/honey-rehydrate-pull.log" 2>&1
    cmp -s "\$MUTATED_CONTENT" "\$FIXTURE_PATH"
    "\$TCFS_BIN" --config "\$CONFIG_PATH" sync-status "\$FIXTURE_PATH" >"\$EVIDENCE_DIR/honey-sync-status-after-rehydrate.out" 2>&1
    grep -q "sync state: synced" "\$EVIDENCE_DIR/honey-sync-status-after-rehydrate.out"
    if [[ -e "\$STUB_PATH" ]]; then
      {
        echo "stub_after_pull=present"
        echo "stub_path=\$STUB_PATH"
      } >"\$EVIDENCE_DIR/honey-stub-status.env"
      echo "stale honey stub still present after rehydrate: \$STUB_PATH" >&2
      exit 1
    fi
    {
      echo "stub_after_pull=absent"
      echo "stub_path=\$STUB_PATH"
    } >"\$EVIDENCE_DIR/honey-stub-status.env"
    echo "honey reverse rehydrate ok: \$FIXTURE_FILE"
    echo "stub_after_pull=absent"
    ;;
  mounted-read)
    if [[ "\$START_MOUNT" == "1" ]]; then
      nohup "\$TCFS_BIN" mount "\$REMOTE" "\$MOUNT_ROOT" >"\$MOUNT_LOG" 2>&1 &
      mount_pid="\$!"
      MOUNT_STARTED=1
      for _ in {1..300}; do
        if mount | grep -F -- "\$MOUNT_ROOT" >/dev/null 2>&1; then
          break
        fi
        if ! kill -0 "\$mount_pid" 2>/dev/null; then
          tail -n 80 "\$MOUNT_LOG" >&2 || true
          echo "honey tcfs mount exited before mountpoint became active" >&2
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
        echo "timed out waiting for honey mount: \$MOUNT_ROOT" >&2
        exit 1
      fi
    fi
    bash "\$SMOKE_SCRIPT" \
      --mount-root "\$MOUNT_ROOT" \
      --expected-file "\$FIXTURE_FILE" \
      --expected-content-file "\$MUTATED_CONTENT" \
      --expect-entry Projects \
      --expect-entry Projects/shared \
      --max-depth 8 >"\$EVIDENCE_DIR/honey-mounted-read.log" 2>&1
    if [[ -e "\$STUB_PATH" && ! -e "\$FIXTURE_PATH" ]]; then
      physical_state="stub_present"
    else
      physical_state="unexpected"
    fi
    {
      echo "honey_physical_after_mounted_read=\$physical_state"
      echo "stub_path=\$STUB_PATH"
      echo "hydrated_path=\$FIXTURE_PATH"
    } >"\$EVIDENCE_DIR/honey-physical-stub-after-mounted-read.env"
    [[ "\$physical_state" == "stub_present" ]] || {
      echo "honey physical root did not remain stub-only after mounted read" >&2
      exit 1
    }
    echo "honey reverse mounted read ok: \$FIXTURE_FILE"
    echo "honey_physical_after_mounted_read=stub_present"
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

# Copy expected content and the honey reverse runner:
scp $(shell_quote "$initial_content_file") $(shell_quote "$honey_host"):$(shell_quote "$honey_remote_dir/neo-initial-content.txt")
scp $(shell_quote "$mutated_content_file") $(shell_quote "$honey_host"):$(shell_quote "$honey_remote_dir/neo-mutated-content.txt")
scp $(shell_quote "$honey_script") $(shell_quote "$honey_host"):$(shell_quote "$honey_remote_dir/honey-reverse-run.sh")
scp $(shell_quote "$REPO_ROOT/scripts/lazy-hydration-mounted-smoke.sh") $(shell_quote "$honey_host"):$(shell_quote "$honey_remote_dir/lazy-hydration-mounted-smoke.sh")

# Pull and unsync on honey:
ssh $(shell_quote "$honey_host") 'TCFS_HONEY_INITIAL_CONTENT_FILE=$(shell_quote "$honey_remote_dir/neo-initial-content.txt") bash $(shell_quote "$honey_remote_dir/honey-reverse-run.sh") prepare-unsync'

# After neo mutates and pushes, rehydrate honey:
ssh $(shell_quote "$honey_host") 'TCFS_HONEY_MUTATED_CONTENT_FILE=$(shell_quote "$honey_remote_dir/neo-mutated-content.txt") bash $(shell_quote "$honey_remote_dir/honey-reverse-run.sh") rehydrate'

# Or, for the Linux-mounted reverse-read row:
ssh $(shell_quote "$honey_host") 'TCFS_HONEY_MUTATED_CONTENT_FILE=$(shell_quote "$honey_remote_dir/neo-mutated-content.txt") TCFS_HONEY_SMOKE_SCRIPT=$(shell_quote "$honey_remote_dir/lazy-hydration-mounted-smoke.sh") TCFS_HONEY_MOUNT_ROOT=$(shell_quote "$honey_mount_root") TCFS_HONEY_START_MOUNT=1 bash $(shell_quote "$honey_remote_dir/honey-reverse-run.sh") mounted-read'
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
honey_mount_root=$honey_mount_root
honey_remote_dir=$honey_remote_dir
honey_tcfs_bin=$honey_tcfs_bin
push=$push_remote
run_honey=$run_honey
honey_mounted_read=$honey_mounted_read
honey_start_mount=$honey_start_mount
allow_real_roots=$allow_real_roots
EOF
cp "$config_path" "$evidence_dir/tcfs-reverse-unsynced-rehydrate.toml"

if [[ "$push_remote" == "1" ]]; then
  create_bucket_if_requested
  printf 'pushing initial neo fixture to %s\n' "$remote"
  "${tcfs_cmd[@]}" --config "$config_path" push "$neo_canon" --prefix "$prefix" --state "$state_json" >"$push_initial_log" 2>&1
else
  printf 'plan-only: fixture created but not pushed. Re-run with --push when ready.\n'
fi

if [[ "$run_honey" == "1" ]]; then
  [[ "$push_remote" == "1" ]] || fail "--run-honey requires --push"
  command -v ssh >/dev/null 2>&1 || fail "ssh not found"
  command -v scp >/dev/null 2>&1 || fail "scp not found"

  printf 'running honey reverse unsync on %s\n' "$honey_host"
  # shellcheck disable=SC2029
  ssh "$honey_host" "mkdir -p $(shell_quote "$honey_remote_dir")"
  scp "$initial_content_file" "$honey_host:$honey_remote_dir/neo-initial-content.txt"
  scp "$mutated_content_file" "$honey_host:$honey_remote_dir/neo-mutated-content.txt"
  scp "$honey_script" "$honey_host:$honey_remote_dir/honey-reverse-run.sh"
  if [[ "$honey_mounted_read" == "1" ]]; then
    scp "$REPO_ROOT/scripts/lazy-hydration-mounted-smoke.sh" "$honey_host:$honey_remote_dir/lazy-hydration-mounted-smoke.sh"
  fi

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

  prepare_cmd="$(printf 'TCFS_HONEY_INITIAL_CONTENT_FILE=%q bash %q prepare-unsync' \
    "$honey_remote_dir/neo-initial-content.txt" \
    "$honey_remote_dir/honey-reverse-run.sh")"
  if [[ "$honey_mounted_read" == "1" ]]; then
    rehydrate_cmd="$(printf 'TCFS_HONEY_MUTATED_CONTENT_FILE=%q TCFS_HONEY_SMOKE_SCRIPT=%q TCFS_HONEY_MOUNT_ROOT=%q TCFS_HONEY_START_MOUNT=%q TCFS_HONEY_MOUNT_LOG=%q bash %q mounted-read' \
      "$honey_remote_dir/neo-mutated-content.txt" \
      "$honey_remote_dir/lazy-hydration-mounted-smoke.sh" \
      "$honey_mount_root" \
      "$honey_start_mount" \
      "$honey_remote_dir/honey-mount.log" \
      "$honey_remote_dir/honey-reverse-run.sh")"
  else
    rehydrate_cmd="$(printf 'TCFS_HONEY_MUTATED_CONTENT_FILE=%q bash %q rehydrate' \
      "$honey_remote_dir/neo-mutated-content.txt" \
      "$honey_remote_dir/honey-reverse-run.sh")"
  fi
  if [[ -n "$remote_env_file" ]]; then
    prepare_cmd="$(printf 'TCFS_HONEY_ENV_FILE=%q %s' "$remote_env_file" "$prepare_cmd")"
    rehydrate_cmd="$(printf 'TCFS_HONEY_ENV_FILE=%q %s' "$remote_env_file" "$rehydrate_cmd")"
  fi

  remote_status=0
  # shellcheck disable=SC2029
  ssh "$honey_host" "$prepare_cmd" | tee "$honey_prepare_log" || remote_status=$?
  if [[ "$remote_status" -ne 0 ]]; then
    cleanup_remote_env
    printf 'honey prepare-unsync failed; see %s\n' "$honey_prepare_log" >&2
    exit "$remote_status"
  fi

  printf 'mutating and pushing neo fixture\n'
  cp "$mutated_content_file" "$fixture_path"
  "${tcfs_cmd[@]}" --config "$config_path" push "$neo_canon" --prefix "$prefix" --state "$state_json" >"$push_mutated_log" 2>&1

  remote_status=0
  final_honey_log="$honey_rehydrate_log"
  if [[ "$honey_mounted_read" == "1" ]]; then
    final_honey_log="$honey_mounted_log"
  fi
  # shellcheck disable=SC2029
  ssh "$honey_host" "$rehydrate_cmd" | tee "$final_honey_log" || remote_status=$?
  # Best effort: copy detailed remote transcripts when the real remote shell created them.
  scp "$honey_host:$honey_remote_dir/honey-evidence/"* "$honey_evidence_dir/" >/dev/null 2>&1 || true
  cleanup_remote_env
  if [[ "$remote_status" -ne 0 ]]; then
    printf 'honey final read/rehydrate failed; see %s\n' "$final_honey_log" >&2
    exit "$remote_status"
  fi
  if [[ "$honey_mounted_read" == "1" ]]; then
    grep -q "honey reverse mounted read ok" "$final_honey_log" || fail "honey mounted read log missing success marker"
    grep -q "honey_physical_after_mounted_read=stub_present" "$final_honey_log" || fail "honey mounted read log missing stub marker"
    proof="linux-mounted-reverse-read-current-behavior"
  else
    grep -q "honey reverse rehydrate ok" "$final_honey_log" || fail "honey rehydrate log missing success marker"
    grep -q "stub_after_pull=absent" "$final_honey_log" || fail "honey rehydrate log missing stub cleanup marker"
    proof="reverse-same-fixture-unsynced-rehydrate"
  fi

  cat >"$result_env" <<EOF
status=0
completed_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
proof=$proof
EOF
else
  if [[ "$push_remote" == "1" ]]; then
    result_status="pushed"
    if [[ "$honey_mounted_read" == "1" ]]; then
      result_proof="pending-honey-mounted-reverse-read"
    else
      result_proof="pending-honey-reverse"
    fi
  else
    result_status="plan-only"
    if [[ "$honey_mounted_read" == "1" ]]; then
      result_proof="pending-push-or-honey-mounted-reverse-read"
    else
      result_proof="pending-push-or-honey-reverse"
    fi
  fi
  cat >"$result_env" <<EOF
status=$result_status
completed_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
proof=$result_proof
EOF
fi

cat >"$evidence_dir/README.md" <<EOF
# TCFS neo/honey Reverse Unsynced Rehydrate Evidence

Created: $(date -u +%Y-%m-%dT%H:%M:%SZ)

This packet targets the reverse same-fixture permutation:

1. neo creates and pushes \`$fixture_file\` to a disposable prefix.
2. honey pulls that file into a physical sync root and runs \`tcfs unsync\`, so
   honey keeps only \`$fixture_file.tc\`.
3. neo mutates and pushes the same relative path.
4. honey either runs \`tcfs pull $fixture_file\` and must receive neo's exact
   content, or, with \`--honey-mounted-read\`, reads the latest bytes through a
   mounted clean-name view while the physical root remains stub-only.
5. In pull mode, honey's adjacent \`.tc\` stub must be gone after rehydrate. In
   mounted-read mode, the physical stub must remain present after mounted
   \`cat\`.

Remote:

\`\`\`text
$remote
\`\`\`

Important files:

- \`neo-tree.txt\`: isolated neo fixture tree
- \`tcfs-reverse-unsynced-rehydrate.toml\`: disposable neo config copied from the state dir
- \`honey-reverse-commands.txt\`: manual honey commands
- \`honey-prepare-unsync.log\`: honey pull/unsync transcript, when run
- \`honey-rehydrate.log\`: honey rehydrate transcript, when run
- \`honey-mounted-read.log\`: honey mounted clean-name read transcript, when
  \`--honey-mounted-read\` is used
- \`honey-evidence/\`: detailed remote transcripts, copied back when available
- \`neo-initial-push.log\`: neo initial push transcript, when pushed
- \`neo-mutated-push.log\`: neo mutation push transcript, when honey was run
- \`result.env\`: pass/plan-only status

This helper uses an isolated neo root under \`$neo_canon\` and a honey root
\`$honey_root\`; it does not target real \`~/Documents\`, \`~/git\`, dotfiles,
or broad home-directory paths unless \`--allow-real-roots\` is explicitly
supplied.
EOF

printf 'reverse unsynced rehydrate evidence: %s\n' "$evidence_dir"
printf 'honey reverse commands: %s\n' "$honey_commands"
