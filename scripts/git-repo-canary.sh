#!/usr/bin/env bash
#
# Safe generic git-repo canary wrapper.
#
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/git-repo-canary.sh [options]

Create a shadow-first canary packet for one git worktree. This wrapper defaults
to a small clean repo and delegates the actual inventory/shadow/push/honey
workflow to scripts/home-canary-linux-xr-shadow.sh with explicit source,
shadow, evidence, and remote paths.

Options:
  --source <path>        Git worktree to snapshot. Default: ~/git/oauth-mux
  --name <name>          Canary name. Default: basename of --source
  --shadow-root <path>   Shadow path. Default: ~/TCFS Pilot/real-canaries/<name>-shadow-<UTC>
  --evidence-dir <path>  Evidence dir. Default: docs/release/evidence/git-repo-canary-<name>-<UTC>
  --remote <url>         seaweedfs://host:port/bucket/prefix disposable remote
  --state-dir <path>     Local TCFS state/config dir. Default: <evidence-dir>/state
  --tcfs-bin <path>      tcfs binary for local push/proof
  --push                 Push the shadow to the disposable prefix
  --resume-after-push    Reuse existing completed push evidence
  --reuse-shadow         Do not recopy source into shadow
  --create-bucket        Best-effort bucket creation before push/lifecycle
  --run-honey            Run honey mounted traversal/hydration companion
  --run-linux-lifecycle  Run Linux lifecycle companion on honey
  --honey-host <host>    SSH host label. Default: honey
  --honey-mount-root <path>
                          Honey mountpoint
  --honey-remote-dir <path>
                          Honey work dir
  --honey-tcfs-bin <path>
                          tcfs binary on honey. Default: tcfs
  --honey-start-mount    With --run-honey, start tcfs mount on honey
  --honey-existing-mount With --run-honey, assume honey mount is already active
  --honey-smoke-max-depth <n>
                          Mounted traversal depth
  --honey-smoke-timeout-secs <n>
                          Mounted smoke timeout when timeout(1) exists
  --forward-aws-env      Forward AWS env to honey
  --keep-shadow          Do not print cleanup hint for the shadow
  --allow-dirty-source   Snapshot a dirty worktree and record the dirty count
  -h, --help             Show this help

Environment mirrors:
  TCFS_GIT_CANARY_SOURCE
  TCFS_GIT_CANARY_NAME
  TCFS_GIT_CANARY_SHADOW_ROOT
  TCFS_GIT_CANARY_EVIDENCE_DIR
  TCFS_GIT_CANARY_REMOTE
  TCFS_GIT_CANARY_STATE_DIR
  TCFS_GIT_CANARY_ALLOW_DIRTY_SOURCE=1
  TCFS_GIT_CANARY_PUSH=1
  TCFS_GIT_CANARY_RESUME_AFTER_PUSH=1
  TCFS_GIT_CANARY_REUSE_SHADOW=1
  TCFS_GIT_CANARY_CREATE_BUCKET=1
  TCFS_GIT_CANARY_RUN_HONEY=1
  TCFS_GIT_CANARY_RUN_LINUX_LIFECYCLE=1
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

sanitize_name() {
  local name="$1"
  name="${name##*/}"
  name="$(printf '%s' "$name" | tr -c 'A-Za-z0-9._-' '-')"
  name="${name#-}"
  name="${name%-}"
  [[ -n "$name" ]] || name="repo"
  printf '%s\n' "$name"
}

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
timestamp="$(date -u +%Y%m%dT%H%M%SZ)"

source_root="${TCFS_GIT_CANARY_SOURCE:-$HOME/git/oauth-mux}"
canary_name="${TCFS_GIT_CANARY_NAME:-}"
shadow_root="${TCFS_GIT_CANARY_SHADOW_ROOT:-}"
evidence_dir="${TCFS_GIT_CANARY_EVIDENCE_DIR:-}"
remote="${TCFS_GIT_CANARY_REMOTE:-}"
state_dir="${TCFS_GIT_CANARY_STATE_DIR:-}"
tcfs_bin="${TCFS_BIN:-}"
allow_dirty_source="$(bool_env TCFS_GIT_CANARY_ALLOW_DIRTY_SOURCE "${TCFS_GIT_CANARY_ALLOW_DIRTY_SOURCE:-0}")"
push_remote="$(bool_env TCFS_GIT_CANARY_PUSH "${TCFS_GIT_CANARY_PUSH:-0}")"
resume_after_push="$(bool_env TCFS_GIT_CANARY_RESUME_AFTER_PUSH "${TCFS_GIT_CANARY_RESUME_AFTER_PUSH:-0}")"
reuse_shadow="$(bool_env TCFS_GIT_CANARY_REUSE_SHADOW "${TCFS_GIT_CANARY_REUSE_SHADOW:-0}")"
create_bucket="$(bool_env TCFS_GIT_CANARY_CREATE_BUCKET "${TCFS_GIT_CANARY_CREATE_BUCKET:-0}")"
run_honey="$(bool_env TCFS_GIT_CANARY_RUN_HONEY "${TCFS_GIT_CANARY_RUN_HONEY:-0}")"
run_linux_lifecycle="$(bool_env TCFS_GIT_CANARY_RUN_LINUX_LIFECYCLE "${TCFS_GIT_CANARY_RUN_LINUX_LIFECYCLE:-0}")"
honey_host="${TCFS_HONEY_HOST:-honey}"
honey_mount_root="${TCFS_HONEY_MOUNT_ROOT:-}"
honey_remote_dir="${TCFS_HONEY_REMOTE_DIR:-}"
honey_tcfs_bin="${TCFS_HONEY_TCFS_BIN:-tcfs}"
honey_start_mount="$(bool_env TCFS_HONEY_START_MOUNT "${TCFS_HONEY_START_MOUNT:-0}")"
honey_existing_mount=0
forward_aws_env="$(bool_env TCFS_HONEY_FORWARD_AWS_ENV "${TCFS_HONEY_FORWARD_AWS_ENV:-0}")"
keep_shadow=0
honey_smoke_max_depth="${TCFS_HONEY_SMOKE_MAX_DEPTH:-}"
honey_smoke_timeout_secs="${TCFS_HONEY_SMOKE_TIMEOUT_SECS:-}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --source)
      [[ $# -ge 2 ]] || fail "--source requires a value"
      source_root="$2"
      shift 2
      ;;
    --name)
      [[ $# -ge 2 ]] || fail "--name requires a value"
      canary_name="$2"
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
      honey_existing_mount=0
      shift
      ;;
    --honey-existing-mount)
      honey_start_mount=0
      honey_existing_mount=1
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
    --allow-dirty-source)
      allow_dirty_source=1
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

source_canon="$(canonical_existing_path "$source_root")"
if ! git -C "$source_canon" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  fail "source is not a git worktree: $source_canon"
fi

if [[ -z "$canary_name" ]]; then
  canary_name="$(sanitize_name "$source_canon")"
else
  canary_name="$(sanitize_name "$canary_name")"
fi
if [[ -z "$shadow_root" ]]; then
  shadow_root="$HOME/TCFS Pilot/real-canaries/${canary_name}-shadow-${timestamp}"
fi
if [[ -n "$evidence_dir" ]]; then
  evidence_base="$(basename "$evidence_dir")"
  if [[ "$evidence_base" == git-repo-canary-* ]]; then
    run_id="$evidence_base"
  else
    run_id="git-repo-canary-${canary_name}-${timestamp}"
  fi
else
  run_id="git-repo-canary-${canary_name}-${timestamp}"
fi
if [[ -z "$evidence_dir" ]]; then
  evidence_dir="$REPO_ROOT/docs/release/evidence/${run_id}"
fi
if [[ -z "$remote" ]]; then
  remote="seaweedfs://localhost:8333/tcfs/${run_id}"
fi

branch="$(git -C "$source_canon" branch --show-current 2>/dev/null || true)"
head="$(git -C "$source_canon" rev-parse HEAD 2>/dev/null || true)"
dirty_count="$(git -C "$source_canon" status --porcelain=v1 2>/dev/null | wc -l | tr -d ' ')"
tracked_count="$(git -C "$source_canon" ls-files 2>/dev/null | wc -l | tr -d ' ')"
untracked_count="$(git -C "$source_canon" ls-files --others --exclude-standard 2>/dev/null | wc -l | tr -d ' ')"

if [[ "$dirty_count" != "0" && "$allow_dirty_source" != "1" ]]; then
  fail "source has dirty status ($dirty_count entries); pass --allow-dirty-source to snapshot it intentionally"
fi

shadow_canon="$(make_physical_dir "$shadow_root")"
mkdir -p "$evidence_dir"
evidence_dir="$(cd "$evidence_dir" && pwd -P)"

cat >"$evidence_dir/git-repo-canary-policy.env" <<EOF
created_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
run_id=$run_id
canary_name=$canary_name
source=$source_canon
shadow=$shadow_canon
remote=$remote
branch=$branch
head=$head
dirty_status_count=$dirty_count
tracked_count=$tracked_count
untracked_count=$untracked_count
allow_dirty_source=$allow_dirty_source
shadow_first=1
live_repo_mutation=0
finder_claim=0
full_home_claim=0
recommended_first_canary=oauth-mux-shadow
recommended_large_canary=linux-xr-fast-shadow
EOF

cat >"$evidence_dir/git-repo-canary-summary.md" <<EOF
# TCFS Git Repo Canary Summary

This packet is a shadow-first git repo canary. It is safe to use for planning
and evidence gathering because it snapshots the selected worktree into an
isolated shadow before any TCFS push or remote proof.

- Source: \`$source_canon\`
- Shadow: \`$shadow_canon\`
- Remote: \`$remote\`
- Branch: \`$branch\`
- Dirty status entries: \`$dirty_count\`
- Tracked files: \`$tracked_count\`
- Untracked files: \`$untracked_count\`

Boundaries:

- This packet does not mutate the live source repo.
- This packet does not claim Finder/FileProvider production readiness.
- This packet does not claim broad \`~/git\`, \`~/Documents\`, dotfile, or home
  directory takeover.
- A live repo should not be physically moved into TCFS until a shadow packet
  proves restore-from-remote, cross-host rehydrate, and safe-unsync behavior.
EOF

write_generic_readme() {
  cat >"$evidence_dir/README.md" <<EOF
# TCFS Git Repo Canary Evidence

Created: $(date -u +%Y-%m-%dT%H:%M:%SZ)

This bundle inventories one git worktree read-only, copies it to an isolated
shadow, and roots TCFS state/config at that shadow. It does not mutate the live
source repo and does not claim Finder/FileProvider production readiness,
\`~/Documents\`, dotfiles, \`.local\`, broad \`~/git\`, or home-directory takeover.

- Canary: \`$canary_name\`
- Source: \`$source_canon\`
- Shadow: \`$shadow_canon\`
- Remote: \`$remote\`
- Branch: \`$branch\`
- Head: \`$head\`
- Config: \`$evidence_dir/state/tcfs-git-repo-canary.toml\`
- State JSON: \`$evidence_dir/state/push-state.json\`

Truth gate: scoped project-tree parity is claimable only when
\`parity-gates.env\` reports \`scoped-project-tree-parity-evidence-complete\`.
Plan-only packets should report \`full-project-parity-not-claimed\` until push,
honey mounted traversal/hydration, mounted symlink verification, and the Linux
lifecycle companion run.

Contents:

- \`git-repo-canary-policy.env\`: shadow-first claim boundaries and source git
  metadata
- \`git-repo-canary-summary.md\`: short human-readable dogfood summary
- \`source-inventory/\`: branch, remotes, dirty status, counts, hidden dirs,
  symlinks with targets, unsupported special files, and bounded tree listing
- \`shadow-inventory/\`: same inventory after the isolated copy
- \`symlink-shadow-compare.log\`: local source/shadow symlink target comparison
- \`state/tcfs-git-repo-canary.toml\`: generic alias for the disposable config
  with raw \`.git\`, hidden-dir, symlink, and empty-dir sync enabled
- \`push.log\` or \`push.log.gz\`: shadow push transcript when \`--push\` ran
- \`honey-git-repo-canary-commands.txt\`: generic alias for the honey mounted
  proof command packet for traversal, selected hydration, and mounted symlink
  checks
- \`linux-lifecycle-companion.log\` and \`linux-lifecycle/\`: optional mounted
  write/readback, cache clear/rehydrate, dirty safe-unsync refusal, clean
  recursive unsync, and exact rehydrate companion evidence
EOF
}

copy_if_exists() {
  local src="$1"
  local dst="$2"

  if [[ -f "$src" ]]; then
    cp -p "$src" "$dst"
  fi
}

write_generic_aliases() {
  local state_alias_dir="$evidence_dir/state"

  if [[ -n "$state_dir" && -d "$state_dir" ]]; then
    state_alias_dir="$(cd "$state_dir" && pwd -P)"
  fi

  copy_if_exists \
    "$state_alias_dir/tcfs-linux-xr-shadow.toml" \
    "$state_alias_dir/tcfs-git-repo-canary.toml"
  copy_if_exists \
    "$evidence_dir/tcfs-linux-xr-shadow.toml" \
    "$evidence_dir/tcfs-git-repo-canary.toml"
  copy_if_exists \
    "$evidence_dir/honey-linux-xr-shadow-commands.txt" \
    "$evidence_dir/honey-git-repo-canary-commands.txt"
  copy_if_exists \
    "$evidence_dir/honey-linux-xr-shadow-run.sh" \
    "$evidence_dir/honey-git-repo-canary-run.sh"
  copy_if_exists \
    "$evidence_dir/honey-linux-xr-shadow.log" \
    "$evidence_dir/honey-git-repo-canary.log"
}

args=(
  --source "$source_canon"
  --shadow-root "$shadow_canon"
  --evidence-dir "$evidence_dir"
  --remote "$remote"
)

if [[ -n "$state_dir" ]]; then
  args+=(--state-dir "$state_dir")
fi
if [[ -n "$tcfs_bin" ]]; then
  args+=(--tcfs-bin "$tcfs_bin")
fi
if [[ "$push_remote" == "1" ]]; then
  args+=(--push)
fi
if [[ "$resume_after_push" == "1" ]]; then
  args+=(--resume-after-push)
fi
if [[ "$reuse_shadow" == "1" ]]; then
  args+=(--reuse-shadow)
fi
if [[ "$create_bucket" == "1" ]]; then
  args+=(--create-bucket)
fi
if [[ "$run_honey" == "1" ]]; then
  args+=(--run-honey)
fi
if [[ "$run_linux_lifecycle" == "1" ]]; then
  args+=(--run-linux-lifecycle)
fi
if [[ -n "$honey_host" ]]; then
  args+=(--honey-host "$honey_host")
fi
if [[ -n "$honey_mount_root" ]]; then
  args+=(--honey-mount-root "$honey_mount_root")
fi
if [[ -n "$honey_remote_dir" ]]; then
  args+=(--honey-remote-dir "$honey_remote_dir")
fi
if [[ -n "$honey_tcfs_bin" ]]; then
  args+=(--honey-tcfs-bin "$honey_tcfs_bin")
fi
if [[ "$honey_start_mount" == "1" ]]; then
  args+=(--honey-start-mount)
elif [[ "$honey_existing_mount" == "1" ]]; then
  args+=(--honey-existing-mount)
fi
if [[ -n "$honey_smoke_max_depth" ]]; then
  args+=(--honey-smoke-max-depth "$honey_smoke_max_depth")
fi
if [[ -n "$honey_smoke_timeout_secs" ]]; then
  args+=(--honey-smoke-timeout-secs "$honey_smoke_timeout_secs")
fi
if [[ "$forward_aws_env" == "1" ]]; then
  args+=(--forward-aws-env)
fi
if [[ "$keep_shadow" == "1" ]]; then
  args+=(--keep-shadow)
fi

helper_rc=0
"$REPO_ROOT/scripts/home-canary-linux-xr-shadow.sh" "${args[@]}" || helper_rc=$?
write_generic_aliases
write_generic_readme

printf 'git repo canary evidence: %s\n' "$evidence_dir"
printf 'git repo canary policy: %s\n' "$evidence_dir/git-repo-canary-policy.env"
exit "$helper_rc"
