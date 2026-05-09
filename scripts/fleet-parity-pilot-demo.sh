#!/usr/bin/env bash
#
# Prepare or run the isolated neo/honey TCFS fleet parity pilot packet.
#
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/fleet-parity-pilot-demo.sh [options]

Create an isolated fleet-pilot tree, seed plan/run artifacts, and optionally
delegate to the existing honey lazy-hydration helper. This script never targets
real ~/Documents or ~/git by default.

Options:
  --remote <seaweedfs://host:port/bucket/prefix>
      Remote prefix to use. Defaults to a timestamped fleet-parity prefix.
  --pilot-root <path>
      Local isolated pilot root. Defaults to "$HOME/TCFS Pilot/runs/<run-id>".
  --evidence-dir <path>
      Evidence output directory. Defaults to a temp directory.
  --state-dir <path>
      Local helper state directory. Defaults to <evidence-dir>/desktop-state.
  --push
      Push the pilot root to the remote prefix.
  --create-bucket
      Best-effort remote bucket creation before pushing.
  --tcfs-bin <path>
      Local tcfs binary for push. Passed through to the lazy helper.
  --honey-host <host>
      SSH host label for honey. Default: honey.
  --honey-mount-root <path>
      Honey mountpoint. Default: ~/tcfs-pilot/<run-id>.
  --honey-remote-dir <path>
      Honey temp directory. Default: /tmp/tcfs-fleet-pilot-<run-id>.
  --honey-tcfs-bin <path>
      Remote tcfs binary path on honey. Default: tcfs.
  --run-honey
      Copy helper scripts to honey and run the mounted smoke there.
  --honey-start-mount
      With --run-honey, start tcfs mount on honey.
  --honey-existing-mount
      With --run-honey, assume the honey mountpoint is already mounted.
  --forward-aws-env
      With --run-honey, forward current AWS env to honey for this smoke run.
  --run-neo-honey
      Also run `just neo-honey-smoke` locally and archive the transcript.
  --run-linux-lifecycle
      Also run the Linux lifecycle proof helper on honey over SSH and archive
      its evidence under linux-lifecycle/.
  --linux-lifecycle-remote <seaweedfs://host:port/bucket/prefix>
      Remote prefix for --run-linux-lifecycle. Defaults to <remote>/linux-lifecycle.
  --allow-real-roots
      Allow direct HOME, ~/Documents, or ~/git pilot roots. Not recommended.
  -h, --help
      Show this help.

Environment mirrors:
  TCFS_FLEET_PILOT_REMOTE
  TCFS_FLEET_PILOT_ROOT
  TCFS_FLEET_PILOT_EVIDENCE_DIR
  TCFS_FLEET_PILOT_STATE_DIR
  TCFS_FLEET_PILOT_PUSH=1
  TCFS_FLEET_PILOT_CREATE_BUCKET=1
  TCFS_FLEET_PILOT_RUN_HONEY=1
  TCFS_FLEET_PILOT_RUN_NEO_HONEY=1
  TCFS_FLEET_PILOT_RUN_LINUX_LIFECYCLE=1
  TCFS_FLEET_PILOT_LINUX_LIFECYCLE_REMOTE
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
run_id="fleet-parity-${timestamp}-$$"

remote="${TCFS_FLEET_PILOT_REMOTE:-seaweedfs://localhost:8333/tcfs/${run_id}}"
pilot_root="${TCFS_FLEET_PILOT_ROOT:-$HOME/TCFS Pilot/runs/${run_id}}"
evidence_dir="${TCFS_FLEET_PILOT_EVIDENCE_DIR:-${TMPDIR:-/tmp}/tcfs-${run_id}}"
state_dir="${TCFS_FLEET_PILOT_STATE_DIR:-}"
push_remote="$(bool_env TCFS_FLEET_PILOT_PUSH "${TCFS_FLEET_PILOT_PUSH:-0}")"
create_bucket="$(bool_env TCFS_FLEET_PILOT_CREATE_BUCKET "${TCFS_FLEET_PILOT_CREATE_BUCKET:-0}")"
run_honey="$(bool_env TCFS_FLEET_PILOT_RUN_HONEY "${TCFS_FLEET_PILOT_RUN_HONEY:-0}")"
run_neo_honey="$(bool_env TCFS_FLEET_PILOT_RUN_NEO_HONEY "${TCFS_FLEET_PILOT_RUN_NEO_HONEY:-0}")"
run_linux_lifecycle="$(bool_env TCFS_FLEET_PILOT_RUN_LINUX_LIFECYCLE "${TCFS_FLEET_PILOT_RUN_LINUX_LIFECYCLE:-0}")"
tcfs_bin="${TCFS_BIN:-}"
honey_host="${TCFS_HONEY_HOST:-honey}"
honey_mount_root="${TCFS_HONEY_MOUNT_ROOT:-~/tcfs-pilot/${run_id}}"
honey_remote_dir="${TCFS_HONEY_REMOTE_DIR:-/tmp/tcfs-${run_id}}"
honey_tcfs_bin="${TCFS_HONEY_TCFS_BIN:-tcfs}"
honey_start_mount="$(bool_env TCFS_HONEY_START_MOUNT "${TCFS_HONEY_START_MOUNT:-0}")"
forward_aws_env="$(bool_env TCFS_HONEY_FORWARD_AWS_ENV "${TCFS_HONEY_FORWARD_AWS_ENV:-0}")"
allow_real_roots=0
linux_lifecycle_remote="${TCFS_FLEET_PILOT_LINUX_LIFECYCLE_REMOTE:-}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --remote)
      [[ $# -ge 2 ]] || fail "--remote requires a value"
      remote="$2"
      shift 2
      ;;
    --pilot-root)
      [[ $# -ge 2 ]] || fail "--pilot-root requires a value"
      pilot_root="$2"
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
    --run-neo-honey)
      run_neo_honey=1
      shift
      ;;
    --run-linux-lifecycle)
      run_linux_lifecycle=1
      shift
      ;;
    --linux-lifecycle-remote)
      [[ $# -ge 2 ]] || fail "--linux-lifecycle-remote requires a value"
      linux_lifecycle_remote="$2"
      shift 2
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

if [[ -z "$linux_lifecycle_remote" ]]; then
  linux_lifecycle_remote="${remote%/}/linux-lifecycle"
fi

[[ "$remote" == seaweedfs://* ]] || fail "remote must start with seaweedfs://"
[[ "$linux_lifecycle_remote" == seaweedfs://* ]] || fail "linux lifecycle remote must start with seaweedfs://"
case "$honey_remote_dir" in
  *[[:space:]]*) fail "--honey-remote-dir must not contain whitespace: $honey_remote_dir" ;;
esac
if ! [[ "$honey_remote_dir" =~ ^[A-Za-z0-9_./@%+=:-]+$ ]]; then
  fail "--honey-remote-dir contains unsafe shell characters: $honey_remote_dir"
fi

pilot_canon="$(make_physical_dir "$pilot_root")"
home_canon="$(canonical_path "$HOME")"
documents_canon="$(canonical_path "$HOME/Documents")"
git_canon="$(canonical_path "$HOME/git")"

if [[ "$allow_real_roots" != "1" ]]; then
  [[ "$pilot_canon" != "/" ]] || fail "refusing to use filesystem root as pilot root"
  [[ "$pilot_canon" != "$home_canon" ]] || fail "refusing to use HOME as pilot root"
  [[ "$pilot_canon" != "$documents_canon" ]] || fail "refusing to use real Documents as pilot root"
  [[ "$pilot_canon" != "$git_canon" ]] || fail "refusing to use real git as pilot root"
fi

documents_file="Documents/fleet-readiness.md"
git_file="git/tcfs-pilot-repo/README.md"
expected_documents_content="$evidence_dir/fleet-documents-expected.txt"
pilot_tree="$evidence_dir/fleet-pilot-tree.txt"
desktop_evidence="$evidence_dir/desktop-honey"
honey_fleet_script="$evidence_dir/honey-fleet-run.sh"
honey_fleet_commands="$evidence_dir/honey-fleet-commands.txt"
honey_fleet_log="$evidence_dir/honey-fleet-run.log"
neo_honey_log="$evidence_dir/neo-honey-smoke.log"
neo_honey_status="$evidence_dir/neo-honey-status.env"
linux_lifecycle_evidence="$evidence_dir/linux-lifecycle"
honey_linux_lifecycle_script="$evidence_dir/honey-linux-lifecycle-run.sh"
honey_linux_lifecycle_commands="$evidence_dir/honey-linux-lifecycle-commands.txt"
honey_linux_lifecycle_log="$evidence_dir/honey-linux-lifecycle.log"
linux_lifecycle_status="$evidence_dir/linux-lifecycle-status.env"
honey_linux_lifecycle_dir="$honey_remote_dir/linux-lifecycle"
honey_linux_lifecycle_scripts_dir="$honey_remote_dir/scripts"
honey_linux_lifecycle_evidence_dir="$honey_linux_lifecycle_dir/evidence"

mkdir -p "$evidence_dir"
if [[ -z "$state_dir" ]]; then
  state_dir="$evidence_dir/desktop-state"
fi

mkdir -p \
  "$pilot_canon/Documents" \
  "$pilot_canon/git/tcfs-pilot-repo/src" \
  "$pilot_canon/git/tcfs-pilot-repo/.git/refs/heads"

cat >"$pilot_canon/$documents_file" <<'EOF'
# TCFS Fleet Pilot

This isolated Documents fixture should traverse remotely without hydrating the
whole tree. It is safe to delete after the proof packet is archived.
EOF

cat >"$pilot_canon/$git_file" <<'EOF'
# tcfs-pilot-repo

Small project fixture for fleet parity traversal and hydration checks.
EOF

cat >"$pilot_canon/git/tcfs-pilot-repo/src/main.rs" <<'EOF'
fn main() {
    println!("tcfs fleet pilot fixture");
}
EOF

cat >"$pilot_canon/git/tcfs-pilot-repo/.git/HEAD" <<'EOF'
ref: refs/heads/main
EOF

cat >"$pilot_canon/git/tcfs-pilot-repo/.git/config" <<'EOF'
[core]
	repositoryformatversion = 0
	filemode = true
	bare = false
EOF

cp "$pilot_canon/$documents_file" "$expected_documents_content"
find "$pilot_canon" -maxdepth 8 -print | sort >"$pilot_tree"

desktop_args=(
  --remote "$remote"
  --desktop-root "$pilot_canon"
  --evidence-dir "$desktop_evidence"
  --honey-host "$honey_host"
  --honey-mount-root "$honey_mount_root"
  --honey-remote-dir "$honey_remote_dir"
  --honey-tcfs-bin "$honey_tcfs_bin"
)
if [[ -n "$tcfs_bin" ]]; then
  desktop_args+=(--tcfs-bin "$tcfs_bin")
fi
if [[ "$push_remote" == "1" ]]; then
  desktop_args+=(--push)
fi
if [[ "$create_bucket" == "1" ]]; then
  desktop_args+=(--create-bucket)
fi
if [[ "$run_honey" == "1" ]]; then
  desktop_args+=(--run-honey)
fi
if [[ "$honey_start_mount" == "1" ]]; then
  desktop_args+=(--honey-start-mount)
else
  desktop_args+=(--honey-existing-mount)
fi
if [[ "$forward_aws_env" == "1" ]]; then
  desktop_args+=(--forward-aws-env)
fi

TCFS_DESKTOP_DEMO_STATE_DIR="$state_dir" \
  bash "$REPO_ROOT/scripts/lazy-hydration-desktop-honey-demo.sh" "${desktop_args[@]}"

cat >"$honey_fleet_script" <<EOF
#!/usr/bin/env bash
set -euo pipefail

MOUNT_ROOT_RAW=$(single_quote "$honey_mount_root")
case "\$MOUNT_ROOT_RAW" in
  "~/"*) MOUNT_ROOT="\${HOME}/\${MOUNT_ROOT_RAW#\\~/}" ;;
  *) MOUNT_ROOT="\$MOUNT_ROOT_RAW" ;;
esac
SMOKE_SCRIPT="\${TCFS_HONEY_SMOKE_SCRIPT:-$(shell_quote "$honey_remote_dir/lazy-hydration-mounted-smoke.sh")}"
EXPECTED_CONTENT_FILE="\${TCFS_HONEY_FLEET_EXPECTED_CONTENT_FILE:-$(shell_quote "$honey_remote_dir/fleet-documents-expected.txt")}"

bash "\$SMOKE_SCRIPT" \\
  --mount-root "\$MOUNT_ROOT" \\
  --expected-file $(shell_quote "$documents_file") \\
  --expected-content-file "\$EXPECTED_CONTENT_FILE" \\
  --expect-entry Documents \\
  --expect-entry git \\
  --expect-entry git/tcfs-pilot-repo \\
  --max-depth 8
EOF
chmod +x "$honey_fleet_script"

cat >"$honey_fleet_commands" <<EOF
# Run this after the desktop-honey commands have mounted or verified honey.
scp $(shell_quote "$expected_documents_content") $(shell_quote "$honey_host"):$(shell_quote "$honey_remote_dir/fleet-documents-expected.txt")
scp $(shell_quote "$honey_fleet_script") $(shell_quote "$honey_host"):$(shell_quote "$honey_remote_dir/honey-fleet-run.sh")
ssh $(shell_quote "$honey_host") 'TCFS_HONEY_SMOKE_SCRIPT=$(shell_quote "$honey_remote_dir/lazy-hydration-mounted-smoke.sh") TCFS_HONEY_FLEET_EXPECTED_CONTENT_FILE=$(shell_quote "$honey_remote_dir/fleet-documents-expected.txt") bash $(shell_quote "$honey_remote_dir/honey-fleet-run.sh")'
EOF

cat >"$honey_linux_lifecycle_script" <<EOF
#!/usr/bin/env bash
set -euo pipefail

REMOTE=$(shell_quote "$linux_lifecycle_remote")
TCFS_BIN_RAW=$(shell_quote "$honey_tcfs_bin")
LIFECYCLE_SCRIPT=$(shell_quote "$honey_linux_lifecycle_dir/lazy-hydration-linux-lifecycle-demo.sh")
EVIDENCE_DIR=$(shell_quote "$honey_linux_lifecycle_evidence_dir")
CREATE_BUCKET=$(shell_quote "$create_bucket")

if [[ -n "\${TCFS_HONEY_ENV_FILE:-}" ]]; then
  # shellcheck disable=SC1090
  source "\$TCFS_HONEY_ENV_FILE"
fi

if [[ "\$TCFS_BIN_RAW" == */* ]]; then
  TCFS_BIN_RESOLVED="\$TCFS_BIN_RAW"
else
  TCFS_BIN_RESOLVED="\$(command -v "\$TCFS_BIN_RAW" || true)"
fi
if [[ -z "\$TCFS_BIN_RESOLVED" || ! -x "\$TCFS_BIN_RESOLVED" ]]; then
  echo "missing executable tcfs binary on honey: \$TCFS_BIN_RAW" >&2
  exit 1
fi

mkdir -p "\$(dirname "\$EVIDENCE_DIR")" "\$EVIDENCE_DIR"
args=(
  --remote "\$REMOTE"
  --evidence-dir "\$EVIDENCE_DIR"
  --tcfs-bin "\$TCFS_BIN_RESOLVED"
)
if [[ "\$CREATE_BUCKET" == "1" ]]; then
  args+=(--create-bucket)
fi

bash "\$LIFECYCLE_SCRIPT" "\${args[@]}"
EOF
chmod +x "$honey_linux_lifecycle_script"

cat >"$honey_linux_lifecycle_commands" <<EOF
# Optional Linux lifecycle companion. This proves mounted write/readback,
# cache clear/rehydrate, and recursive safe-unsync under a nested disposable
# prefix without changing the fleet traversal fixture.
ssh $(shell_quote "$honey_host") 'mkdir -p $(shell_quote "$honey_linux_lifecycle_dir")'
ssh $(shell_quote "$honey_host") 'mkdir -p $(shell_quote "$honey_linux_lifecycle_scripts_dir")'
scp $(shell_quote "$REPO_ROOT/scripts/lazy-hydration-linux-demo.sh") $(shell_quote "$honey_host"):$(shell_quote "$honey_linux_lifecycle_dir/lazy-hydration-linux-demo.sh")
scp $(shell_quote "$REPO_ROOT/scripts/lazy-hydration-linux-lifecycle-demo.sh") $(shell_quote "$honey_host"):$(shell_quote "$honey_linux_lifecycle_dir/lazy-hydration-linux-lifecycle-demo.sh")
scp $(shell_quote "$REPO_ROOT/scripts/lazy-hydration-mounted-smoke.sh") $(shell_quote "$honey_host"):$(shell_quote "$honey_linux_lifecycle_scripts_dir/lazy-hydration-mounted-smoke.sh")
scp $(shell_quote "$honey_linux_lifecycle_script") $(shell_quote "$honey_host"):$(shell_quote "$honey_linux_lifecycle_dir/honey-linux-lifecycle-run.sh")
ssh $(shell_quote "$honey_host") 'chmod +x $(shell_quote "$honey_linux_lifecycle_dir/lazy-hydration-linux-demo.sh") $(shell_quote "$honey_linux_lifecycle_dir/lazy-hydration-linux-lifecycle-demo.sh") $(shell_quote "$honey_linux_lifecycle_scripts_dir/lazy-hydration-mounted-smoke.sh") $(shell_quote "$honey_linux_lifecycle_dir/honey-linux-lifecycle-run.sh")'
ssh $(shell_quote "$honey_host") 'bash $(shell_quote "$honey_linux_lifecycle_dir/honey-linux-lifecycle-run.sh")'
mkdir -p $(shell_quote "$linux_lifecycle_evidence")
scp -r $(shell_quote "$honey_host"):$(shell_quote "$honey_linux_lifecycle_evidence_dir/.") $(shell_quote "$linux_lifecycle_evidence/")

# If honey does not already have AWS credentials, rerun this local helper with
# --run-linux-lifecycle --forward-aws-env. The generated command file never
# stores those credentials.
EOF

if [[ "$run_honey" == "1" ]]; then
  command -v ssh >/dev/null 2>&1 || fail "ssh not found"
  command -v scp >/dev/null 2>&1 || fail "scp not found"
  scp "$expected_documents_content" "$honey_host:$honey_remote_dir/fleet-documents-expected.txt"
  scp "$honey_fleet_script" "$honey_host:$honey_remote_dir/honey-fleet-run.sh"
  remote_cmd="$(printf 'TCFS_HONEY_SMOKE_SCRIPT=%q TCFS_HONEY_FLEET_EXPECTED_CONTENT_FILE=%q bash %q' \
    "$honey_remote_dir/lazy-hydration-mounted-smoke.sh" \
    "$honey_remote_dir/fleet-documents-expected.txt" \
    "$honey_remote_dir/honey-fleet-run.sh")"
  # shellcheck disable=SC2029
  ssh "$honey_host" "$remote_cmd" | tee "$honey_fleet_log"
fi

linux_lifecycle_rc=0
if [[ "$run_linux_lifecycle" == "1" ]]; then
  command -v ssh >/dev/null 2>&1 || fail "ssh not found"
  command -v scp >/dev/null 2>&1 || fail "scp not found"

  printf 'running Linux lifecycle companion on %s\n' "$honey_host"
  if [[ "$forward_aws_env" == "1" ]]; then
    [[ -n "${AWS_ACCESS_KEY_ID:-}" ]] || fail "--forward-aws-env requires AWS_ACCESS_KEY_ID"
    [[ -n "${AWS_SECRET_ACCESS_KEY:-}" ]] || fail "--forward-aws-env requires AWS_SECRET_ACCESS_KEY"
  fi

  # shellcheck disable=SC2029
  ssh "$honey_host" "mkdir -p $(shell_quote "$honey_linux_lifecycle_dir")"
  # shellcheck disable=SC2029
  ssh "$honey_host" "mkdir -p $(shell_quote "$honey_linux_lifecycle_scripts_dir")"
  scp "$REPO_ROOT/scripts/lazy-hydration-linux-demo.sh" "$honey_host:$honey_linux_lifecycle_dir/lazy-hydration-linux-demo.sh"
  scp "$REPO_ROOT/scripts/lazy-hydration-linux-lifecycle-demo.sh" "$honey_host:$honey_linux_lifecycle_dir/lazy-hydration-linux-lifecycle-demo.sh"
  scp "$REPO_ROOT/scripts/lazy-hydration-mounted-smoke.sh" "$honey_host:$honey_linux_lifecycle_scripts_dir/lazy-hydration-mounted-smoke.sh"
  scp "$honey_linux_lifecycle_script" "$honey_host:$honey_linux_lifecycle_dir/honey-linux-lifecycle-run.sh"
  # shellcheck disable=SC2029
  ssh "$honey_host" "chmod +x $(shell_quote "$honey_linux_lifecycle_dir/lazy-hydration-linux-demo.sh") $(shell_quote "$honey_linux_lifecycle_dir/lazy-hydration-linux-lifecycle-demo.sh") $(shell_quote "$honey_linux_lifecycle_scripts_dir/lazy-hydration-mounted-smoke.sh") $(shell_quote "$honey_linux_lifecycle_dir/honey-linux-lifecycle-run.sh")"

  remote_lifecycle_env_file=""
  cleanup_remote_lifecycle_env() {
    [[ -n "$remote_lifecycle_env_file" ]] || return 0
    # shellcheck disable=SC2029
    ssh "$honey_host" "rm -f $(shell_quote "$remote_lifecycle_env_file")" >/dev/null 2>&1 || true
    remote_lifecycle_env_file=""
  }

  if [[ "$forward_aws_env" == "1" ]]; then
    remote_lifecycle_env_file="$honey_linux_lifecycle_dir/aws-env.sh"
    aws_env_payload="$(printf 'export AWS_ACCESS_KEY_ID=%q\nexport AWS_SECRET_ACCESS_KEY=%q\n' "$AWS_ACCESS_KEY_ID" "$AWS_SECRET_ACCESS_KEY")"
    # shellcheck disable=SC2029
    ssh "$honey_host" "umask 077; cat > $(shell_quote "$remote_lifecycle_env_file")" <<<"$aws_env_payload"
  fi

  remote_lifecycle_cmd="$(printf 'bash %q' "$honey_linux_lifecycle_dir/honey-linux-lifecycle-run.sh")"
  if [[ -n "$remote_lifecycle_env_file" ]]; then
    remote_lifecycle_cmd="$(printf 'TCFS_HONEY_ENV_FILE=%q %s' "$remote_lifecycle_env_file" "$remote_lifecycle_cmd")"
  fi
  # shellcheck disable=SC2029
  ssh "$honey_host" "$remote_lifecycle_cmd" | tee "$honey_linux_lifecycle_log" || linux_lifecycle_rc=$?
  cleanup_remote_lifecycle_env

  if [[ "$linux_lifecycle_rc" -eq 0 ]]; then
    mkdir -p "$linux_lifecycle_evidence"
    scp -r "$honey_host:$honey_linux_lifecycle_evidence_dir/." "$linux_lifecycle_evidence/" || linux_lifecycle_rc=$?
  fi

  cat >"$linux_lifecycle_status" <<EOF
ran=1
status=$linux_lifecycle_rc
completed_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
remote=$linux_lifecycle_remote
evidence_dir=$linux_lifecycle_evidence
EOF
  if [[ "$linux_lifecycle_rc" -ne 0 ]]; then
    printf 'Linux lifecycle companion failed; see %s\n' "$honey_linux_lifecycle_log" >&2
    exit "$linux_lifecycle_rc"
  fi
else
  cat >"$linux_lifecycle_status" <<EOF
ran=0
status=skipped
completed_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
remote=$linux_lifecycle_remote
evidence_dir=$linux_lifecycle_evidence
EOF
fi

neo_honey_rc=0
if [[ "$run_neo_honey" == "1" ]]; then
  (cd "$REPO_ROOT" && just neo-honey-smoke) >"$neo_honey_log" 2>&1 || neo_honey_rc=$?
  cat >"$neo_honey_status" <<EOF
ran=1
status=$neo_honey_rc
completed_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
EOF
  if [[ "$neo_honey_rc" -ne 0 ]]; then
    printf 'neo-honey smoke failed; see %s\n' "$neo_honey_log" >&2
    exit "$neo_honey_rc"
  fi
else
  cat >"$neo_honey_status" <<EOF
ran=0
status=skipped
completed_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
EOF
fi

cat >"$evidence_dir/run-metadata.env" <<EOF
created_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
run_id=$run_id
remote=$remote
pilot_root=$pilot_canon
documents_file=$documents_file
git_file=$git_file
honey_host=$honey_host
honey_mount_root=$honey_mount_root
honey_remote_dir=$honey_remote_dir
state_dir=$state_dir
push=$push_remote
run_honey=$run_honey
honey_start_mount=$honey_start_mount
run_neo_honey=$run_neo_honey
run_linux_lifecycle=$run_linux_lifecycle
linux_lifecycle_remote=$linux_lifecycle_remote
allow_real_roots=$allow_real_roots
EOF

cat >"$evidence_dir/README.md" <<EOF
# TCFS Fleet Parity Pilot Evidence

Created: $(date -u +%Y-%m-%dT%H:%M:%SZ)

This bundle is an isolated fleet-pilot packet. It does not target real
\`~/Documents\` or \`~/git\` unless \`--allow-real-roots\` was explicitly used.

Remote:

\`\`\`text
$remote
\`\`\`

Pilot root:

\`\`\`text
$pilot_canon
\`\`\`

Contents:

- \`desktop-honey/\`: output from the existing desktop-to-honey lazy helper
- \`fleet-pilot-tree.txt\`: local isolated pilot tree
- \`fleet-documents-expected.txt\`: exact content for honey hydration smoke
- \`honey-fleet-commands.txt\`: extra honey commands for Documents/git traversal
- \`honey-fleet-run.log\`: extra honey smoke transcript, when run
- \`honey-linux-lifecycle-commands.txt\`: optional companion commands for
  honey-side mounted write/readback, cache rehydrate, and safe-unsync proof
- \`honey-linux-lifecycle.log\`: companion lifecycle transcript, when run
- \`linux-lifecycle/\`: companion lifecycle evidence copied back from honey,
  when run
- \`linux-lifecycle-status.env\`: whether the companion lifecycle ran
- \`neo-honey-smoke.log\`: live backend smoke transcript, when requested
- \`neo-honey-status.env\`: whether \`just neo-honey-smoke\` ran

Current proof boundary:

- plan-only bundles prove command shape and safe isolated fixture generation
- bundles with \`push=1\` prove remote seed command execution
- bundles with \`run_honey=1\` prove honey mounted traversal/hydration for the
  generated pilot fixture
- bundles with \`run_neo_honey=1\` also include live SeaweedFS/NATS sync proof
- bundles with \`run_linux_lifecycle=1\` also include a honey-side Linux
  lifecycle companion under a nested disposable prefix; this proves mounted
  write/readback, cache clear/rehydrate, and recursive safe-unsync, but it is
  still not a real \`~/Documents\` or \`~/git\` takeover
EOF

printf 'fleet pilot root: %s\n' "$pilot_canon"
printf 'remote prefix: %s\n' "$remote"
printf 'evidence dir: %s\n' "$evidence_dir"
printf 'desktop-honey evidence: %s\n' "$desktop_evidence"
printf 'honey fleet commands: %s\n' "$honey_fleet_commands"
printf 'honey linux lifecycle commands: %s\n' "$honey_linux_lifecycle_commands"
