#!/usr/bin/env bash
#
# End-to-end Linux terminal demo for mounted TCFS lazy traversal and hydration.
#
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/lazy-hydration-linux-demo.sh [options]

Seed a dedicated remote prefix, mount it through tcfs on Linux, prove clean
ls/find names before hydration, cat a remote-backed file, verify cache hydration,
clear the mount cache, and cat again to prove rehydration.

Options:
  --remote <seaweedfs://host:port/bucket/prefix>
      Remote prefix to use. Defaults to a timestamped prefix under
      seaweedfs://localhost:8333/tcfs/.
  --tcfs-bin <path>
      tcfs binary to run. Defaults to TCFS_BIN, target/debug/tcfs, or
      cargo run -p tcfs-cli --.
  --nfs
      Use tcfs mount --nfs instead of the default FUSE backend.
  --create-bucket
      Best-effort bucket creation with aws cli, s5cmd, or mc before seeding.
  --evidence-dir <path>
      Write transcript, redacted metadata, result, and mount log evidence.
  --keep
      Keep the temp source/config/mount/cache directory after the run.
  -h, --help
      Show this help.

Environment:
  TCFS_LAZY_DEMO_REMOTE      Same as --remote
  TCFS_BIN                   Same as --tcfs-bin
  TCFS_LAZY_DEMO_BACKEND     fuse or nfs, default fuse
  TCFS_LAZY_DEMO_CREATE_BUCKET=1
                             Same as --create-bucket
  TCFS_LAZY_DEMO_EVIDENCE_DIR
                             Same as --evidence-dir
  TCFS_LAZY_DEMO_KEEP=1      Same as --keep
  AWS_ACCESS_KEY_ID          S3 access key; defaults to admin only for localhost
  AWS_SECRET_ACCESS_KEY      S3 secret key; defaults to admin only for localhost
  TCFS_S3_REGION             S3 region, default us-east-1

This harness requires Linux, /dev/fuse plus fusermount3 for FUSE mode, and a
pre-existing bucket unless --create-bucket succeeds.
EOF
}

fail() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
timestamp="$(date -u +%Y%m%dT%H%M%SZ)"
remote="${TCFS_LAZY_DEMO_REMOTE:-seaweedfs://localhost:8333/tcfs/lazy-demo-${USER:-user}-${timestamp}-$$}"
tcfs_bin="${TCFS_BIN:-}"
backend="${TCFS_LAZY_DEMO_BACKEND:-fuse}"
create_bucket="${TCFS_LAZY_DEMO_CREATE_BUCKET:-0}"
evidence_dir="${TCFS_LAZY_DEMO_EVIDENCE_DIR:-}"
keep="${TCFS_LAZY_DEMO_KEEP:-0}"
transcript_path=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --remote)
      [[ $# -ge 2 ]] || fail "--remote requires a value"
      remote="$2"
      shift 2
      ;;
    --tcfs-bin)
      [[ $# -ge 2 ]] || fail "--tcfs-bin requires a value"
      tcfs_bin="$2"
      shift 2
      ;;
    --nfs)
      backend="nfs"
      shift
      ;;
    --create-bucket)
      create_bucket=1
      shift
      ;;
    --evidence-dir)
      [[ $# -ge 2 ]] || fail "--evidence-dir requires a value"
      evidence_dir="$2"
      shift 2
      ;;
    --keep)
      keep=1
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

case "$backend" in
  fuse|nfs) ;;
  *) fail "TCFS_LAZY_DEMO_BACKEND must be fuse or nfs, got: $backend" ;;
esac

case "$create_bucket" in
  1|true|yes|on) create_bucket=1 ;;
  0|false|no|off|"") create_bucket=0 ;;
  *) fail "TCFS_LAZY_DEMO_CREATE_BUCKET must be 0/1, got: $create_bucket" ;;
esac

if [[ "$(uname -s)" != "Linux" ]]; then
  fail "Linux-only harness; run mounted-view smoke on macOS with scripts/lazy-hydration-mounted-smoke.sh against an existing mount"
fi

if [[ "$backend" == "fuse" ]]; then
  [[ -e /dev/fuse ]] || fail "missing /dev/fuse"
  command -v fusermount3 >/dev/null 2>&1 || fail "fusermount3 not found"
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
[[ -n "$prefix" ]] || fail "remote must include a dedicated non-root prefix for this demo: $remote"
endpoint="http://${remote_host}"
region="${TCFS_S3_REGION:-us-east-1}"

tmp_root="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-lazy-linux-demo.XXXXXX")"
source_root="$tmp_root/source"
mount_root="$tmp_root/mount"
cache_root="$tmp_root/cache"
state_json="$tmp_root/state.json"
config_path="$tmp_root/tcfs.toml"
mount_log="$tmp_root/mount.log"
mc_config_dir="$tmp_root/mc"
expected_file="docs/deep/remote.txt"
expected_content=$'TCFS lazy hydration fixture\nThis file should hydrate only when cat opens it.\n'
mount_pid=""

tcfs_cmd=()
if [[ -n "$tcfs_bin" ]]; then
  [[ -x "$tcfs_bin" ]] || fail "--tcfs-bin is not executable: $tcfs_bin"
  tcfs_cmd=("$tcfs_bin")
elif [[ -x "$REPO_ROOT/target/debug/tcfs" ]]; then
  tcfs_cmd=("$REPO_ROOT/target/debug/tcfs")
else
  tcfs_cmd=(cargo run --quiet -p tcfs-cli --)
fi

setup_evidence() {
  [[ -n "$evidence_dir" ]] || return 0

  mkdir -p "$evidence_dir"
  transcript_path="$evidence_dir/transcript.log"

  {
    printf 'started_at_utc=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf 'remote=%s\n' "$remote"
    printf 'endpoint=%s\n' "$endpoint"
    printf 'bucket=%s\n' "$bucket"
    printf 'prefix=%s\n' "$prefix"
    printf 'backend=%s\n' "$backend"
    printf 'region=%s\n' "$region"
    printf 'create_bucket=%s\n' "$create_bucket"
    printf 'tcfs_command=%q' "${tcfs_cmd[0]}"
    local arg
    for arg in "${tcfs_cmd[@]:1}"; do
      printf ' %q' "$arg"
    done
    printf '\n'
  } >"$evidence_dir/run-metadata.env"

  exec > >(tee "$transcript_path") 2>&1
  printf 'evidence dir: %s\n' "$evidence_dir"
}

setup_evidence

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

is_mounted() {
  [[ -r /proc/self/mountinfo ]] || return 1
  awk -v mp="$mount_root" '$5 == mp { found = 1 } END { exit(found ? 0 : 1) }' /proc/self/mountinfo
}

cache_entry_count() {
  if [[ ! -d "$cache_root" ]]; then
    printf '0\n'
    return
  fi
  find "$cache_root" -mindepth 2 -maxdepth 2 -type f ! -name '*.tmp' | wc -l | tr -d ' '
}

cache_entry_bytes() {
  if [[ ! -d "$cache_root" ]]; then
    printf '0\n'
    return
  fi
  find "$cache_root" -mindepth 2 -maxdepth 2 -type f ! -name '*.tmp' -printf '%s\n' \
    | awk '{ total += $1 } END { print total + 0 }'
}

show_mount_log() {
  if [[ -s "$mount_log" ]]; then
    printf '\n--- mount log ---\n' >&2
    tail -n 80 "$mount_log" >&2
  fi
}

short_pause() {
  if command -v perl >/dev/null 2>&1; then
    perl -e 'select undef, undef, undef, 0.1'
  else
    python3 -c 'import select; select.select([], [], [], 0.1)'
  fi
}

cleanup() {
  local status=$?

  if [[ -n "$mount_pid" ]] && kill -0 "$mount_pid" 2>/dev/null; then
    if is_mounted; then
      "${tcfs_cmd[@]}" --config "$config_path" unmount "$mount_root" >/dev/null 2>&1 \
        || fusermount3 -u "$mount_root" >/dev/null 2>&1 \
        || true
    fi

    for _ in {1..50}; do
      if ! kill -0 "$mount_pid" 2>/dev/null; then
        break
      fi
      short_pause
    done

    if kill -0 "$mount_pid" 2>/dev/null; then
      kill "$mount_pid" 2>/dev/null || true
    fi
    wait "$mount_pid" 2>/dev/null || true
  fi

  if [[ -n "$evidence_dir" ]]; then
    mkdir -p "$evidence_dir"
    {
      printf 'completed_at_utc=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
      printf 'status=%s\n' "$status"
    } >"$evidence_dir/result.env"
    [[ -f "$mount_log" ]] && cp "$mount_log" "$evidence_dir/mount.log"
    [[ -f "$config_path" ]] && cp "$config_path" "$evidence_dir/tcfs.toml"
  fi

  if [[ "$keep" == "1" ]]; then
    printf 'kept demo temp root: %s\n' "$tmp_root"
  else
    rm -rf "$tmp_root"
  fi

  exit "$status"
}
trap cleanup EXIT

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
    mc --config-dir "$mc_config_dir" alias set tcfs-lazy-demo "$endpoint" "$AWS_ACCESS_KEY_ID" "$AWS_SECRET_ACCESS_KEY" >/dev/null
    mc --config-dir "$mc_config_dir" mb --ignore-existing "tcfs-lazy-demo/$bucket" >/dev/null
    return 0
  fi

  fail "--create-bucket requested, but none of aws, s5cmd, or mc was found"
}

wait_for_mount() {
  for _ in {1..300}; do
    if is_mounted; then
      return 0
    fi
    if ! kill -0 "$mount_pid" 2>/dev/null; then
      show_mount_log
      fail "tcfs mount exited before $mount_root became mounted"
    fi
    short_pause
  done

  show_mount_log
  fail "mountpoint did not become active: $mount_root"
}

clear_cache_entries() {
  if [[ -d "$cache_root" ]]; then
    find "$cache_root" -mindepth 1 -maxdepth 1 -exec rm -rf {} +
  fi
}

mkdir -p "$source_root/docs/deep" "$source_root/docs/empty-dir" "$mount_root" "$cache_root"
printf '%s' "$expected_content" >"$source_root/$expected_file"
printf 'visible before hydration\n' >"$source_root/docs/README.md"

cat >"$config_path" <<EOF
[daemon]
socket = "$tmp_root/no-daemon.sock"

[storage]
endpoint = "$endpoint"
region = "$region"
bucket = "$bucket"
remote_prefix = "$prefix"
enforce_tls = false

[sync]
state_db = "$tmp_root/state.db"
sync_root = "$source_root"
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

printf 'remote: %s\n' "$remote"
printf 'temp root: %s\n' "$tmp_root"
printf 'tcfs command: %s\n' "${tcfs_cmd[*]}"

create_bucket_if_requested

printf '\n[1/6] seed fixture into remote prefix\n'
"${tcfs_cmd[@]}" --config "$config_path" push "$source_root" --prefix "$prefix" --state "$state_json"

before_count="$(cache_entry_count)"
[[ "$before_count" == "0" ]] || fail "cache should be empty before mount traversal, got $before_count entries"

printf '\n[2/6] start direct %s mount\n' "$backend"
mount_args=(--config "$config_path" mount "$remote" "$mount_root")
if [[ "$backend" == "nfs" ]]; then
  mount_args+=(--nfs)
fi
"${tcfs_cmd[@]}" "${mount_args[@]}" >"$mount_log" 2>&1 &
mount_pid=$!
wait_for_mount
printf 'mounted at: %s\n' "$mount_root"

printf '\n[3/6] prove traversal does not hydrate content\n'
find "$mount_root" -maxdepth 4 -print | sort
after_ls_count="$(cache_entry_count)"
[[ "$after_ls_count" == "0" ]] || fail "ls/find hydrated cache unexpectedly; entries=$after_ls_count"

printf '\n[4/6] cat remote-backed file and verify hydration\n'
"$REPO_ROOT/scripts/lazy-hydration-mounted-smoke.sh" \
  --mount-root "$mount_root" \
  --expected-file "$expected_file" \
  --expected-content "$expected_content" \
  --expect-entry docs \
  --expect-entry docs/deep \
  --max-depth 6

after_cat_count="$(cache_entry_count)"
after_cat_bytes="$(cache_entry_bytes)"
[[ "$after_cat_count" -gt 0 ]] || fail "cat succeeded but cache remained empty"
printf 'cache after first cat: entries=%s bytes=%s\n' "$after_cat_count" "$after_cat_bytes"

printf '\n[5/6] clear mount cache as mounted-surface dehydration\n'
clear_cache_entries
after_clear_count="$(cache_entry_count)"
[[ "$after_clear_count" == "0" ]] || fail "cache clear left $after_clear_count entries"
printf 'cache after clear: entries=%s\n' "$after_clear_count"

printf '\n[6/6] cat again and verify rehydration\n'
rehydrated_output="$tmp_root/rehydrated-output.txt"
cat "$mount_root/$expected_file" >"$rehydrated_output"
cmp -s "$source_root/$expected_file" "$rehydrated_output" || fail "rehydrated cat output mismatch"
after_recat_count="$(cache_entry_count)"
after_recat_bytes="$(cache_entry_bytes)"
[[ "$after_recat_count" -gt 0 ]] || fail "re-cat succeeded but cache remained empty"
printf 'cache after re-cat: entries=%s bytes=%s\n' "$after_recat_count" "$after_recat_bytes"

printf '\nlazy hydration Linux demo passed\n'
if [[ -n "$evidence_dir" ]]; then
  printf 'evidence written to: %s\n' "$evidence_dir"
fi
