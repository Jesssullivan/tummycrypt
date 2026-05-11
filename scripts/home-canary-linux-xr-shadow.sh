#!/usr/bin/env bash
#
# Real project-tree canary for a full isolated shadow of /Users/jess/git/linux-xr.
#
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/home-canary-linux-xr-shadow.sh [options]

Inventory a live project tree read-only, copy it to an isolated shadow under
~/TCFS Pilot, write a TCFS config/state rooted at that shadow, and optionally
push/run remote proof companions against a disposable prefix.

Options:
  --source <path>        Source project. Default: /Users/jess/git/linux-xr
  --shadow-root <path>   Shadow copy path. Default: ~/TCFS Pilot/real-canaries/linux-xr-shadow-<UTC>
  --evidence-dir <path>  Evidence dir. Default: docs/release/evidence/home-canary-linux-xr-shadow-<UTC>
  --remote <url>         seaweedfs://host:port/bucket/prefix disposable remote
  --state-dir <path>     Local TCFS state/config dir. Default: <evidence-dir>/state
  --tcfs-bin <path>      tcfs binary. Default: TCFS_BIN, target/debug/tcfs, or cargo run
  --push                 Push the shadow to the disposable prefix
  --resume-after-push    Reuse an existing completed push.log/state and run
                          post-push companions without rerunning push
  --reuse-shadow         Do not recopy source into shadow; inventory the
                          existing shadow as the pushed snapshot
  --create-bucket        Best-effort bucket creation before push/lifecycle
  --run-honey            Emit/copy/run honey mounted traversal smoke
  --run-linux-lifecycle  Run Linux lifecycle companion on honey under <remote>/linux-lifecycle
  --honey-host <host>    SSH host label. Default: honey
  --honey-mount-root <path>
                          Honey mountpoint. Default: /tmp/tcfs-linux-xr-shadow-<UTC>/mount
  --honey-remote-dir <path>
                          Honey work dir. Default: /tmp/tcfs-linux-xr-shadow-<UTC>
  --honey-tcfs-bin <path>
                          tcfs binary on honey. Default: tcfs
  --honey-start-mount    With --run-honey, start tcfs mount on honey
  --honey-existing-mount With --run-honey, assume --honey-mount-root is already mounted
  --honey-smoke-max-depth <n>
                          Mounted traversal depth. Default: 8
  --honey-smoke-timeout-secs <n>
                          Bound mounted smoke when timeout(1) exists. Default: 900
  --forward-aws-env      Forward AWS env to honey for mount/lifecycle companions
  --keep-shadow          Do not print cleanup hint for the shadow
  -h, --help             Show this help

Environment mirrors:
  TCFS_HOME_CANARY_SOURCE
  TCFS_HOME_CANARY_SHADOW_ROOT
  TCFS_HOME_CANARY_EVIDENCE_DIR
  TCFS_HOME_CANARY_REMOTE
  TCFS_HOME_CANARY_STATE_DIR
  TCFS_HOME_CANARY_PUSH=1
  TCFS_HOME_CANARY_RESUME_AFTER_PUSH=1
  TCFS_HOME_CANARY_REUSE_SHADOW=1
  TCFS_HOME_CANARY_CREATE_BUCKET=1
  TCFS_HOME_CANARY_RUN_HONEY=1
  TCFS_HOME_CANARY_RUN_LINUX_LIFECYCLE=1
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

canonical_existing_path() {
  local path="$1"
  [[ -e "$path" ]] || fail "path does not exist: $path"
  (cd "$path" && pwd -P)
}

make_physical_dir() {
  local path="$1"
  mkdir -p "$path"
  (cd "$path" && pwd -P)
}

write_count() {
  local label="$1"
  local count="$2"
  printf '%s=%s\n' "$label" "$count"
}

write_symlink_targets() {
  local root="$1"
  local out="$2"

  {
    while IFS= read -r -d '' link; do
      local rel
      local target
      rel="${link#"$root"/}"
      target="$(readlink "$link")" || target="__TCFS_READLINK_FAILED__"
      printf '%s\t%s\n' "$rel" "$target"
    done < <(find "$root" -type l -print0)
  } | sort >"$out"
}

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
timestamp="$(date -u +%Y%m%dT%H%M%SZ)"
run_id="home-canary-linux-xr-shadow-${timestamp}"

source_root="${TCFS_HOME_CANARY_SOURCE:-/Users/jess/git/linux-xr}"
shadow_root="${TCFS_HOME_CANARY_SHADOW_ROOT:-$HOME/TCFS Pilot/real-canaries/linux-xr-shadow-${timestamp}}"
evidence_dir="${TCFS_HOME_CANARY_EVIDENCE_DIR:-$REPO_ROOT/docs/release/evidence/${run_id}}"
remote="${TCFS_HOME_CANARY_REMOTE:-seaweedfs://localhost:8333/tcfs/${run_id}}"
state_dir="${TCFS_HOME_CANARY_STATE_DIR:-}"
tcfs_bin="${TCFS_BIN:-}"
push_remote="$(bool_env TCFS_HOME_CANARY_PUSH "${TCFS_HOME_CANARY_PUSH:-0}")"
resume_after_push="$(bool_env TCFS_HOME_CANARY_RESUME_AFTER_PUSH "${TCFS_HOME_CANARY_RESUME_AFTER_PUSH:-0}")"
reuse_shadow="$(bool_env TCFS_HOME_CANARY_REUSE_SHADOW "${TCFS_HOME_CANARY_REUSE_SHADOW:-0}")"
create_bucket="$(bool_env TCFS_HOME_CANARY_CREATE_BUCKET "${TCFS_HOME_CANARY_CREATE_BUCKET:-0}")"
run_honey="$(bool_env TCFS_HOME_CANARY_RUN_HONEY "${TCFS_HOME_CANARY_RUN_HONEY:-0}")"
run_linux_lifecycle="$(bool_env TCFS_HOME_CANARY_RUN_LINUX_LIFECYCLE "${TCFS_HOME_CANARY_RUN_LINUX_LIFECYCLE:-0}")"
honey_host="${TCFS_HONEY_HOST:-honey}"
honey_mount_root="${TCFS_HONEY_MOUNT_ROOT:-/tmp/tcfs-${run_id}/mount}"
honey_remote_dir="${TCFS_HONEY_REMOTE_DIR:-/tmp/tcfs-${run_id}}"
honey_tcfs_bin="${TCFS_HONEY_TCFS_BIN:-tcfs}"
honey_start_mount="$(bool_env TCFS_HONEY_START_MOUNT "${TCFS_HONEY_START_MOUNT:-0}")"
honey_smoke_max_depth="${TCFS_HONEY_SMOKE_MAX_DEPTH:-8}"
honey_smoke_timeout_secs="${TCFS_HONEY_SMOKE_TIMEOUT_SECS:-900}"
forward_aws_env="$(bool_env TCFS_HONEY_FORWARD_AWS_ENV "${TCFS_HONEY_FORWARD_AWS_ENV:-0}")"
keep_shadow=0

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
    --resume-after-push)
      resume_after_push=1
      shift
      ;;
    --reuse-shadow)
      reuse_shadow=1
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
    --keep-shadow)
      keep_shadow=1
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

if [[ "$push_remote" == "1" && "$resume_after_push" == "1" ]]; then
  fail "--push and --resume-after-push are mutually exclusive"
fi
[[ "$remote" == seaweedfs://* ]] || fail "remote must start with seaweedfs://"
[[ "$honey_smoke_max_depth" =~ ^[0-9]+$ ]] || fail "--honey-smoke-max-depth must be an integer"
[[ "$honey_smoke_timeout_secs" =~ ^[0-9]+$ ]] || fail "--honey-smoke-timeout-secs must be an integer"
case "$honey_remote_dir" in
  *[[:space:]]*) fail "--honey-remote-dir must not contain whitespace: $honey_remote_dir" ;;
esac
if ! [[ "$honey_remote_dir" =~ ^[A-Za-z0-9_./@%+=:-]+$ ]]; then
  fail "--honey-remote-dir contains unsafe shell characters: $honey_remote_dir"
fi

source_canon="$(canonical_existing_path "$source_root")"
home_canon="$(canonical_existing_path "$HOME")"
git_canon="$home_canon/git"
shadow_canon="$(make_physical_dir "$shadow_root")"
mkdir -p "$evidence_dir"
if [[ -z "$state_dir" ]]; then
  state_dir="$evidence_dir/state"
fi
state_canon="$(make_physical_dir "$state_dir")"

case "$source_canon" in
  "$shadow_canon"|"$shadow_canon"/*) fail "source cannot be inside the shadow root" ;;
esac
case "$shadow_canon" in
  "$source_canon"|"$source_canon"/*) fail "shadow root cannot be inside the source tree" ;;
esac
[[ "$source_canon" != "$home_canon" ]] || fail "refusing to canary full HOME"
[[ "$source_canon" != "$git_canon" ]] || fail "refusing to canary full ~/git"

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

tcfs_cmd=()
if [[ -n "$tcfs_bin" ]]; then
  [[ -x "$tcfs_bin" ]] || fail "--tcfs-bin is not executable: $tcfs_bin"
  tcfs_cmd=("$tcfs_bin")
elif [[ -x "$REPO_ROOT/target/debug/tcfs" ]]; then
  tcfs_cmd=("$REPO_ROOT/target/debug/tcfs")
else
  tcfs_cmd=(cargo run --quiet -p tcfs-cli --)
fi

inventory_dir="$evidence_dir/source-inventory"
shadow_inventory_dir="$evidence_dir/shadow-inventory"
mkdir -p "$inventory_dir" "$shadow_inventory_dir"

inventory_tree() {
  local root="$1"
  local out="$2"

  mkdir -p "$out"
  printf '%s\n' "$root" >"$out/root.txt"
  find "$root" -mindepth 1 -maxdepth 12 -print | sort >"$out/tree-maxdepth-12.txt"
  find "$root" -type l -print | sort >"$out/symlinks.txt"
  write_symlink_targets "$root" "$out/symlink-targets.tsv"
  find "$root" \( -type p -o -type s -o -type b -o -type c \) -print | sort >"$out/unsupported-special-files.txt"
  find "$root" -type d -name '.*' -print | sort >"$out/hidden-dirs.txt"
  {
    write_count regular_files "$(find "$root" -type f | wc -l | tr -d ' ')"
    write_count directories "$(find "$root" -type d | wc -l | tr -d ' ')"
    write_count symlinks "$(find "$root" -type l | wc -l | tr -d ' ')"
    write_count hidden_dirs "$(find "$root" -type d -name '.*' | wc -l | tr -d ' ')"
    write_count unsupported_special_files "$(find "$root" \( -type p -o -type s -o -type b -o -type c \) | wc -l | tr -d ' ')"
    if command -v du >/dev/null 2>&1; then
      printf 'du_sk=%s\n' "$(du -sk "$root" | awk '{print $1}')"
    fi
  } >"$out/counts.env"
}

inventory_git() {
  local root="$1"
  local out="$2"

  {
    printf 'git_dir_present='
    if [[ -d "$root/.git" ]]; then
      printf '1\n'
    else
      printf '0\n'
    fi
    if git -C "$root" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
      printf 'branch=%s\n' "$(git -C "$root" branch --show-current 2>/dev/null || true)"
      printf 'head=%s\n' "$(git -C "$root" rev-parse HEAD 2>/dev/null || true)"
      printf 'dirty_status_count=%s\n' "$(git -C "$root" status --porcelain=v1 2>/dev/null | wc -l | tr -d ' ')"
    else
      printf 'branch=\nhead=\ndirty_status_count=not-a-git-worktree\n'
    fi
  } >"$out/git-summary.env"
  git -C "$root" status --porcelain=v1 >"$out/git-status-porcelain.txt" 2>"$out/git-status.err" || true
  git -C "$root" remote -v >"$out/git-remotes.txt" 2>"$out/git-remotes.err" || true
}

printf 'inventorying source: %s\n' "$source_canon"
inventory_tree "$source_canon" "$inventory_dir"
inventory_git "$source_canon" "$inventory_dir"

printf 'creating isolated shadow: %s\n' "$shadow_canon"
if [[ "$reuse_shadow" == "1" ]]; then
  if ! find "$shadow_canon" -mindepth 1 -maxdepth 1 -print -quit | grep -q .; then
    fail "--reuse-shadow requires an existing non-empty shadow root: $shadow_canon"
  fi
  printf 'reused existing shadow without recopying source: %s\n' "$shadow_canon" >"$evidence_dir/shadow-copy.log"
elif command -v rsync >/dev/null 2>&1; then
  rsync -a --delete "$source_canon"/ "$shadow_canon"/ >"$evidence_dir/shadow-copy.log" 2>&1
else
  cp -a "$source_canon"/. "$shadow_canon"/ >"$evidence_dir/shadow-copy.log" 2>&1
fi

inventory_tree "$shadow_canon" "$shadow_inventory_dir"
inventory_git "$shadow_canon" "$shadow_inventory_dir"

shadow_symlink_target_match=0
if cmp -s "$inventory_dir/symlink-targets.tsv" "$shadow_inventory_dir/symlink-targets.tsv"; then
  shadow_symlink_target_match=1
  printf 'source and shadow symlink target manifests match\n' >"$evidence_dir/symlink-shadow-compare.log"
else
  diff -u "$inventory_dir/symlink-targets.tsv" "$shadow_inventory_dir/symlink-targets.tsv" \
    >"$evidence_dir/symlink-shadow-compare.diff" || true
  printf 'source and shadow symlink target manifests differ\n' >"$evidence_dir/symlink-shadow-compare.log"
fi

selected_file="$(find "$shadow_canon" -type f ! -path '*/.git/*' | sort | head -n 1 || true)"
if [[ -n "$selected_file" ]]; then
  selected_rel="${selected_file#"$shadow_canon"/}"
  printf '%s\n' "$selected_rel" >"$evidence_dir/selected-hydration-file.txt"
  cp "$selected_file" "$evidence_dir/selected-hydration-file.content"
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$selected_file" >"$evidence_dir/selected-hydration-file.sha256"
  elif command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$selected_file" >"$evidence_dir/selected-hydration-file.sha256"
  fi
else
  selected_rel=""
  printf 'no regular non-.git file found\n' >"$evidence_dir/selected-hydration-file.txt"
fi

config_path="$state_canon/tcfs-linux-xr-shadow.toml"
state_json="$state_canon/push-state.json"
cat >"$config_path" <<EOF
[daemon]
socket = "$state_canon/no-daemon.sock"

[storage]
endpoint = "$endpoint"
region = "$region"
bucket = "$bucket"
remote_prefix = "$prefix"
enforce_tls = false

[sync]
state_db = "$state_canon/state.db"
sync_root = "$shadow_canon"
nats_url = "nats://localhost:4222"
nats_tls = false
sync_git_dirs = true
git_sync_mode = "raw"
sync_hidden_dirs = true
sync_symlinks = true
sync_empty_dirs = true

[fuse]
cache_dir = "$state_canon/cache"
cache_max_mb = 512
negative_cache_ttl_secs = 1

[crypto]
enabled = false
EOF

symlink_count="$(awk -F= '$1 == "symlinks" { print $2 }' "$inventory_dir/counts.env")"
shadow_symlink_count="$(awk -F= '$1 == "symlinks" { print $2 }' "$shadow_inventory_dir/counts.env")"
unsupported_count="$(awk -F= '$1 == "unsupported_special_files" { print $2 }' "$inventory_dir/counts.env")"
parity_status="full-project-parity-not-claimed"
parity_reason="Symlink preservation is configured for this lane, but full project parity still requires a fresh host packet proving source symlinks rehydrate as symlinks with matching targets."

push_rc=0
honey_rc=0
linux_lifecycle_rc=0
push_available=0
push_status_label=0
honey_status_label=0
linux_lifecycle_status_label=0
mounted_symlink_status_label=not-run
if [[ "$push_remote" == "1" ]]; then
  push_status_label=pending
fi
if [[ "$resume_after_push" == "1" ]]; then
  push_status_label=0
fi
if [[ "$run_honey" == "1" ]]; then
  honey_status_label=pending
  mounted_symlink_status_label=pending
fi
if [[ "$run_linux_lifecycle" == "1" ]]; then
  linux_lifecycle_status_label=pending
fi

compute_parity_status() {
  parity_status="full-project-parity-not-claimed"
  parity_reason="full project parity requires push, mounted traversal/hydration, mounted symlink target verification, Linux lifecycle, and zero unsupported special files"

  if [[ "$unsupported_count" != "0" ]]; then
    parity_reason="source inventory includes unsupported special files"
  elif [[ "$shadow_symlink_count" != "$symlink_count" ]]; then
    parity_reason="source/shadow symlink counts differ"
  elif [[ "$shadow_symlink_target_match" != "1" ]]; then
    parity_reason="source/shadow symlink target manifests differ"
  elif [[ "$push_available" != "1" ]]; then
    parity_reason="shadow push evidence is not present"
  elif [[ "$push_rc" -ne 0 ]]; then
    parity_reason="shadow push failed"
  elif [[ "$run_honey" != "1" ]]; then
    parity_reason="mounted honey traversal and symlink target verification were not run"
  elif [[ "$honey_rc" -ne 0 ]]; then
    parity_reason="mounted honey traversal or symlink target verification failed"
  elif [[ "$run_linux_lifecycle" != "1" ]]; then
    parity_reason="Linux lifecycle companion was not run"
  elif [[ "$linux_lifecycle_rc" -ne 0 ]]; then
    parity_reason="Linux lifecycle companion failed"
  else
    parity_status="scoped-project-tree-parity-evidence-complete"
    parity_reason="shadow push, mounted traversal/hydration, mounted symlink target verification, and Linux lifecycle companion passed for the isolated project-tree canary"
  fi
}

status_from_rc_label() {
  local label="$1"
  case "$label" in
    0)
      printf 'passed'
      ;;
    not-run | pending)
      printf '%s' "$label"
      ;;
    *)
      if [[ "$label" =~ ^[0-9]+$ ]]; then
        printf 'failed'
      else
        printf '%s' "$label"
      fi
      ;;
  esac
}

write_parity_gates() {
  compute_parity_status
  local mounted_symlink_status
  mounted_symlink_status="$(status_from_rc_label "$mounted_symlink_status_label")"
  {
    printf 'status=%s\n' "$parity_status"
    printf 'reason=%s\n' "$parity_reason"
    printf 'source_symlink_count=%s\n' "$symlink_count"
    printf 'shadow_symlink_count=%s\n' "$shadow_symlink_count"
    printf 'shadow_symlink_targets_match=%s\n' "$shadow_symlink_target_match"
    printf 'source_unsupported_special_file_count=%s\n' "$unsupported_count"
    printf 'sync_symlinks=true\n'
    printf 'mounted_symlink_verification=%s\n' "$mounted_symlink_status_label"
    printf 'mounted_symlink_verification_rc=%s\n' "$mounted_symlink_status_label"
    printf 'mounted_symlink_verification_status=%s\n' "$mounted_symlink_status"
  } >"$evidence_dir/parity-gates.env"
}

write_run_metadata() {
  local mounted_symlink_status
  mounted_symlink_status="$(status_from_rc_label "$mounted_symlink_status_label")"
  cat >"$evidence_dir/run-metadata.env" <<EOF
created_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
run_id=$run_id
source=$source_canon
shadow=$shadow_canon
remote=$remote
remote_prefix=$prefix
config_path=$config_path
state_json=$state_json
push=$push_remote
resume_after_push=$resume_after_push
reuse_shadow=$reuse_shadow
push_status=$push_status_label
run_honey=$run_honey
honey_status=$honey_status_label
mounted_symlink_verification=$mounted_symlink_status_label
mounted_symlink_verification_rc=$mounted_symlink_status_label
mounted_symlink_verification_status=$mounted_symlink_status
honey_start_mount=$honey_start_mount
honey_smoke_max_depth=$honey_smoke_max_depth
honey_smoke_timeout_secs=$honey_smoke_timeout_secs
run_linux_lifecycle=$run_linux_lifecycle
linux_lifecycle_status=$linux_lifecycle_status_label
selected_hydration_file=$selected_rel
source_symlink_count=$symlink_count
shadow_symlink_count=$shadow_symlink_count
shadow_symlink_targets_match=$shadow_symlink_target_match
EOF
}

write_result() {
  local status="$1"
  local proof="$2"

  write_parity_gates
  cat >"$evidence_dir/result.env" <<EOF
status=$status
completed_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
proof=$proof
parity_status=$parity_status
parity_reason=$parity_reason
source_symlink_count=$symlink_count
shadow_symlink_count=$shadow_symlink_count
shadow_symlink_targets_match=$shadow_symlink_target_match
source_unsupported_special_file_count=$unsupported_count
EOF
}

write_readme() {
  cat >"$evidence_dir/README.md" <<EOF
# TCFS Home Canary linux-xr Shadow Evidence

Created: $(date -u +%Y-%m-%dT%H:%M:%SZ)

This bundle inventories the live source read-only, copies it to an isolated
shadow, and roots TCFS state/config at that shadow. It does not mutate the live
\`/Users/jess/git/linux-xr\` tree and does not claim \`~/Documents\`, \`~/.local\`,
dotfiles, or broad \`~/git\` takeover.

- Source: \`$source_canon\`
- Shadow: \`$shadow_canon\`
- Remote: \`$remote\`
- Config: \`$config_path\`
- State JSON: \`$state_json\`

Truth gate: full project parity is not claimed until a fresh host packet proves
source symlinks rehydrate as symlinks with matching targets. See
\`parity-gates.env\`, \`source-inventory/symlink-targets.tsv\`, and
\`source-inventory/unsupported-special-files.txt\`.

Contents:

- \`source-inventory/\`: branch, remotes, dirty status, counts, hidden dirs,
  symlinks with targets, unsupported special files, and bounded tree listing
- \`shadow-inventory/\`: same inventory after the isolated copy
- \`symlink-shadow-compare.log\`: local source/shadow symlink target comparison
- \`tcfs-linux-xr-shadow.toml\` under \`state/\`: disposable config with
  \`sync_git_dirs = true\`, \`sync_hidden_dirs = true\`,
  \`git_sync_mode = "raw"\`, \`sync_symlinks = true\`, and
  \`sync_empty_dirs = true\`
- \`push.log\`: shadow push transcript when \`--push\` ran
- \`honey-linux-xr-shadow-commands.txt\`: honey mounted traversal/hydration
  commands for the selected file, \`.git\` traversal, and mounted symlink
  target verification
- \`linux-lifecycle-companion.log\` and \`linux-lifecycle/\`: optional mounted
  write/readback, cache clear/rehydrate, dirty safe-unsync refusal, clean
  recursive unsync, and exact rehydrate companion evidence
EOF
}

cp "$config_path" "$evidence_dir/tcfs-linux-xr-shadow.toml"
write_run_metadata
write_readme
write_result pending pending-home-canary-shadow

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
  fail "--create-bucket requested, but neither aws nor s5cmd was found"
}

if [[ "$push_remote" == "1" ]]; then
  push_available=1
  if [[ -z "${AWS_ACCESS_KEY_ID:-}" && "$endpoint" =~ ^http://(localhost|127\.0\.0\.1)(:[0-9]+)?$ ]]; then
    export AWS_ACCESS_KEY_ID=admin
  fi
  if [[ -z "${AWS_SECRET_ACCESS_KEY:-}" && "$endpoint" =~ ^http://(localhost|127\.0\.0\.1)(:[0-9]+)?$ ]]; then
    export AWS_SECRET_ACCESS_KEY=admin
  fi
  [[ -n "${AWS_ACCESS_KEY_ID:-}" ]] || fail "set AWS_ACCESS_KEY_ID for $endpoint"
  [[ -n "${AWS_SECRET_ACCESS_KEY:-}" ]] || fail "set AWS_SECRET_ACCESS_KEY for $endpoint"
  create_bucket_if_requested
  printf 'pushing shadow to disposable prefix: %s\n' "$remote"
  "${tcfs_cmd[@]}" --config "$config_path" push "$shadow_canon" --prefix "$prefix" --state "$state_json" \
    >"$evidence_dir/push.log" 2>&1 || push_rc=$?
  push_status_label=$push_rc
  write_run_metadata
  if [[ "$push_rc" -ne 0 ]]; then
    write_result "$push_rc" push-failed
  fi
fi

if [[ "$resume_after_push" == "1" ]]; then
  [[ -f "$evidence_dir/push.log" ]] || fail "--resume-after-push requires existing push.log in $evidence_dir"
  grep -Fq 'Push complete:' "$evidence_dir/push.log" || fail "--resume-after-push requires push.log with 'Push complete:'"
  [[ -f "$state_json" ]] || fail "--resume-after-push requires existing state JSON: $state_json"
  push_available=1
  push_status_label=0
  write_run_metadata
fi

honey_script="$evidence_dir/honey-linux-xr-shadow-run.sh"
honey_commands="$evidence_dir/honey-linux-xr-shadow-commands.txt"
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
SMOKE_SCRIPT="\${TCFS_HONEY_SMOKE_SCRIPT:-$(shell_quote "$honey_remote_dir/lazy-hydration-mounted-smoke.sh")}"
EXPECTED_CONTENT_FILE="\${TCFS_HONEY_EXPECTED_CONTENT_FILE:-$(shell_quote "$honey_remote_dir/selected-hydration-file.content")}"
SYMLINK_TARGETS_FILE="\${TCFS_HONEY_SYMLINK_TARGETS_FILE:-$(shell_quote "$honey_remote_dir/symlink-targets.tsv")}"
MOUNT_LOG="\${TCFS_HONEY_MOUNT_LOG:-$(shell_quote "$honey_remote_dir/mount.log")}"
SMOKE_MAX_DEPTH="\${TCFS_HONEY_SMOKE_MAX_DEPTH:-$(shell_quote "$honey_smoke_max_depth")}"
SMOKE_TIMEOUT_SECS="\${TCFS_HONEY_SMOKE_TIMEOUT_SECS:-$(shell_quote "$honey_smoke_timeout_secs")}"
EXPECTED_FILE=$(shell_quote "$selected_rel")

if [[ -n "\${TCFS_HONEY_ENV_FILE:-}" ]]; then
  # shellcheck disable=SC1090
  source "\$TCFS_HONEY_ENV_FILE"
fi

echo "tcfs binary: \$TCFS_BIN"
if command -v "\$TCFS_BIN" >/dev/null 2>&1; then
  echo "tcfs binary resolved: \$(command -v "\$TCFS_BIN")"
fi
tcfs_version="\$("\$TCFS_BIN" --version 2>&1)" || {
  printf 'failed to run tcfs --version through %s\n' "\$TCFS_BIN" >&2
  printf '%s\n' "\$tcfs_version" >&2
  exit 1
}
echo "tcfs version: \$tcfs_version"
if [[ -n "\${TCFS_HONEY_EXPECTED_VERSION_CONTAINS:-}" && "\$tcfs_version" != *"\$TCFS_HONEY_EXPECTED_VERSION_CONTAINS"* ]]; then
  printf 'tcfs version mismatch: expected output containing %s\n' "\$TCFS_HONEY_EXPECTED_VERSION_CONTAINS" >&2
  exit 1
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

args=(
  --mount-root "\$MOUNT_ROOT"
  --expect-entry .git
  --max-depth "\$SMOKE_MAX_DEPTH"
)
if [[ -n "\$EXPECTED_FILE" ]]; then
  args+=(--expected-file "\$EXPECTED_FILE")
fi
if [[ -f "\$EXPECTED_CONTENT_FILE" ]]; then
  args+=(--expected-content-file "\$EXPECTED_CONTENT_FILE")
fi
if [[ -f "\$SYMLINK_TARGETS_FILE" ]]; then
  args+=(--expected-symlink-targets-file "\$SYMLINK_TARGETS_FILE")
fi

if [[ "\$SMOKE_TIMEOUT_SECS" != "0" && "\$SMOKE_TIMEOUT_SECS" =~ ^[0-9]+$ ]] && command -v timeout >/dev/null 2>&1; then
  timeout "\$SMOKE_TIMEOUT_SECS" bash "\$SMOKE_SCRIPT" "\${args[@]}"
else
  bash "\$SMOKE_SCRIPT" "\${args[@]}"
fi
EOF
chmod +x "$honey_script"

cat >"$honey_commands" <<EOF
# Run after the shadow prefix is mounted on honey at:
# $(shell_quote "$honey_mount_root")
ssh $(shell_quote "$honey_host") 'mkdir -p $(shell_quote "$honey_remote_dir")'
scp $(shell_quote "$REPO_ROOT/scripts/lazy-hydration-mounted-smoke.sh") $(shell_quote "$honey_host"):$(shell_quote "$honey_remote_dir/lazy-hydration-mounted-smoke.sh")
scp $(shell_quote "$evidence_dir/selected-hydration-file.content") $(shell_quote "$honey_host"):$(shell_quote "$honey_remote_dir/selected-hydration-file.content")
scp $(shell_quote "$inventory_dir/symlink-targets.tsv") $(shell_quote "$honey_host"):$(shell_quote "$honey_remote_dir/symlink-targets.tsv")
scp $(shell_quote "$honey_script") $(shell_quote "$honey_host"):$(shell_quote "$honey_remote_dir/honey-linux-xr-shadow-run.sh")
ssh $(shell_quote "$honey_host") 'chmod +x $(shell_quote "$honey_remote_dir/lazy-hydration-mounted-smoke.sh") $(shell_quote "$honey_remote_dir/honey-linux-xr-shadow-run.sh")'
ssh $(shell_quote "$honey_host") 'TCFS_HONEY_START_MOUNT=1 TCFS_HONEY_SMOKE_SCRIPT=$(shell_quote "$honey_remote_dir/lazy-hydration-mounted-smoke.sh") TCFS_HONEY_EXPECTED_CONTENT_FILE=$(shell_quote "$honey_remote_dir/selected-hydration-file.content") TCFS_HONEY_SYMLINK_TARGETS_FILE=$(shell_quote "$honey_remote_dir/symlink-targets.tsv") TCFS_HONEY_MOUNT_LOG=$(shell_quote "$honey_remote_dir/mount.log") TCFS_HONEY_SMOKE_MAX_DEPTH=$(shell_quote "$honey_smoke_max_depth") TCFS_HONEY_SMOKE_TIMEOUT_SECS=$(shell_quote "$honey_smoke_timeout_secs") bash $(shell_quote "$honey_remote_dir/honey-linux-xr-shadow-run.sh")'
EOF

if [[ "$run_honey" == "1" ]]; then
  command -v ssh >/dev/null 2>&1 || fail "ssh not found"
  command -v scp >/dev/null 2>&1 || fail "scp not found"
  # shellcheck disable=SC2029
  ssh "$honey_host" "mkdir -p $(shell_quote "$honey_remote_dir")"
  scp "$REPO_ROOT/scripts/lazy-hydration-mounted-smoke.sh" "$honey_host:$honey_remote_dir/lazy-hydration-mounted-smoke.sh"
  scp "$evidence_dir/selected-hydration-file.content" "$honey_host:$honey_remote_dir/selected-hydration-file.content"
  scp "$inventory_dir/symlink-targets.tsv" "$honey_host:$honey_remote_dir/symlink-targets.tsv"
  scp "$honey_script" "$honey_host:$honey_remote_dir/honey-linux-xr-shadow-run.sh"
  # shellcheck disable=SC2029
  ssh "$honey_host" "chmod +x $(shell_quote "$honey_remote_dir/lazy-hydration-mounted-smoke.sh") $(shell_quote "$honey_remote_dir/honey-linux-xr-shadow-run.sh")"

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

  remote_run_cmd="$(printf 'TCFS_HONEY_START_MOUNT=%q TCFS_HONEY_SMOKE_SCRIPT=%q TCFS_HONEY_EXPECTED_CONTENT_FILE=%q TCFS_HONEY_SYMLINK_TARGETS_FILE=%q TCFS_HONEY_MOUNT_LOG=%q TCFS_HONEY_SMOKE_MAX_DEPTH=%q TCFS_HONEY_SMOKE_TIMEOUT_SECS=%q bash %q' \
    "$honey_start_mount" \
    "$honey_remote_dir/lazy-hydration-mounted-smoke.sh" \
    "$honey_remote_dir/selected-hydration-file.content" \
    "$honey_remote_dir/symlink-targets.tsv" \
    "$honey_remote_dir/mount.log" \
    "$honey_smoke_max_depth" \
    "$honey_smoke_timeout_secs" \
    "$honey_remote_dir/honey-linux-xr-shadow-run.sh")"
  if [[ -n "$remote_env_file" ]]; then
    remote_run_cmd="$(printf 'TCFS_HONEY_ENV_FILE=%q %s' "$remote_env_file" "$remote_run_cmd")"
  fi

  # shellcheck disable=SC2029
  ssh "$honey_host" "$remote_run_cmd" >"$evidence_dir/honey-linux-xr-shadow.log" 2>&1 || honey_rc=$?
  honey_status_label=$honey_rc
  mounted_symlink_status_label=$honey_rc
  write_run_metadata
  if [[ "$honey_rc" -ne 0 ]]; then
    write_result "$honey_rc" honey-smoke-failed
  fi
  # shellcheck disable=SC2029
  ssh "$honey_host" "test -f $(shell_quote "$honey_remote_dir/mount.log") && cat $(shell_quote "$honey_remote_dir/mount.log")" >"$evidence_dir/honey-mount.log" 2>/dev/null || true
  cleanup_remote_env
fi

linux_lifecycle_remote="${remote%/}/linux-lifecycle"
if [[ "$run_linux_lifecycle" == "1" ]]; then
  args=(
    --remote "$linux_lifecycle_remote"
    --evidence-dir "$evidence_dir/linux-lifecycle"
    --honey-host "$honey_host"
    --honey-remote-dir "$honey_remote_dir/linux-lifecycle"
    --honey-tcfs-bin "$honey_tcfs_bin"
    --run-linux-lifecycle
  )
  if [[ "$create_bucket" == "1" ]]; then
    args+=(--create-bucket)
  fi
  if [[ "$forward_aws_env" == "1" ]]; then
    args+=(--forward-aws-env)
  fi
  bash "$REPO_ROOT/scripts/fleet-parity-pilot-demo.sh" "${args[@]}" \
    >"$evidence_dir/linux-lifecycle-companion.log" 2>&1 || linux_lifecycle_rc=$?
  linux_lifecycle_status_label=$linux_lifecycle_rc
  write_run_metadata
  if [[ "$linux_lifecycle_rc" -ne 0 ]]; then
    write_result "$linux_lifecycle_rc" linux-lifecycle-companion-failed
  fi
fi

write_run_metadata
write_readme
push_available=0
if [[ "$push_remote" == "1" || "$resume_after_push" == "1" ]]; then
  push_available=1
fi
if [[ "$push_available" == "0" ]]; then
  write_result plan-only inventory-shadow-config
elif [[ "$push_rc" -eq 0 && "$run_honey" == "1" && "$honey_rc" -eq 0 && "$run_linux_lifecycle" == "1" && "$linux_lifecycle_rc" -eq 0 ]]; then
  write_result 0 shadow-push-honey-linux-lifecycle-symlink-targets
elif [[ "$push_rc" -eq 0 && "$run_honey" == "1" && "$honey_rc" -eq 0 ]]; then
  write_result 0 shadow-push-honey-traversal-symlink-targets
elif [[ "$push_rc" -eq 0 && "$run_honey" == "0" && "$run_linux_lifecycle" == "0" ]]; then
  write_result 0 shadow-push
fi

printf 'home canary evidence: %s\n' "$evidence_dir"
printf 'shadow root: %s\n' "$shadow_canon"
printf 'remote: %s\n' "$remote"
printf 'parity gate: %s (source symlinks=%s unsupported_special_files=%s)\n' \
  "$parity_status" "$symlink_count" "$unsupported_count"
if [[ "$keep_shadow" != "1" ]]; then
  printf 'shadow cleanup after review: rm -rf %q\n' "$shadow_canon"
fi

if [[ "$push_rc" -ne 0 ]]; then
  printf 'push failed; see %s\n' "$evidence_dir/push.log" >&2
  exit "$push_rc"
fi
if [[ "$honey_rc" -ne 0 ]]; then
  printf 'honey smoke failed; see %s\n' "$evidence_dir/honey-linux-xr-shadow.log" >&2
  exit "$honey_rc"
fi
if [[ "$linux_lifecycle_rc" -ne 0 ]]; then
  printf 'linux lifecycle companion failed; see %s\n' "$evidence_dir/linux-lifecycle-companion.log" >&2
  exit "$linux_lifecycle_rc"
fi
