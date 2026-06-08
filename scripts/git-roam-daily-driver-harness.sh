#!/usr/bin/env bash
#
# Plan-only packet generator for the TCFS `~/git` daily-driver roam story.
#
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/git-roam-daily-driver-harness.sh [options]

Create a plan-only evidence packet for one repo plus matching agent context.
This does not call tcfs, ssh, cargo, nix, or mutate the repo.

Options:
  --repo <path>                 Git worktree to inspect. Default: $PWD
  --agent-root <path>           Agent context root. Default: ~/.claude/projects
  --name <name>                 Packet name. Default: basename of --repo
  --origin-host <host>          Host where work starts. Default: hostname
  --continuation-host <host>    Host where work continues. Default: honey
  --third-host <host>           Optional third enrolled host, e.g. bumble
  --remote-prefix <prefix>      Expected TCFS prefix. Default: git-roam/<name>
  --evidence-dir <path>         Evidence output dir
  -h, --help                    Show this help

Environment mirrors:
  TCFS_GIT_ROAM_REPO
  TCFS_GIT_ROAM_AGENT_ROOT
  TCFS_GIT_ROAM_NAME
  TCFS_GIT_ROAM_ORIGIN_HOST
  TCFS_GIT_ROAM_CONTINUATION_HOST
  TCFS_GIT_ROAM_THIRD_HOST
  TCFS_GIT_ROAM_REMOTE_PREFIX
  TCFS_GIT_ROAM_EVIDENCE_DIR
EOF
}

fail() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

canonical_existing_path() {
  local path="$1"
  [[ -e "$path" ]] || fail "path does not exist: $path"
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

host_name() {
  hostname 2>/dev/null || printf 'unknown-host\n'
}

write_tree_hashes() {
  local root="$1"
  local output="$2"
  local mode="$3"

  : >"$output"
  if [[ ! -d "$root" ]]; then
    printf 'status=missing root=%s\n' "$root" >"$output"
    return 0
  fi

  (
    cd "$root"
    case "$mode" in
      repo)
        find . -type f \
          ! -path './.git/*' \
          ! -path './node_modules/*' \
          ! -path './target/*' \
          ! -path './.svelte-kit/*' \
          ! -path './build/*' \
          ! -path './.venv/*' \
          -print | LC_ALL=C sort
        ;;
      agent)
        find . -type f \
          ! -name 'auth.json' \
          ! -name '.credentials.json' \
          ! -name 'mcp-auth.json' \
          ! -name 'mcp-needs-auth-cache.json' \
          ! -name '*.sqlite' \
          ! -name '*.db' \
          ! -name '*-wal' \
          ! -name '*-shm' \
          ! -path './.cache/*' \
          -print | LC_ALL=C sort
        ;;
      *)
        fail "unknown hash mode: $mode"
        ;;
    esac
  ) | while IFS= read -r rel; do
    [[ -n "$rel" ]] || continue
    (cd "$root" && shasum -a 256 "$rel")
  done >"$output"
}

write_symlinks() {
  local root="$1"
  local output="$2"

  : >"$output"
  [[ -d "$root" ]] || return 0
  while IFS= read -r link_path; do
    [[ -n "$link_path" ]] || continue
    printf '%s -> %s\n' "$link_path" "$(readlink "$link_path" 2>/dev/null || true)"
  done < <(find "$root" -type l -print | LC_ALL=C sort)
}

write_policy_scan() {
  local root="$1"
  local label="$2"
  local output="$3"

  [[ -d "$root" ]] || return 0
  {
    while IFS= read -r path; do
      [[ -n "$path" ]] || continue
      local base
      base="$(basename "$path")"
      case "$path" in
        */.git|*/.git/*)
          printf '%s\tgit-safety\t%s\n' "$label" "$path"
          ;;
        */node_modules|*/node_modules/*|*/target|*/target/*|*/.svelte-kit|*/.svelte-kit/*|*/build|*/build/*|*/.venv|*/.venv/*)
          printf '%s\tgenerated-deny\t%s\n' "$label" "$path"
          ;;
        */.ssh|*/.ssh/*|*/.gnupg|*/.gnupg/*|*/.config/sops-nix/secrets|*/.config/sops-nix/secrets/*)
          printf '%s\tsecret-deny\t%s\n' "$label" "$path"
          ;;
      esac
      case "$base" in
        .env|.env.*|auth.json|.credentials.json|mcp-auth.json|mcp-needs-auth-cache.json|session-env)
          printf '%s\tsecret-deny\t%s\n' "$label" "$path"
          ;;
        *.sqlite|*.db|*-wal|*-shm|*.wal|*.shm)
          printf '%s\tlive-db-deny\t%s\n' "$label" "$path"
          ;;
        *.log)
          printf '%s\tlog-deny\t%s\n' "$label" "$path"
          ;;
      esac
    done < <(find "$root" -print | LC_ALL=C sort)
  } >>"$output"
}

repo_root="${TCFS_GIT_ROAM_REPO:-$PWD}"
agent_root="${TCFS_GIT_ROAM_AGENT_ROOT:-$HOME/.claude/projects}"
packet_name="${TCFS_GIT_ROAM_NAME:-}"
origin_host="${TCFS_GIT_ROAM_ORIGIN_HOST:-$(host_name)}"
continuation_host="${TCFS_GIT_ROAM_CONTINUATION_HOST:-honey}"
third_host="${TCFS_GIT_ROAM_THIRD_HOST:-}"
remote_prefix="${TCFS_GIT_ROAM_REMOTE_PREFIX:-}"
evidence_dir="${TCFS_GIT_ROAM_EVIDENCE_DIR:-}"
timestamp="$(date -u +%Y%m%dT%H%M%SZ)"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo)
      [[ $# -ge 2 ]] || fail "--repo requires a value"
      repo_root="$2"
      shift 2
      ;;
    --agent-root)
      [[ $# -ge 2 ]] || fail "--agent-root requires a value"
      agent_root="$2"
      shift 2
      ;;
    --name)
      [[ $# -ge 2 ]] || fail "--name requires a value"
      packet_name="$2"
      shift 2
      ;;
    --origin-host)
      [[ $# -ge 2 ]] || fail "--origin-host requires a value"
      origin_host="$2"
      shift 2
      ;;
    --continuation-host)
      [[ $# -ge 2 ]] || fail "--continuation-host requires a value"
      continuation_host="$2"
      shift 2
      ;;
    --third-host)
      [[ $# -ge 2 ]] || fail "--third-host requires a value"
      third_host="$2"
      shift 2
      ;;
    --remote-prefix)
      [[ $# -ge 2 ]] || fail "--remote-prefix requires a value"
      remote_prefix="$2"
      shift 2
      ;;
    --evidence-dir)
      [[ $# -ge 2 ]] || fail "--evidence-dir requires a value"
      evidence_dir="$2"
      shift 2
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

repo_root="$(canonical_existing_path "$repo_root")"
git -C "$repo_root" rev-parse --is-inside-work-tree >/dev/null 2>&1 ||
  fail "repo is not a git worktree: $repo_root"

if [[ -z "$packet_name" ]]; then
  packet_name="$(sanitize_name "$repo_root")"
else
  packet_name="$(sanitize_name "$packet_name")"
fi
remote_prefix="${remote_prefix:-git-roam/$packet_name}"

if [[ -n "$agent_root" && -d "$agent_root" ]]; then
  agent_root="$(canonical_existing_path "$agent_root")"
fi

if [[ -z "$evidence_dir" ]]; then
  evidence_dir="docs/release/evidence/git-roam-${packet_name}-${timestamp}"
fi
mkdir -p "$evidence_dir"

git_status_count="$(git -C "$repo_root" status --porcelain=v1 | wc -l | tr -d ' ')"

{
  printf 'status=plan-only\n'
  printf 'packet=git-roam-daily-driver\n'
  printf 'name=%s\n' "$packet_name"
  printf 'repo=%s\n' "$repo_root"
  printf 'agent_root=%s\n' "$agent_root"
  printf 'origin_host=%s\n' "$origin_host"
  printf 'continuation_host=%s\n' "$continuation_host"
  printf 'third_host=%s\n' "$third_host"
  printf 'remote_prefix=%s\n' "$remote_prefix"
  printf 'dirty_status_count=%s\n' "$git_status_count"
  printf 'tcfs_mutation=0\n'
  printf 'ssh_mutation=0\n'
  printf 'live_repo_claim=0\n'
  printf 'daily_driver_claim=0\n'
} >"$evidence_dir/source.env"

{
  printf '# source git status\n'
  git -C "$repo_root" status --porcelain=v1 -b
  printf '\n# head\n'
  git -C "$repo_root" rev-parse HEAD
  printf '\n# branch\n'
  git -C "$repo_root" branch --show-current || true
  printf '\n# refs\n'
  git -C "$repo_root" show-ref --heads --tags || true
} >"$evidence_dir/git-source.txt"

write_tree_hashes "$repo_root" "$evidence_dir/tree-source.sha256" repo
write_tree_hashes "$agent_root" "$evidence_dir/agent-source.sha256" agent
write_symlinks "$repo_root" "$evidence_dir/symlinks.txt"
: >"$evidence_dir/policy-deny.txt"
write_policy_scan "$repo_root" repo "$evidence_dir/policy-deny.txt"
write_policy_scan "$agent_root" agent "$evidence_dir/policy-deny.txt"

cat >"$evidence_dir/gates.env" <<EOF
r0_policy_inventory=planned
r1_single_origin_dirty_wip=pending-live
r2_reverse_origin=pending-live
r3_unsync_cloud_only=pending-live
r4_git_bundle_restore=pending-live
r5_conflict_independent_edits=pending-live
r6_third_host=pending-third-host
EOF

cat >"$evidence_dir/result.env" <<EOF
status=plan-only
proof=git-roam-daily-driver-packet-shape
allowed_claim=packet-shape-only
live_tcfs_mutation=0
daily_driver_git_claim=0
EOF

cat >"$evidence_dir/README.md" <<EOF
# TCFS Git Roam Daily-Driver Packet

Status: plan-only.

Repo: \`$repo_root\`
Agent root: \`$agent_root\`
Origin host: \`$origin_host\`
Continuation host: \`$continuation_host\`
Third host: \`$third_host\`
Remote prefix: \`$remote_prefix\`

This packet records the evidence shape for the \`~/git\` daily-driver roam
story. It does not call TCFS, SSH, cargo, Nix, or mutate the repo.

Use this packet to wire the live R0-R6 gates from
\`docs/ops/git-roam-daily-driver-acceptance-2026-06-08.md\`.
EOF

printf 'git roam daily-driver evidence: %s\n' "$evidence_dir"
printf 'status: plan-only\n'
