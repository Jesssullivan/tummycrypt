#!/usr/bin/env bash
#
# Prepare or run the M8 delete/rename while peer-unsynced proof packet.
#
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/neo-honey-delete-rename-unsynced-demo.sh [options]

Create isolated neo fixtures, optionally push them to a disposable remote
prefix, pull and unsync the same files on honey, then delete one file and
rename another from neo. Honey then verifies the current peer-unsynced behavior:
old paths do not rehydrate, the renamed new path does hydrate exact bytes, and
stale old .tc stubs remain as an explicit product/QA gap.

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
      Push the initial neo fixtures.
  --create-bucket
      Best-effort remote bucket creation before pushing.
  --tcfs-bin <path>
      Local tcfs binary for neo push/rm.
  --honey-host <host>
      SSH host label for honey. Default: honey.
  --honey-root <path>
      Honey physical sync root. Default: /tmp/tcfs-<run-id>-honey/root.
  --honey-remote-dir <path>
      Honey temp directory. Default: /tmp/tcfs-<run-id>-honey/run.
  --honey-tcfs-bin <path>
      Remote tcfs binary path on honey. Default: tcfs.
  --run-honey
      Copy the honey runner to honey, pull/unsync there, perform peer delete
      and rename on neo, then verify honey behavior.
  --forward-aws-env
      With --run-honey, forward current AWS env to honey for this smoke run.
  --allow-real-roots
      Allow direct HOME, ~/Documents, or ~/git neo roots. Not recommended.
  -h, --help
      Show this help.

Environment mirrors:
  TCFS_DELETE_RENAME_UNSYNCED_REMOTE
  TCFS_DELETE_RENAME_UNSYNCED_NEO_ROOT
  TCFS_DELETE_RENAME_UNSYNCED_EVIDENCE_DIR
  TCFS_DELETE_RENAME_UNSYNCED_STATE_DIR
  TCFS_DELETE_RENAME_UNSYNCED_PUSH=1
  TCFS_DELETE_RENAME_UNSYNCED_CREATE_BUCKET=1
  TCFS_DELETE_RENAME_UNSYNCED_RUN_HONEY=1
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
run_id="delete-rename-unsynced-${timestamp}-$$"

remote="${TCFS_DELETE_RENAME_UNSYNCED_REMOTE:-seaweedfs://localhost:8333/tcfs/${run_id}}"
neo_root="${TCFS_DELETE_RENAME_UNSYNCED_NEO_ROOT:-$HOME/TCFS Pilot/runs/${run_id}/neo}"
evidence_dir="${TCFS_DELETE_RENAME_UNSYNCED_EVIDENCE_DIR:-${TMPDIR:-/tmp}/tcfs-${run_id}}"
state_dir="${TCFS_DELETE_RENAME_UNSYNCED_STATE_DIR:-}"
push_remote="$(bool_env TCFS_DELETE_RENAME_UNSYNCED_PUSH "${TCFS_DELETE_RENAME_UNSYNCED_PUSH:-0}")"
create_bucket="$(bool_env TCFS_DELETE_RENAME_UNSYNCED_CREATE_BUCKET "${TCFS_DELETE_RENAME_UNSYNCED_CREATE_BUCKET:-0}")"
run_honey="$(bool_env TCFS_DELETE_RENAME_UNSYNCED_RUN_HONEY "${TCFS_DELETE_RENAME_UNSYNCED_RUN_HONEY:-0}")"
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
config_path="$state_dir/tcfs-delete-rename-unsynced.toml"
mc_config_dir="$state_dir/mc"
delete_file="Projects/shared/delete-me.md"
rename_old_file="Projects/shared/rename-old.md"
rename_new_file="Projects/shared/rename-new.md"
delete_path="$neo_canon/$delete_file"
rename_old_path="$neo_canon/$rename_old_file"
rename_new_path="$neo_canon/$rename_new_file"
delete_content_file="$evidence_dir/delete-initial-content.txt"
rename_content_file="$evidence_dir/rename-content.txt"
local_tree="$evidence_dir/neo-tree.txt"
local_tree_after="$evidence_dir/neo-tree-after-delete-rename.txt"
initial_push_log="$evidence_dir/neo-initial-push.log"
delete_log="$evidence_dir/neo-delete.log"
rename_push_log="$evidence_dir/neo-rename-push.log"
rename_delete_old_log="$evidence_dir/neo-rename-delete-old.log"
honey_script="$evidence_dir/honey-delete-rename-run.sh"
honey_commands="$evidence_dir/honey-delete-rename-commands.txt"
honey_prepare_log="$evidence_dir/honey-prepare-unsync.log"
honey_delete_log="$evidence_dir/honey-verify-delete.log"
honey_rename_log="$evidence_dir/honey-verify-rename.log"
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
    mc --config-dir "$mc_config_dir" alias set tcfs-m8 "$endpoint" "$AWS_ACCESS_KEY_ID" "$AWS_SECRET_ACCESS_KEY" >/dev/null
    mc --config-dir "$mc_config_dir" mb --ignore-existing "tcfs-m8/$bucket" >/dev/null
    return 0
  fi

  fail "--create-bucket requested, but none of aws, s5cmd, or mc was found"
}

mkdir -p "$neo_canon/Projects/shared" "$cache_root" "$honey_evidence_dir"

cat >"$delete_path" <<'EOF'
# TCFS M8 delete target

version: neo-initial
body: honey will be unsynced before neo deletes this file.
EOF

cat >"$rename_old_path" <<'EOF'
# TCFS M8 rename target

version: neo-initial
body: honey will be unsynced before neo renames this file.
EOF

cp "$delete_path" "$delete_content_file"
cp "$rename_old_path" "$rename_content_file"
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
DELETE_FILE=$(shell_quote "$delete_file")
RENAME_OLD_FILE=$(shell_quote "$rename_old_file")
RENAME_NEW_FILE=$(shell_quote "$rename_new_file")
DELETE_CONTENT="\${TCFS_HONEY_DELETE_CONTENT_FILE:-$(shell_quote "$honey_remote_dir/delete-initial-content.txt")}"
RENAME_CONTENT="\${TCFS_HONEY_RENAME_CONTENT_FILE:-$(shell_quote "$honey_remote_dir/rename-content.txt")}"

case "\$HONEY_ROOT_RAW" in
  "~/"*) HONEY_ROOT="\${HOME}/\${HONEY_ROOT_RAW#\\~/}" ;;
  *) HONEY_ROOT="\$HONEY_ROOT_RAW" ;;
esac

if [[ -n "\${TCFS_HONEY_ENV_FILE:-}" ]]; then
  # shellcheck disable=SC1090
  source "\$TCFS_HONEY_ENV_FILE"
fi

mode="\${1:-}"
[[ -n "\$mode" ]] || { echo "mode required: prepare-unsync, verify-delete, or verify-rename" >&2; exit 2; }

STATE_DIR="\$RUN_DIR/honey-state"
CACHE_ROOT="\$STATE_DIR/cache"
EVIDENCE_DIR="\$RUN_DIR/honey-evidence"
CONFIG_PATH="\$STATE_DIR/tcfs-delete-rename-unsynced.toml"
STATE_JSON="\$STATE_DIR/state.json"
DELETE_PATH="\$HONEY_ROOT/\$DELETE_FILE"
RENAME_OLD_PATH="\$HONEY_ROOT/\$RENAME_OLD_FILE"
RENAME_NEW_PATH="\$HONEY_ROOT/\$RENAME_NEW_FILE"
DELETE_STUB="\${DELETE_PATH}.tc"
RENAME_OLD_STUB="\${RENAME_OLD_PATH}.tc"
RENAME_NEW_STUB="\${RENAME_NEW_PATH}.tc"

mkdir -p "\$(dirname "\$DELETE_PATH")" "\$CACHE_ROOT" "\$EVIDENCE_DIR"

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
    "\$TCFS_BIN" --config "\$CONFIG_PATH" pull "\$DELETE_FILE" "\$DELETE_PATH" --prefix "\$PREFIX" --state "\$STATE_JSON" >"\$EVIDENCE_DIR/honey-delete-initial-pull.log" 2>&1
    cmp -s "\$DELETE_CONTENT" "\$DELETE_PATH"
    "\$TCFS_BIN" --config "\$CONFIG_PATH" pull "\$RENAME_OLD_FILE" "\$RENAME_OLD_PATH" --prefix "\$PREFIX" --state "\$STATE_JSON" >"\$EVIDENCE_DIR/honey-rename-initial-pull.log" 2>&1
    cmp -s "\$RENAME_CONTENT" "\$RENAME_OLD_PATH"
    "\$TCFS_BIN" --config "\$CONFIG_PATH" unsync "\$DELETE_PATH" >"\$EVIDENCE_DIR/honey-delete-unsync.out" 2>&1
    "\$TCFS_BIN" --config "\$CONFIG_PATH" unsync "\$RENAME_OLD_PATH" >"\$EVIDENCE_DIR/honey-rename-unsync.out" 2>&1
    [[ ! -f "\$DELETE_PATH" && -f "\$DELETE_STUB" ]] || { echo "delete target did not become stub-only" >&2; exit 1; }
    [[ ! -f "\$RENAME_OLD_PATH" && -f "\$RENAME_OLD_STUB" ]] || { echo "rename target did not become stub-only" >&2; exit 1; }
    "\$TCFS_BIN" --config "\$CONFIG_PATH" sync-status "\$DELETE_PATH" >"\$EVIDENCE_DIR/honey-delete-status-after-unsync.out" 2>&1
    "\$TCFS_BIN" --config "\$CONFIG_PATH" sync-status "\$RENAME_OLD_PATH" >"\$EVIDENCE_DIR/honey-rename-status-after-unsync.out" 2>&1
    grep -q "sync state: not_synced" "\$EVIDENCE_DIR/honey-delete-status-after-unsync.out"
    grep -q "sync state: not_synced" "\$EVIDENCE_DIR/honey-rename-status-after-unsync.out"
    echo "honey delete/rename prepare unsync ok"
    ;;
  verify-delete)
    if "\$TCFS_BIN" --config "\$CONFIG_PATH" pull "\$DELETE_FILE" "\$DELETE_PATH" --prefix "\$PREFIX" --state "\$STATE_JSON" >"\$EVIDENCE_DIR/honey-delete-pull-after-peer-delete.log" 2>&1; then
      echo "delete old path unexpectedly rehydrated" >&2
      exit 1
    fi
    if "\$TCFS_BIN" --config "\$CONFIG_PATH" pull "\$DELETE_FILE" "\$DELETE_PATH" --prefix "\$PREFIX" --state "\$STATE_JSON" >"\$EVIDENCE_DIR/honey-delete-pull-after-peer-delete-repeat.log" 2>&1; then
      echo "delete old path unexpectedly rehydrated on repeated pull" >&2
      exit 1
    fi
    "\$TCFS_BIN" --config "\$CONFIG_PATH" sync-status "\$DELETE_PATH" >"\$EVIDENCE_DIR/honey-delete-status-after-peer-delete.out" 2>&1 || true
    "\$TCFS_BIN" --config "\$CONFIG_PATH" sync-status "\$DELETE_STUB" >"\$EVIDENCE_DIR/honey-delete-stub-status-after-peer-delete.out" 2>&1 || true
    if [[ -e "\$DELETE_STUB" ]]; then
      delete_stub_after_failed_pull=present
    else
      delete_stub_after_failed_pull=absent
    fi
    {
      echo "delete_old_pull=failed_as_expected"
      echo "delete_old_pull_repeat=failed_as_expected"
      echo "delete_stub_after_failed_pull=\$delete_stub_after_failed_pull"
      echo "delete_stub_path=\$DELETE_STUB"
    } >"\$EVIDENCE_DIR/honey-delete-peer-result.env"
    echo "honey peer-delete verify ok: \$DELETE_FILE"
    echo "delete_old_pull=failed_as_expected"
    echo "delete_old_pull_repeat=failed_as_expected"
    echo "delete_stub_after_failed_pull=\$delete_stub_after_failed_pull"
    ;;
  verify-rename)
    if "\$TCFS_BIN" --config "\$CONFIG_PATH" pull "\$RENAME_OLD_FILE" "\$RENAME_OLD_PATH" --prefix "\$PREFIX" --state "\$STATE_JSON" >"\$EVIDENCE_DIR/honey-rename-old-pull-after-peer-rename.log" 2>&1; then
      echo "rename old path unexpectedly rehydrated" >&2
      exit 1
    fi
    if "\$TCFS_BIN" --config "\$CONFIG_PATH" pull "\$RENAME_OLD_FILE" "\$RENAME_OLD_PATH" --prefix "\$PREFIX" --state "\$STATE_JSON" >"\$EVIDENCE_DIR/honey-rename-old-pull-after-peer-rename-repeat.log" 2>&1; then
      echo "rename old path unexpectedly rehydrated on repeated pull" >&2
      exit 1
    fi
    "\$TCFS_BIN" --config "\$CONFIG_PATH" sync-status "\$RENAME_OLD_PATH" >"\$EVIDENCE_DIR/honey-rename-old-status-after-peer-rename.out" 2>&1 || true
    "\$TCFS_BIN" --config "\$CONFIG_PATH" sync-status "\$RENAME_OLD_STUB" >"\$EVIDENCE_DIR/honey-rename-old-stub-status-after-peer-rename.out" 2>&1 || true
    "\$TCFS_BIN" --config "\$CONFIG_PATH" pull "\$RENAME_NEW_FILE" "\$RENAME_NEW_PATH" --prefix "\$PREFIX" --state "\$STATE_JSON" >"\$EVIDENCE_DIR/honey-rename-new-pull.log" 2>&1
    cmp -s "\$RENAME_CONTENT" "\$RENAME_NEW_PATH"
    "\$TCFS_BIN" --config "\$CONFIG_PATH" pull "\$RENAME_NEW_FILE" "\$RENAME_NEW_PATH" --prefix "\$PREFIX" --state "\$STATE_JSON" >"\$EVIDENCE_DIR/honey-rename-new-pull-repeat.log" 2>&1
    cmp -s "\$RENAME_CONTENT" "\$RENAME_NEW_PATH"
    "\$TCFS_BIN" --config "\$CONFIG_PATH" sync-status "\$RENAME_NEW_PATH" >"\$EVIDENCE_DIR/honey-rename-new-status.out" 2>&1
    grep -q "sync state: synced" "\$EVIDENCE_DIR/honey-rename-new-status.out"
    if [[ -e "\$RENAME_OLD_STUB" ]]; then
      rename_old_stub_after_new_pull=present
    else
      rename_old_stub_after_new_pull=absent
    fi
    if [[ -e "\$RENAME_NEW_STUB" ]]; then
      rename_new_stub_after_pull=present
    else
      rename_new_stub_after_pull=absent
    fi
    {
      echo "rename_old_pull=failed_as_expected"
      echo "rename_old_pull_repeat=failed_as_expected"
      echo "rename_new_pull=synced"
      echo "rename_new_pull_repeat=synced"
      echo "rename_old_stub_after_new_pull=\$rename_old_stub_after_new_pull"
      echo "rename_new_stub_after_pull=\$rename_new_stub_after_pull"
      echo "rename_old_stub_path=\$RENAME_OLD_STUB"
      echo "rename_new_stub_path=\$RENAME_NEW_STUB"
    } >"\$EVIDENCE_DIR/honey-rename-peer-result.env"
    echo "honey peer-rename verify ok: \$RENAME_OLD_FILE -> \$RENAME_NEW_FILE"
    echo "rename_old_pull=failed_as_expected"
    echo "rename_old_pull_repeat=failed_as_expected"
    echo "rename_new_pull=synced"
    echo "rename_new_pull_repeat=synced"
    echo "rename_old_stub_after_new_pull=\$rename_old_stub_after_new_pull"
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

# Copy expected content and the honey M8 runner:
scp $(shell_quote "$delete_content_file") $(shell_quote "$honey_host"):$(shell_quote "$honey_remote_dir/delete-initial-content.txt")
scp $(shell_quote "$rename_content_file") $(shell_quote "$honey_host"):$(shell_quote "$honey_remote_dir/rename-content.txt")
scp $(shell_quote "$honey_script") $(shell_quote "$honey_host"):$(shell_quote "$honey_remote_dir/honey-delete-rename-run.sh")

# Pull and unsync on honey:
ssh $(shell_quote "$honey_host") 'TCFS_HONEY_DELETE_CONTENT_FILE=$(shell_quote "$honey_remote_dir/delete-initial-content.txt") TCFS_HONEY_RENAME_CONTENT_FILE=$(shell_quote "$honey_remote_dir/rename-content.txt") bash $(shell_quote "$honey_remote_dir/honey-delete-rename-run.sh") prepare-unsync'

# After neo deletes/renames, verify current peer-unsynced behavior:
ssh $(shell_quote "$honey_host") 'bash $(shell_quote "$honey_remote_dir/honey-delete-rename-run.sh") verify-delete'
ssh $(shell_quote "$honey_host") 'TCFS_HONEY_RENAME_CONTENT_FILE=$(shell_quote "$honey_remote_dir/rename-content.txt") bash $(shell_quote "$honey_remote_dir/honey-delete-rename-run.sh") verify-rename'
EOF

cat >"$evidence_dir/run-metadata.env" <<EOF
created_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
run_id=$run_id
remote=$remote
endpoint=$endpoint
bucket=$bucket
prefix=$prefix
neo_root=$neo_canon
delete_file=$delete_file
rename_old_file=$rename_old_file
rename_new_file=$rename_new_file
state_dir=$state_dir
honey_host=$honey_host
honey_root=$honey_root
honey_remote_dir=$honey_remote_dir
honey_tcfs_bin=$honey_tcfs_bin
push=$push_remote
run_honey=$run_honey
allow_real_roots=$allow_real_roots
EOF
cp "$config_path" "$evidence_dir/tcfs-delete-rename-unsynced.toml"

if [[ "$push_remote" == "1" ]]; then
  create_bucket_if_requested
  printf 'pushing initial neo fixtures to %s\n' "$remote"
  "${tcfs_cmd[@]}" --config "$config_path" push "$neo_canon" --prefix "$prefix" --state "$state_json" >"$initial_push_log" 2>&1
else
  printf 'plan-only: fixtures created but not pushed. Re-run with --push when ready.\n'
fi

if [[ "$run_honey" == "1" ]]; then
  [[ "$push_remote" == "1" ]] || fail "--run-honey requires --push"
  command -v ssh >/dev/null 2>&1 || fail "ssh not found"
  command -v scp >/dev/null 2>&1 || fail "scp not found"

  printf 'running honey prepare-unsync on %s\n' "$honey_host"
  # shellcheck disable=SC2029
  ssh "$honey_host" "mkdir -p $(shell_quote "$honey_remote_dir")"
  scp "$delete_content_file" "$honey_host:$honey_remote_dir/delete-initial-content.txt"
  scp "$rename_content_file" "$honey_host:$honey_remote_dir/rename-content.txt"
  scp "$honey_script" "$honey_host:$honey_remote_dir/honey-delete-rename-run.sh"

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

  prepare_cmd="$(printf 'TCFS_HONEY_DELETE_CONTENT_FILE=%q TCFS_HONEY_RENAME_CONTENT_FILE=%q bash %q prepare-unsync' \
    "$honey_remote_dir/delete-initial-content.txt" \
    "$honey_remote_dir/rename-content.txt" \
    "$honey_remote_dir/honey-delete-rename-run.sh")"
  verify_delete_cmd="$(printf 'bash %q verify-delete' "$honey_remote_dir/honey-delete-rename-run.sh")"
  verify_rename_cmd="$(printf 'TCFS_HONEY_RENAME_CONTENT_FILE=%q bash %q verify-rename' \
    "$honey_remote_dir/rename-content.txt" \
    "$honey_remote_dir/honey-delete-rename-run.sh")"
  if [[ -n "$remote_env_file" ]]; then
    prepare_cmd="$(printf 'TCFS_HONEY_ENV_FILE=%q %s' "$remote_env_file" "$prepare_cmd")"
    verify_delete_cmd="$(printf 'TCFS_HONEY_ENV_FILE=%q %s' "$remote_env_file" "$verify_delete_cmd")"
    verify_rename_cmd="$(printf 'TCFS_HONEY_ENV_FILE=%q %s' "$remote_env_file" "$verify_rename_cmd")"
  fi

  remote_status=0
  # shellcheck disable=SC2029
  ssh "$honey_host" "$prepare_cmd" | tee "$honey_prepare_log" || remote_status=$?
  if [[ "$remote_status" -ne 0 ]]; then
    cleanup_remote_env
    printf 'honey prepare-unsync failed; see %s\n' "$honey_prepare_log" >&2
    exit "$remote_status"
  fi

  printf 'deleting neo fixture\n'
  "${tcfs_cmd[@]}" --config "$config_path" rm "$delete_path" --prefix "$prefix" --state "$state_json" >"$delete_log" 2>&1

  printf 'renaming neo fixture and publishing new path\n'
  mkdir -p "$(dirname "$rename_new_path")"
  mv "$rename_old_path" "$rename_new_path"
  "${tcfs_cmd[@]}" --config "$config_path" rm "$rename_old_path" --prefix "$prefix" --state "$state_json" >"$rename_delete_old_log" 2>&1
  "${tcfs_cmd[@]}" --config "$config_path" push "$neo_canon" --prefix "$prefix" --state "$state_json" >"$rename_push_log" 2>&1
  find "$neo_canon" -maxdepth 8 -print | sort >"$local_tree_after"

  remote_status=0
  # shellcheck disable=SC2029
  ssh "$honey_host" "$verify_delete_cmd" | tee "$honey_delete_log" || remote_status=$?
  if [[ "$remote_status" -ne 0 ]]; then
    scp "$honey_host:$honey_remote_dir/honey-evidence/"* "$honey_evidence_dir/" >/dev/null 2>&1 || true
    cleanup_remote_env
    printf 'honey verify-delete failed; see %s\n' "$honey_delete_log" >&2
    exit "$remote_status"
  fi

  remote_status=0
  # shellcheck disable=SC2029
  ssh "$honey_host" "$verify_rename_cmd" | tee "$honey_rename_log" || remote_status=$?
  scp "$honey_host:$honey_remote_dir/honey-evidence/"* "$honey_evidence_dir/" >/dev/null 2>&1 || true
  cleanup_remote_env
  if [[ "$remote_status" -ne 0 ]]; then
    printf 'honey verify-rename failed; see %s\n' "$honey_rename_log" >&2
    exit "$remote_status"
  fi

  grep -q "delete_old_pull=failed_as_expected" "$honey_delete_log" || fail "delete verification missing failed old-path marker"
  grep -q "delete_old_pull_repeat=failed_as_expected" "$honey_delete_log" || fail "delete verification missing repeated failed old-path marker"
  grep -q "rename_old_pull=failed_as_expected" "$honey_rename_log" || fail "rename verification missing failed old-path marker"
  grep -q "rename_old_pull_repeat=failed_as_expected" "$honey_rename_log" || fail "rename verification missing repeated failed old-path marker"
  grep -q "rename_new_pull=synced" "$honey_rename_log" || fail "rename verification missing new-path hydration marker"
  grep -q "rename_new_pull_repeat=synced" "$honey_rename_log" || fail "rename verification missing repeated new-path hydration marker"

  cat >"$result_env" <<EOF
status=0
completed_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
proof=delete-rename-peer-unsynced-current-behavior
delete_old_pull=failed_as_expected
delete_old_pull_repeat=failed_as_expected
rename_old_pull=failed_as_expected
rename_old_pull_repeat=failed_as_expected
rename_new_pull=synced
rename_new_pull_repeat=synced
stale_old_stub_cleanup=not-implemented
EOF
else
  if [[ "$push_remote" == "1" ]]; then
    result_status="pushed"
    result_proof="pending-honey-delete-rename"
  else
    result_status="plan-only"
    result_proof="pending-push-or-honey-delete-rename"
  fi
  cat >"$result_env" <<EOF
status=$result_status
completed_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
proof=$result_proof
EOF
fi

cat >"$evidence_dir/README.md" <<EOF
# TCFS neo/honey Delete/Rename While Peer Unsynced Evidence

Created: $(date -u +%Y-%m-%dT%H:%M:%SZ)

This packet targets the M8 permutation:

1. neo creates and pushes \`$delete_file\` and \`$rename_old_file\`.
2. honey pulls both files into a physical sync root and runs \`tcfs unsync\`, so
   honey keeps only adjacent \`.tc\` stubs.
3. neo deletes \`$delete_file\` using \`tcfs rm\`.
4. neo renames \`$rename_old_file\` to \`$rename_new_file\` by deleting the old
   remote index entry, then publishing the new path.
5. honey verifies current behavior: old paths fail to rehydrate, the renamed new
   path hydrates exact bytes, and stale old stubs are recorded as an open product
   cleanup/tombstone gap.

Remote:

\`\`\`text
$remote
\`\`\`

Important files:

- \`neo-tree.txt\`: isolated neo fixture tree before delete/rename
- \`neo-tree-after-delete-rename.txt\`: neo fixture tree after peer operations
- \`tcfs-delete-rename-unsynced.toml\`: disposable neo config copied from the state dir
- \`honey-delete-rename-commands.txt\`: manual honey commands
- \`honey-prepare-unsync.log\`: honey pull/unsync transcript, when run
- \`neo-delete.log\`: neo \`tcfs rm\` delete transcript, when run
- \`neo-rename-push.log\`: neo new-path publish transcript, when run
- \`neo-rename-delete-old.log\`: neo old-path remote delete transcript, when run
- \`honey-verify-delete.log\`: old-path pull failure, repeated old-path pull
  failure, stale delete-stub status, and stale delete-stub \`sync-status\`
- \`honey-verify-rename.log\`: old-path pull failure, repeated old-path pull
  failure, new-path hydrate, repeated new-path hydrate, stale old-stub status,
  and old/new path \`sync-status\`
- \`honey-evidence/\`: detailed remote transcripts, copied back when available
- \`result.env\`: plan/current-behavior status

This helper uses an isolated neo root under \`$neo_canon\` and a honey root
\`$honey_root\`; it does not target real \`~/Documents\`, \`~/git\`, dotfiles,
or broad home-directory paths unless \`--allow-real-roots\` is explicitly
supplied.

Claimability note: this packet does not by itself make a user-facing
"renames/deletes clean stale peer placeholders" claim. TCFS currently lacks a
durable tombstone/stale-stub cleanup protocol for physical unsynced roots, so
that stronger claim remains open until product semantics and QA assertions are
accepted.
EOF

printf 'delete/rename unsynced evidence: %s\n' "$evidence_dir"
printf 'neo root: %s\n' "$neo_canon"
printf 'remote: %s\n' "$remote"
printf 'honey delete/rename commands: %s\n' "$honey_commands"
