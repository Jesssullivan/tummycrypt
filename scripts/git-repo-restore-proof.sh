#!/usr/bin/env bash
#
# Fresh-tree restore proof for a completed git-repo canary packet.
#
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/git-repo-restore-proof.sh --evidence-dir <path> [options]

Restore a pushed git-repo canary prefix into a fresh local tree using
`tcfs reconcile --execute`, then compare restored regular-file hashes and
symlink targets against the archived shadow.

Options:
  --evidence-dir <path>  Existing pushed canary evidence packet
  --restore-root <path>  Fresh restore root. Default: ~/TCFS Pilot/restore-proofs/<packet>-restore-<UTC>
  --restore-dir <path>   Evidence subdirectory. Default: <evidence>/restore-proof
  --config <path>        tcfs config. Default: <evidence>/state/tcfs-git-repo-canary.toml
  --tcfs-bin <path>      tcfs binary. Default: TCFS_BIN, packet run-metadata tcfs_command, or tcfs
  --state <path>         Restore state JSON. Default: <evidence>/restore-state.json
  --reconcile-timeout-secs <n>
                         Bound each reconcile command. Default: 900; 0 disables timeout
  --require-empty-dirs   Fail if empty directories are not restored exactly
  -h, --help             Show this help

Environment mirrors:
  EVIDENCE_DIR
  RESTORE_ROOT
  RESTORE_PROOF_DIR
  TCFS_RESTORE_CONFIG
  TCFS_BIN
  TCFS_STATE_PATH
  RESTORE_RECONCILE_TIMEOUT_SECS
  REQUIRE_EMPTY_DIRS=1
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

read_kv() {
  local file="$1"
  local key="$2"

  [[ -f "$file" ]] || return 1
  awk -v key="$key" 'index($0, key "=") == 1 { sub("^[^=]*=", ""); print; found=1; exit } END { exit found ? 0 : 1 }' "$file"
}

toml_string() {
  local file="$1"
  local key="$2"

  [[ -f "$file" ]] || return 1
  awk -v key="$key" '
    $1 == key && $2 == "=" {
      sub("^[^=]*= *\"", "")
      sub("\"[[:space:]]*$", "")
      print
      found=1
      exit
    }
    END { exit found ? 0 : 1 }
  ' "$file"
}

sha256_file() {
  local path="$1"

  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$path" | awk '{print $1}'
  else
    shasum -a 256 "$path" | awk '{print $1}'
  fi
}

write_regular_manifest() {
  local root="$1"
  local out="$2"

  (
    cd "$root"
    LC_ALL=C find . -type f -print | LC_ALL=C sort | while IFS= read -r path; do
      local rel="${path#./}"
      local hash
      hash="$(sha256_file "$root/$rel")"
      printf '%s\t%s\n' "$hash" "$rel"
    done
  ) >"$out"
}

write_symlink_manifest() {
  local root="$1"
  local out="$2"

  (
    cd "$root"
    LC_ALL=C find . -type l -print | LC_ALL=C sort | while IFS= read -r path; do
      local rel="${path#./}"
      local target
      target="$(readlink "$root/$rel")" || target="__TCFS_READLINK_FAILED__"
      printf '%s\t%s\n' "$rel" "$target"
    done
  ) >"$out"
}

write_empty_dir_manifest() {
  local root="$1"
  local out="$2"

  (
    cd "$root"
    LC_ALL=C find . -type d -empty -print | LC_ALL=C sort | while IFS= read -r path; do
      local rel="${path#./}"
      [[ "$rel" == "." ]] && continue
      printf '%s\n' "$rel"
    done
  ) >"$out"
}

write_unsupported_manifest() {
  local root="$1"
  local out="$2"

  (
    cd "$root"
    LC_ALL=C find . ! -type f ! -type d ! -type l -print | LC_ALL=C sort | while IFS= read -r path; do
      printf '%s\n' "${path#./}"
    done
  ) >"$out"
}

count_lines() {
  local file="$1"
  wc -l <"$file" | tr -d ' '
}

write_state_manifest() {
  local state_file="$1"
  local root="$2"
  local out="$3"

  [[ -f "$state_file" ]] || return 1
  command -v jq >/dev/null 2>&1 || return 1

  local root_prefix="${root%/}/"
  jq -r --arg root "$root_prefix" '
    ($root | length) as $root_len
    | (.entries // {})
    | to_entries[]
    | select(.key | startswith($root))
    | [(.key[$root_len:]), (.value.status // ""), (.value.device_id // ""), (.value.remote_path // "")]
    | @tsv
  ' "$state_file" | LC_ALL=C sort >"$out"
}

count_symlink_state_matches() {
  local symlinks_file="$1"
  local state_manifest="$2"
  local count=0

  while IFS=$'\t' read -r rel _target; do
    [[ -n "$rel" ]] || continue
    if awk -F '\t' -v rel="$rel" '$1 == rel && $2 == "synced" { found = 1 } END { exit found ? 0 : 1 }' "$state_manifest"; then
      count=$((count + 1))
    fi
  done <"$symlinks_file"

  printf '%s\n' "$count"
}

timestamp="$(date -u +%Y%m%dT%H%M%SZ)"

evidence_dir="${EVIDENCE_DIR:-}"
restore_root="${RESTORE_ROOT:-}"
restore_dir="${RESTORE_PROOF_DIR:-}"
config_path="${TCFS_RESTORE_CONFIG:-}"
tcfs_bin="${TCFS_BIN:-}"
state_path="${TCFS_STATE_PATH:-}"
reconcile_timeout_secs="${RESTORE_RECONCILE_TIMEOUT_SECS:-900}"
require_empty_dirs="$(bool_env REQUIRE_EMPTY_DIRS "${REQUIRE_EMPTY_DIRS:-0}")"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --evidence-dir)
      [[ $# -ge 2 ]] || fail "--evidence-dir requires a value"
      evidence_dir="$2"
      shift 2
      ;;
    --restore-root)
      [[ $# -ge 2 ]] || fail "--restore-root requires a value"
      restore_root="$2"
      shift 2
      ;;
    --restore-dir)
      [[ $# -ge 2 ]] || fail "--restore-dir requires a value"
      restore_dir="$2"
      shift 2
      ;;
    --config)
      [[ $# -ge 2 ]] || fail "--config requires a value"
      config_path="$2"
      shift 2
      ;;
    --tcfs-bin)
      [[ $# -ge 2 ]] || fail "--tcfs-bin requires a value"
      tcfs_bin="$2"
      shift 2
      ;;
    --state)
      [[ $# -ge 2 ]] || fail "--state requires a value"
      state_path="$2"
      shift 2
      ;;
    --reconcile-timeout-secs)
      [[ $# -ge 2 ]] || fail "--reconcile-timeout-secs requires a value"
      reconcile_timeout_secs="$2"
      shift 2
      ;;
    --require-empty-dirs)
      require_empty_dirs=1
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

[[ -n "$evidence_dir" ]] || fail "--evidence-dir is required"
[[ -d "$evidence_dir" ]] || fail "evidence dir does not exist: $evidence_dir"
evidence_dir="$(cd "$evidence_dir" && pwd -P)"

policy_path="$evidence_dir/git-repo-canary-policy.env"
run_metadata="$evidence_dir/run-metadata.env"
[[ -f "$policy_path" ]] || fail "missing git-repo-canary-policy.env in $evidence_dir"

shadow_root="$(read_kv "$policy_path" shadow || read_kv "$run_metadata" shadow || true)"
[[ -n "$shadow_root" ]] || fail "could not determine shadow root from packet"
[[ -d "$shadow_root" ]] || fail "shadow root does not exist: $shadow_root"

if [[ -z "$config_path" ]]; then
  config_path="$evidence_dir/state/tcfs-git-repo-canary.toml"
  [[ -f "$config_path" ]] || config_path="$evidence_dir/state/tcfs-linux-xr-shadow.toml"
fi
[[ -f "$config_path" ]] || fail "config does not exist: $config_path"

remote_prefix="$(read_kv "$run_metadata" remote_prefix || toml_string "$config_path" remote_prefix || true)"
[[ -n "$remote_prefix" ]] || fail "could not determine remote_prefix from run metadata or config"

if [[ -z "$tcfs_bin" ]]; then
  tcfs_bin="$(read_kv "$run_metadata" tcfs_command || true)"
fi
if [[ -z "$tcfs_bin" ]]; then
  tcfs_bin="tcfs"
fi

packet_name="$(basename "$evidence_dir")"
if [[ -z "$restore_root" ]]; then
  restore_root="$HOME/TCFS Pilot/restore-proofs/${packet_name}-restore-${timestamp}"
fi
if [[ -e "$restore_root" ]]; then
  if [[ ! -d "$restore_root" ]]; then
    fail "restore root exists and is not a directory: $restore_root"
  fi
  if find "$restore_root" -mindepth 1 -maxdepth 1 | read -r _; then
    fail "restore root must be empty: $restore_root"
  fi
else
  mkdir -p "$restore_root"
fi
restore_root="$(cd "$restore_root" && pwd -P)"

if [[ -z "$state_path" ]]; then
  state_path="$evidence_dir/restore-state.json"
fi
mkdir -p "$(dirname "$state_path")"

if [[ -z "$restore_dir" ]]; then
  restore_dir="$evidence_dir/restore-proof"
fi
mkdir -p "$restore_dir"
restore_dir="$(cd "$restore_dir" && pwd -P)"

tcfs_version_status=ok
tcfs_version="$("$tcfs_bin" --version 2>&1)" || tcfs_version_status=failed
tcfs_sha256_status=ok
tcfs_sha256="$(sha256_file "$tcfs_bin" 2>/dev/null)" || tcfs_sha256_status=failed

run_with_timeout() {
  local log_path="$1"
  shift

  local rc=0
  if [[ "$reconcile_timeout_secs" == "0" ]]; then
    "$@" >"$log_path" 2>&1 || rc=$?
  else
    local timeout_bin
    timeout_bin="$(command -v timeout || command -v gtimeout || true)"
    [[ -n "$timeout_bin" ]] || fail "timeout command unavailable; rerun with --reconcile-timeout-secs 0 to disable"
    "$timeout_bin" "${reconcile_timeout_secs}s" "$@" >"$log_path" 2>&1 || rc=$?
  fi
  return "$rc"
}

write_blocked_result() {
  local phase="$1"
  local rc="$2"
  local reason="$3"

  cat >"$restore_dir/restore-proof.env" <<EOF
created_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
status=failed
proof=fresh-tree-restore-blocked
reason=$reason
blocked_phase=$phase
blocked_rc=$rc
evidence_dir=$evidence_dir
shadow_root=$shadow_root
restore_root=$restore_root
config_path=$config_path
remote_prefix=$remote_prefix
state_path=$state_path
tcfs_command=$tcfs_bin
tcfs_version_status=$tcfs_version_status
tcfs_version=$tcfs_version
tcfs_sha256_status=$tcfs_sha256_status
tcfs_sha256=$tcfs_sha256
reconcile_timeout_secs=$reconcile_timeout_secs
regular_files_match=0
symlink_targets_match=0
empty_dirs_match=0
unsupported_special_files_match=0
EOF

  cat >"$restore_dir/README.md" <<EOF
# TCFS Git Repo Fresh-Tree Restore Proof

Created: $(date -u +%Y-%m-%dT%H:%M:%SZ)

Status: \`failed\`

Proof: \`fresh-tree-restore-blocked\`

Reason: $reason

- Packet: \`$evidence_dir\`
- Shadow: \`$shadow_root\`
- Restore root: \`$restore_root\`
- Config: \`$config_path\`
- Remote prefix: \`$remote_prefix\`
- tcfs: \`$tcfs_bin\`
- Timeout seconds: \`$reconcile_timeout_secs\`

Inspect \`reconcile-dry-run.log\` and \`reconcile-execute.log\` when present.
EOF
}

dry_run_rc=0
run_with_timeout "$restore_dir/reconcile-dry-run.log" \
  "$tcfs_bin" --config "$config_path" reconcile \
  --path "$restore_root" \
  --prefix "$remote_prefix" \
  --state "$state_path" || dry_run_rc=$?

if [[ "$dry_run_rc" -ne 0 ]]; then
  dry_reason="reconcile dry-run failed with rc=$dry_run_rc"
  if [[ "$dry_run_rc" -eq 124 ]]; then
    dry_reason="reconcile dry-run timed out after ${reconcile_timeout_secs}s"
  fi
  write_blocked_result dry-run "$dry_run_rc" "$dry_reason"
  printf 'restore proof evidence: %s\n' "$restore_dir"
  printf 'restore proof status: failed\n'
  printf 'restore proof reason: %s\n' "$dry_reason"
  exit "$dry_run_rc"
fi

execute_rc=0
run_with_timeout "$restore_dir/reconcile-execute.log" \
  "$tcfs_bin" --config "$config_path" reconcile \
  --path "$restore_root" \
  --prefix "$remote_prefix" \
  --state "$state_path" \
  --execute || execute_rc=$?

if [[ "$execute_rc" -ne 0 ]]; then
  execute_reason="reconcile execute failed with rc=$execute_rc"
  if [[ "$execute_rc" -eq 124 ]]; then
    execute_reason="reconcile execute timed out after ${reconcile_timeout_secs}s"
  fi
  write_blocked_result execute "$execute_rc" "$execute_reason"
  printf 'restore proof evidence: %s\n' "$restore_dir"
  printf 'restore proof status: failed\n'
  printf 'restore proof reason: %s\n' "$execute_reason"
  exit "$execute_rc"
fi

write_regular_manifest "$shadow_root" "$restore_dir/shadow-regular-sha256.tsv"
write_regular_manifest "$restore_root" "$restore_dir/restored-regular-sha256.tsv"
write_symlink_manifest "$shadow_root" "$restore_dir/shadow-symlink-targets.tsv"
write_symlink_manifest "$restore_root" "$restore_dir/restored-symlink-targets.tsv"
write_empty_dir_manifest "$shadow_root" "$restore_dir/shadow-empty-dirs.txt"
write_empty_dir_manifest "$restore_root" "$restore_dir/restored-empty-dirs.txt"
write_unsupported_manifest "$shadow_root" "$restore_dir/shadow-unsupported-special-files.txt"
write_unsupported_manifest "$restore_root" "$restore_dir/restored-unsupported-special-files.txt"

state_manifest_status=unavailable
state_entry_count=0
restored_symlink_state_count=0
if write_state_manifest "$state_path" "$restore_root" "$restore_dir/restored-state.tsv"; then
  state_manifest_status=ok
  state_entry_count="$(count_lines "$restore_dir/restored-state.tsv")"
  restored_symlink_state_count="$(count_symlink_state_matches "$restore_dir/restored-symlink-targets.tsv" "$restore_dir/restored-state.tsv")"
fi

regular_diff_rc=0
diff -u "$restore_dir/shadow-regular-sha256.tsv" "$restore_dir/restored-regular-sha256.tsv" \
  >"$restore_dir/regular-files.diff" || regular_diff_rc=$?

symlink_diff_rc=0
diff -u "$restore_dir/shadow-symlink-targets.tsv" "$restore_dir/restored-symlink-targets.tsv" \
  >"$restore_dir/symlinks.diff" || symlink_diff_rc=$?

empty_dir_diff_rc=0
diff -u "$restore_dir/shadow-empty-dirs.txt" "$restore_dir/restored-empty-dirs.txt" \
  >"$restore_dir/empty-dirs.diff" || empty_dir_diff_rc=$?

unsupported_diff_rc=0
diff -u "$restore_dir/shadow-unsupported-special-files.txt" "$restore_dir/restored-unsupported-special-files.txt" \
  >"$restore_dir/unsupported-special-files.diff" || unsupported_diff_rc=$?

shadow_regular_count="$(count_lines "$restore_dir/shadow-regular-sha256.tsv")"
restored_regular_count="$(count_lines "$restore_dir/restored-regular-sha256.tsv")"
shadow_symlink_count="$(count_lines "$restore_dir/shadow-symlink-targets.tsv")"
restored_symlink_count="$(count_lines "$restore_dir/restored-symlink-targets.tsv")"
shadow_empty_dir_count="$(count_lines "$restore_dir/shadow-empty-dirs.txt")"
restored_empty_dir_count="$(count_lines "$restore_dir/restored-empty-dirs.txt")"
shadow_unsupported_count="$(count_lines "$restore_dir/shadow-unsupported-special-files.txt")"
restored_unsupported_count="$(count_lines "$restore_dir/restored-unsupported-special-files.txt")"

regular_files_match=0
symlink_targets_match=0
empty_dirs_match=0
unsupported_special_files_match=0
[[ "$regular_diff_rc" -eq 0 ]] && regular_files_match=1
[[ "$symlink_diff_rc" -eq 0 ]] && symlink_targets_match=1
[[ "$empty_dir_diff_rc" -eq 0 ]] && empty_dirs_match=1
[[ "$unsupported_diff_rc" -eq 0 ]] && unsupported_special_files_match=1

status=passed
proof=fresh-tree-restore-files-symlinks-empty-dirs
reason="regular files, symlink targets, and empty directories restored exactly"

if [[ "$regular_files_match" != "1" ]]; then
  status=failed
  reason="regular file hash manifest mismatch"
elif [[ "$symlink_targets_match" != "1" ]]; then
  status=failed
  reason="symlink target manifest mismatch"
elif [[ "$shadow_unsupported_count" != "0" || "$restored_unsupported_count" != "0" || "$unsupported_special_files_match" != "1" ]]; then
  status=failed
  reason="unsupported special file mismatch"
elif [[ "$require_empty_dirs" == "1" && "$empty_dirs_match" != "1" ]]; then
  status=failed
  reason="empty directory manifest mismatch"
elif [[ "$empty_dirs_match" != "1" ]]; then
  proof=fresh-tree-restore-files-and-symlinks-empty-dirs-gap
  reason="regular files and symlink targets restored exactly; empty directories are not restored by reconcile"
fi

cat >"$restore_dir/restore-proof.env" <<EOF
created_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
status=$status
proof=$proof
reason=$reason
evidence_dir=$evidence_dir
shadow_root=$shadow_root
restore_root=$restore_root
config_path=$config_path
remote_prefix=$remote_prefix
state_path=$state_path
tcfs_command=$tcfs_bin
tcfs_version_status=$tcfs_version_status
tcfs_version=$tcfs_version
tcfs_sha256_status=$tcfs_sha256_status
tcfs_sha256=$tcfs_sha256
regular_files_match=$regular_files_match
shadow_regular_file_count=$shadow_regular_count
restored_regular_file_count=$restored_regular_count
symlink_targets_match=$symlink_targets_match
shadow_symlink_count=$shadow_symlink_count
restored_symlink_count=$restored_symlink_count
state_manifest_status=$state_manifest_status
state_entry_count=$state_entry_count
restored_symlink_state_count=$restored_symlink_state_count
empty_dirs_match=$empty_dirs_match
shadow_empty_dir_count=$shadow_empty_dir_count
restored_empty_dir_count=$restored_empty_dir_count
require_empty_dirs=$require_empty_dirs
unsupported_special_files_match=$unsupported_special_files_match
shadow_unsupported_special_file_count=$shadow_unsupported_count
restored_unsupported_special_file_count=$restored_unsupported_count
EOF

cat >"$restore_dir/README.md" <<EOF
# TCFS Git Repo Fresh-Tree Restore Proof

Created: $(date -u +%Y-%m-%dT%H:%M:%SZ)

This proof restores an already-pushed git-repo canary prefix into a fresh local
tree with \`tcfs reconcile --execute\`, then compares restored regular-file
SHA-256 hashes and symlink targets against the archived shadow tree.

- Packet: \`$evidence_dir\`
- Shadow: \`$shadow_root\`
- Restore root: \`$restore_root\`
- Config: \`$config_path\`
- Remote prefix: \`$remote_prefix\`
- tcfs: \`$tcfs_bin\`
- Status: \`$status\`
- Proof: \`$proof\`
- Reason: \`$reason\`

Files:

- \`restore-proof.env\`: machine-readable result
- \`reconcile-dry-run.log\`: restore plan before mutation
- \`reconcile-execute.log\`: restore execution transcript
- \`shadow-regular-sha256.tsv\` / \`restored-regular-sha256.tsv\`: regular-file
  hash manifests
- \`shadow-symlink-targets.tsv\` / \`restored-symlink-targets.tsv\`: symlink
  target manifests
- \`restored-state.tsv\`: restored sync-state entries when the state JSON and
  \`jq\` are available
- \`shadow-empty-dirs.txt\` / \`restored-empty-dirs.txt\`: recorded empty-dir
  manifests. Empty directories are a known separate gate unless
  \`--require-empty-dirs\` is used.
EOF

printf 'restore proof evidence: %s\n' "$restore_dir"
printf 'restore proof status: %s\n' "$status"
printf 'restore proof reason: %s\n' "$reason"

if [[ "$status" != "passed" ]]; then
  exit 1
fi
