#!/usr/bin/env bash
#
# Prepare a safe Desktop-originated TCFS lazy traversal demo for honey.
#
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/lazy-hydration-desktop-honey-demo.sh [options]

Create an isolated Desktop fixture, optionally push it to a disposable remote
prefix, emit the honey commands needed to mount that prefix, and optionally run
the remote mounted lazy-hydration smoke over SSH.

By default this helper does not push remote data. Add --push once the remote
endpoint, bucket, and credentials are ready.

Options:
  --remote <seaweedfs://host:port/bucket/prefix>
      Remote prefix to use. Defaults to a timestamped desktop-demo prefix.
  --desktop-root <path>
      Local fixture root. Defaults to "$HOME/Desktop/TCFS Demo".
  --push
      Run tcfs push for the fixture root.
  --tcfs-bin <path>
      tcfs binary to run. Defaults to TCFS_BIN, target/debug/tcfs, or
      cargo run -p tcfs-cli --.
  --create-bucket
      Best-effort bucket creation with aws cli, s5cmd, or mc before pushing.
  --honey-host <host>
      SSH host label to use in emitted commands. Default: honey.
  --honey-mount-root <path>
      Honey mountpoint path for the remote prefix. Default: ~/tcfs-demo/Desktop.
  --honey-remote-dir <path>
      Remote temp directory for copied smoke scripts. Default: /tmp/tcfs-desktop-honey-<run-id>.
  --honey-tcfs-bin <path>
      Remote tcfs binary path to use on honey. Default: tcfs from honey PATH.
  --run-honey
      Copy scripts to honey and run the remote smoke. Honey must already have
      tcfs, mount permissions, and S3 credentials available.
  --honey-start-mount
      With --run-honey, start `tcfs mount` on honey before running smoke.
  --honey-existing-mount
      With --run-honey, assume --honey-mount-root is already mounted.
  --forward-aws-env
      With --run-honey, copy current AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY to
      a temporary 0600 env file on honey for this smoke run. Off by default.
  --evidence-dir <path>
      Write config, expected content, local tree, honey commands, and logs.
  --allow-real-desktop
      Allow --desktop-root to be exactly "$HOME/Desktop". Not recommended.
  --allow-honey-real-desktop
      Allow --honey-mount-root to target honey's real Desktop. Not recommended.
  -h, --help
      Show this help.

Environment:
  TCFS_DESKTOP_DEMO_REMOTE         Same as --remote
  TCFS_DESKTOP_DEMO_ROOT           Same as --desktop-root
  TCFS_DESKTOP_DEMO_PUSH=1         Same as --push
  TCFS_DESKTOP_DEMO_CREATE_BUCKET=1
                                    Same as --create-bucket
  TCFS_DESKTOP_DEMO_EVIDENCE_DIR   Same as --evidence-dir
  TCFS_BIN                         Same as --tcfs-bin
  TCFS_HONEY_HOST                  Same as --honey-host
  TCFS_HONEY_MOUNT_ROOT            Same as --honey-mount-root
  TCFS_HONEY_REMOTE_DIR            Same as --honey-remote-dir
  TCFS_HONEY_TCFS_BIN              Same as --honey-tcfs-bin
  TCFS_DESKTOP_DEMO_RUN_HONEY=1    Same as --run-honey
  TCFS_HONEY_START_MOUNT=1         Same as --honey-start-mount
  TCFS_HONEY_FORWARD_AWS_ENV=1      Same as --forward-aws-env
  TCFS_HONEY_ALLOW_REAL_DESKTOP=1   Same as --allow-honey-real-desktop
  AWS_ACCESS_KEY_ID                S3 access key; defaults to admin only for localhost
  AWS_SECRET_ACCESS_KEY            S3 secret key; defaults to admin only for localhost
  TCFS_S3_REGION                   S3 region, default us-east-1
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

physical_dir() {
  local path="$1"
  mkdir -p "$path"
  (cd "$path" && pwd -P)
}

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
timestamp="$(date -u +%Y%m%dT%H%M%SZ)"
remote="${TCFS_DESKTOP_DEMO_REMOTE:-seaweedfs://localhost:8333/tcfs/desktop-demo-${USER:-user}-${timestamp}-$$}"
desktop_root="${TCFS_DESKTOP_DEMO_ROOT:-$HOME/Desktop/TCFS Demo}"
push_remote="$(bool_env TCFS_DESKTOP_DEMO_PUSH "${TCFS_DESKTOP_DEMO_PUSH:-0}")"
create_bucket="$(bool_env TCFS_DESKTOP_DEMO_CREATE_BUCKET "${TCFS_DESKTOP_DEMO_CREATE_BUCKET:-0}")"
evidence_dir="${TCFS_DESKTOP_DEMO_EVIDENCE_DIR:-}"
tcfs_bin="${TCFS_BIN:-}"
honey_host="${TCFS_HONEY_HOST:-honey}"
honey_mount_root="${TCFS_HONEY_MOUNT_ROOT:-~/tcfs-demo/Desktop}"
honey_remote_dir="${TCFS_HONEY_REMOTE_DIR:-/tmp/tcfs-desktop-honey-${timestamp}-$$}"
honey_tcfs_bin="${TCFS_HONEY_TCFS_BIN:-tcfs}"
run_honey="$(bool_env TCFS_DESKTOP_DEMO_RUN_HONEY "${TCFS_DESKTOP_DEMO_RUN_HONEY:-0}")"
honey_start_mount="$(bool_env TCFS_HONEY_START_MOUNT "${TCFS_HONEY_START_MOUNT:-0}")"
forward_aws_env="$(bool_env TCFS_HONEY_FORWARD_AWS_ENV "${TCFS_HONEY_FORWARD_AWS_ENV:-0}")"
allow_honey_real_desktop="$(bool_env TCFS_HONEY_ALLOW_REAL_DESKTOP "${TCFS_HONEY_ALLOW_REAL_DESKTOP:-0}")"
allow_real_desktop=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --remote)
      [[ $# -ge 2 ]] || fail "--remote requires a value"
      remote="$2"
      shift 2
      ;;
    --desktop-root)
      [[ $# -ge 2 ]] || fail "--desktop-root requires a value"
      desktop_root="$2"
      shift 2
      ;;
    --push)
      push_remote=1
      shift
      ;;
    --tcfs-bin)
      [[ $# -ge 2 ]] || fail "--tcfs-bin requires a value"
      tcfs_bin="$2"
      shift 2
      ;;
    --create-bucket)
      create_bucket=1
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
    --evidence-dir)
      [[ $# -ge 2 ]] || fail "--evidence-dir requires a value"
      evidence_dir="$2"
      shift 2
      ;;
    --allow-real-desktop)
      allow_real_desktop=1
      shift
      ;;
    --allow-honey-real-desktop)
      allow_honey_real_desktop=1
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
case "$honey_remote_dir" in
  *[[:space:]]*) fail "--honey-remote-dir must not contain whitespace: $honey_remote_dir" ;;
esac
if ! [[ "$honey_remote_dir" =~ ^[A-Za-z0-9_./@%+=:-]+$ ]]; then
  fail "--honey-remote-dir contains unsafe shell characters: $honey_remote_dir"
fi
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

desktop_canon="$(physical_dir "$desktop_root")"
home_desktop_canon="$(physical_dir "$HOME/Desktop")"
home_canon="$(physical_dir "$HOME")"

[[ "$desktop_canon" != "/" ]] || fail "refusing to use filesystem root as desktop demo root"
[[ "$desktop_canon" != "$home_canon" ]] || fail "refusing to use HOME as desktop demo root"
if [[ "$desktop_canon" == "$home_desktop_canon" && "$allow_real_desktop" != "1" ]]; then
  fail "refusing to use real Desktop as demo root; use '$HOME/Desktop/TCFS Demo' or pass --allow-real-desktop"
fi

honey_mount_root_trimmed="${honey_mount_root%/}"
tilde_desktop="~"/Desktop
dollar_home_desktop="\$HOME/Desktop"
braced_home_desktop="\${HOME}/Desktop"
case "$honey_mount_root_trimmed" in
  "$tilde_desktop"|"$dollar_home_desktop"|"$braced_home_desktop"|/home/*/Desktop|/Users/*/Desktop)
    if [[ "$allow_honey_real_desktop" != "1" ]]; then
      fail "refusing to use honey real Desktop as mount root; use '~/tcfs-demo/Desktop' or pass --allow-honey-real-desktop"
    fi
    ;;
esac

if [[ -z "$evidence_dir" ]]; then
  evidence_dir="${TMPDIR:-/tmp}/tcfs-desktop-honey-demo-${timestamp}-$$"
fi
mkdir -p "$evidence_dir"

state_dir="${TCFS_DESKTOP_DEMO_STATE_DIR:-$HOME/.local/state/tcfs/desktop-demo/${timestamp}-$$}"
mkdir -p "$state_dir"
state_json="$state_dir/state.json"
cache_root="$state_dir/cache"
config_path="$state_dir/tcfs.toml"
expected_file="Projects/tcfs-odrive-parity/honey-readme.txt"
expected_content_file="$evidence_dir/expected-content.txt"
honey_script="$evidence_dir/honey-run.sh"
honey_commands="$evidence_dir/honey-commands.txt"
honey_run_log="$evidence_dir/honey-run.log"
honey_mount_log="$evidence_dir/honey-mount.log"
local_tree="$evidence_dir/local-tree.txt"
push_log="$evidence_dir/push.log"
mc_config_dir="$state_dir/mc"

tcfs_cmd=()
if [[ -n "$tcfs_bin" ]]; then
  [[ -x "$tcfs_bin" ]] || fail "--tcfs-bin is not executable: $tcfs_bin"
  tcfs_cmd=("$tcfs_bin")
elif [[ -x "$REPO_ROOT/target/debug/tcfs" ]]; then
  tcfs_cmd=("$REPO_ROOT/target/debug/tcfs")
else
  tcfs_cmd=(cargo run --quiet -p tcfs-cli --)
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
    mc --config-dir "$mc_config_dir" alias set tcfs-desktop-demo "$endpoint" "$AWS_ACCESS_KEY_ID" "$AWS_SECRET_ACCESS_KEY" >/dev/null
    mc --config-dir "$mc_config_dir" mb --ignore-existing "tcfs-desktop-demo/$bucket" >/dev/null
    return 0
  fi

  fail "--create-bucket requested, but none of aws, s5cmd, or mc was found"
}

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

mkdir -p \
  "$desktop_canon/Projects/tcfs-odrive-parity/Notes" \
  "$desktop_canon/Photos/2026/april" \
  "$desktop_canon/Notes"

cat >"$desktop_canon/$expected_file" <<'EOF'
TCFS Desktop honey fixture
This file starts in an isolated Desktop demo folder and should hydrate lazily on honey.
EOF
cat >"$desktop_canon/Projects/tcfs-odrive-parity/Notes/product-goal.md" <<'EOF'
# Product Goal

Show a desktop-originated tree on another host as a clean lazy filesystem.
EOF
cat >"$desktop_canon/Photos/2026/april/manifest.txt" <<'EOF'
placeholder photo manifest for traversal proof
EOF
cat >"$desktop_canon/Notes/unsync-checklist.txt" <<'EOF'
ls before hydration
cat to hydrate
clear or unsync cache
cat again
EOF

cp "$desktop_canon/$expected_file" "$expected_content_file"
find "$desktop_canon" -maxdepth 6 -print | sort >"$local_tree"

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
sync_root = "$desktop_canon"
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

cat >"$evidence_dir/run-metadata.env" <<EOF
created_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
remote=$remote
endpoint=$endpoint
bucket=$bucket
prefix=$prefix
desktop_root=$desktop_canon
expected_file=$expected_file
honey_host=$honey_host
honey_mount_root=$honey_mount_root
honey_remote_dir=$honey_remote_dir
honey_tcfs_bin=$honey_tcfs_bin
push=$push_remote
run_honey=$run_honey
honey_start_mount=$honey_start_mount
forward_aws_env=$forward_aws_env
allow_honey_real_desktop=$allow_honey_real_desktop
state_dir=$state_dir
EOF

cp "$config_path" "$evidence_dir/tcfs.toml"

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
EXPECTED_FILE=$(shell_quote "$expected_file")
EXPECTED_CONTENT_FILE="\${TCFS_HONEY_EXPECTED_CONTENT_FILE:-/tmp/tcfs-desktop-honey-expected.txt}"
SMOKE_SCRIPT="\${TCFS_HONEY_SMOKE_SCRIPT:-/tmp/lazy-hydration-mounted-smoke.sh}"
MOUNT_LOG="\${TCFS_HONEY_MOUNT_LOG:-/tmp/tcfs-desktop-honey-mount.log}"

if [[ -n "\${TCFS_HONEY_ENV_FILE:-}" ]]; then
  # shellcheck disable=SC1090
  source "\$TCFS_HONEY_ENV_FILE"
fi

mkdir -p "\$MOUNT_ROOT" "\$(dirname "\$EXPECTED_CONTENT_FILE")"
cat >"\$EXPECTED_CONTENT_FILE" <<'EXPECTED_CONTENT_EOF'
$(cat "$expected_content_file")
EXPECTED_CONTENT_EOF

if [[ "\${TCFS_HONEY_START_MOUNT:-0}" == "1" ]]; then
  nohup "\$TCFS_BIN" mount "\$REMOTE" "\$MOUNT_ROOT" >"\$MOUNT_LOG" 2>&1 &
  mount_pid="\$!"
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

if [[ ! -x "\$SMOKE_SCRIPT" && ! -f "\$SMOKE_SCRIPT" ]]; then
  echo "missing mounted smoke helper: \$SMOKE_SCRIPT" >&2
  echo "copy scripts/lazy-hydration-mounted-smoke.sh there or set TCFS_HONEY_SMOKE_SCRIPT" >&2
  exit 1
fi

bash "\$SMOKE_SCRIPT" \\
  --mount-root "\$MOUNT_ROOT" \\
  --expected-file "\$EXPECTED_FILE" \\
  --expected-content-file "\$EXPECTED_CONTENT_FILE" \\
  --expect-entry Projects \\
  --expect-entry Projects/tcfs-odrive-parity \\
  --max-depth 6
EOF
chmod +x "$honey_script"

cat >"$honey_commands" <<EOF
# Prepare remote work directory:
ssh $(shell_quote "$honey_host") 'mkdir -p $(shell_quote "$honey_remote_dir")'

# Copy the smoke helper and honey run script:
scp $(shell_quote "$REPO_ROOT/scripts/lazy-hydration-mounted-smoke.sh") $(shell_quote "$honey_host"):$(shell_quote "$honey_remote_dir/lazy-hydration-mounted-smoke.sh")
scp $(shell_quote "$honey_script") $(shell_quote "$honey_host"):$(shell_quote "$honey_remote_dir/honey-run.sh")

# On honey, ensure tcfs can reach the same remote and has S3 credentials.
# To start the mount from the script, set TCFS_HONEY_START_MOUNT=1:
ssh $(shell_quote "$honey_host") 'TCFS_HONEY_START_MOUNT=1 TCFS_HONEY_SMOKE_SCRIPT=$(shell_quote "$honey_remote_dir/lazy-hydration-mounted-smoke.sh") TCFS_HONEY_EXPECTED_CONTENT_FILE=$(shell_quote "$honey_remote_dir/expected-content.txt") TCFS_HONEY_MOUNT_LOG=$(shell_quote "$honey_remote_dir/mount.log") bash $(shell_quote "$honey_remote_dir/honey-run.sh")'

# If the mount is already active on honey, omit TCFS_HONEY_START_MOUNT:
ssh $(shell_quote "$honey_host") 'TCFS_HONEY_SMOKE_SCRIPT=$(shell_quote "$honey_remote_dir/lazy-hydration-mounted-smoke.sh") TCFS_HONEY_EXPECTED_CONTENT_FILE=$(shell_quote "$honey_remote_dir/expected-content.txt") TCFS_HONEY_MOUNT_LOG=$(shell_quote "$honey_remote_dir/mount.log") bash $(shell_quote "$honey_remote_dir/honey-run.sh")'

# If honey does not already have AWS credentials, rerun this local helper with
# --run-honey --forward-aws-env. The generated command file never stores those
# credentials.
EOF

if [[ "$push_remote" == "1" ]]; then
  create_bucket_if_requested
  printf 'pushing desktop fixture to %s\n' "$remote"
  "${tcfs_cmd[@]}" --config "$config_path" push "$desktop_canon" --prefix "$prefix" --state "$state_json" | tee "$push_log"
else
  printf 'plan-only: fixture created but not pushed. Re-run with --push when ready.\n'
fi

if [[ "$run_honey" == "1" ]]; then
  command -v ssh >/dev/null 2>&1 || fail "ssh not found"
  command -v scp >/dev/null 2>&1 || fail "scp not found"

  printf 'running honey smoke on %s\n' "$honey_host"
  if [[ "$forward_aws_env" == "1" && "$honey_start_mount" == "1" ]]; then
    printf 'warning: forwarded AWS credentials are inherited by the honey mount process; unmount after the smoke unless honey has its own credential source\n' >&2
  fi
  # shellcheck disable=SC2029
  ssh "$honey_host" "mkdir -p $(shell_quote "$honey_remote_dir")"
  scp "$REPO_ROOT/scripts/lazy-hydration-mounted-smoke.sh" "$honey_host:$honey_remote_dir/lazy-hydration-mounted-smoke.sh"
  scp "$honey_script" "$honey_host:$honey_remote_dir/honey-run.sh"

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

  remote_run_cmd="$(printf 'TCFS_HONEY_START_MOUNT=%q TCFS_HONEY_SMOKE_SCRIPT=%q TCFS_HONEY_EXPECTED_CONTENT_FILE=%q TCFS_HONEY_MOUNT_LOG=%q bash %q' \
    "$honey_start_mount" \
    "$honey_remote_dir/lazy-hydration-mounted-smoke.sh" \
    "$honey_remote_dir/expected-content.txt" \
    "$honey_remote_dir/mount.log" \
    "$honey_remote_dir/honey-run.sh")"
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
    exit "$remote_status"
  fi
fi

printf 'desktop fixture root: %s\n' "$desktop_canon"
printf 'expected file: %s\n' "$expected_file"
printf 'remote prefix: %s\n' "$remote"
printf 'evidence dir: %s\n' "$evidence_dir"
printf 'honey commands: %s\n' "$honey_commands"
