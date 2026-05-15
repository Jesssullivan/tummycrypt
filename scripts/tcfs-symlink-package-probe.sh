#!/usr/bin/env bash
#
# Probe one or more tcfs binaries for sync_symlinks behavior.
#
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/tcfs-symlink-package-probe.sh [options]

Creates a tiny source tree containing target.txt and link.txt -> target.txt,
runs each candidate tcfs binary with sync_symlinks = true, and archives whether
the candidate preserved, skipped, or failed the symlink push.

Options:
  --candidate <label=path>  Candidate tcfs binary. Repeatable.
  --evidence-dir <path>    Evidence dir. Default: docs/release/evidence/tcfs-symlink-package-probe-<UTC>
  --endpoint <url>         S3 endpoint. Default: TCFS_SYMLINK_PROBE_ENDPOINT or http://100.64.48.53:8333
  --bucket <name>          S3 bucket. Default: TCFS_SYMLINK_PROBE_BUCKET or tcfs
  --prefix-base <prefix>   Remote prefix base. Default: tcfs-symlink-package-probe-<UTC>
  --strict                 Exit non-zero unless every candidate preserves symlinks.
  -h, --help               Show this help.

Environment:
  TCFS_SYMLINK_PROBE_ENDPOINT
  TCFS_SYMLINK_PROBE_BUCKET
  TCFS_SYMLINK_PROBE_PREFIX_BASE
  TCFS_SYMLINK_PROBE_EVIDENCE_DIR
  TCFS_SYMLINK_PROBE_STRICT=1
  TCFS_SYMLINK_PROBE_RUST_LOG

If no candidates are provided, the script probes executable local defaults:
Homebrew at /opt/homebrew/opt/tcfs/bin/tcfs and source-built
target/codex-verify/debug/tcfs.
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

sanitize_label() {
  local label="$1"
  label="$(printf '%s' "$label" | tr -c 'A-Za-z0-9_' '_')"
  label="${label##_}"
  label="${label%%_}"
  [[ -n "$label" ]] || label="candidate"
  printf '%s\n' "$label"
}

shell_quote() {
  printf '%q' "$1"
}

write_config() {
  local config_path="$1"
  local socket_path="$2"
  local state_path="$3"
  local sync_root="$4"
  local prefix="$5"

  cat >"$config_path" <<EOF
[daemon]
socket = "$socket_path"

[storage]
endpoint = "$endpoint"
region = "us-east-1"
bucket = "$bucket"
remote_prefix = "$prefix"
enforce_tls = false

[sync]
state_db = "$state_path"
sync_root = "$sync_root"
nats_url = "nats://localhost:4222"
nats_tls = false
sync_git_dirs = true
git_sync_mode = "raw"
sync_hidden_dirs = true
sync_symlinks = true
sync_empty_dirs = true

[crypto]
enabled = false
EOF
}

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
timestamp="$(date -u +%Y%m%dT%H%M%SZ)"

endpoint="${TCFS_SYMLINK_PROBE_ENDPOINT:-http://100.64.48.53:8333}"
bucket="${TCFS_SYMLINK_PROBE_BUCKET:-tcfs}"
prefix_base="${TCFS_SYMLINK_PROBE_PREFIX_BASE:-tcfs-symlink-package-probe-${timestamp}}"
evidence_dir="${TCFS_SYMLINK_PROBE_EVIDENCE_DIR:-$REPO_ROOT/docs/release/evidence/tcfs-symlink-package-probe-${timestamp}}"
strict="$(bool_env TCFS_SYMLINK_PROBE_STRICT "${TCFS_SYMLINK_PROBE_STRICT:-0}")"
rust_log="${TCFS_SYMLINK_PROBE_RUST_LOG:-tcfs_sync=debug,tcfs=info,tcfs_storage=warn,tcfs_secrets=warn}"

candidate_specs=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --candidate)
      [[ $# -ge 2 ]] || fail "--candidate requires a label=path value"
      candidate_specs+=("$2")
      shift 2
      ;;
    --evidence-dir)
      [[ $# -ge 2 ]] || fail "--evidence-dir requires a value"
      evidence_dir="$2"
      shift 2
      ;;
    --endpoint)
      [[ $# -ge 2 ]] || fail "--endpoint requires a value"
      endpoint="$2"
      shift 2
      ;;
    --bucket)
      [[ $# -ge 2 ]] || fail "--bucket requires a value"
      bucket="$2"
      shift 2
      ;;
    --prefix-base)
      [[ $# -ge 2 ]] || fail "--prefix-base requires a value"
      prefix_base="$2"
      shift 2
      ;;
    --strict)
      strict=1
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

if [[ "${#candidate_specs[@]}" -eq 0 ]]; then
  if [[ -x /opt/homebrew/opt/tcfs/bin/tcfs ]]; then
    candidate_specs+=("homebrew=/opt/homebrew/opt/tcfs/bin/tcfs")
  fi
  if [[ -x "$REPO_ROOT/target/codex-verify/debug/tcfs" ]]; then
    candidate_specs+=("source_built=$REPO_ROOT/target/codex-verify/debug/tcfs")
  fi
fi

[[ "${#candidate_specs[@]}" -gt 0 ]] || fail "no candidate tcfs binaries found; pass --candidate label=/path/to/tcfs"

mkdir -p "$evidence_dir"
evidence_dir="$(cd "$evidence_dir" && pwd -P)"

tmpdir="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-symlink-package-probe.XXXXXX")"
trap 'rm -rf "$tmpdir"' EXIT

fixture="$tmpdir/source"
mkdir -p "$fixture"
printf 'target\n' >"$fixture/target.txt"
ln -s target.txt "$fixture/link.txt"

{
  printf 'target.txt\tfile\n'
  printf 'link.txt\ttarget.txt\n'
} >"$evidence_dir/fixture.tsv"

result_env="$evidence_dir/result.env"
{
  printf 'created_at_utc=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  printf 'endpoint=%s\n' "$endpoint"
  printf 'bucket=%s\n' "$bucket"
  printf 'prefix_base=%s\n' "$prefix_base"
  printf 'sync_symlinks=true\n'
  printf 'candidate_count=%s\n' "${#candidate_specs[@]}"
  printf 'production_claim=0\n'
  printf 'finder_claim=0\n'
  printf 'home_takeover_claim=0\n'
} >"$result_env"

candidate_index=0
preserved_count=0
blocked_count=0
unknown_count=0

for spec in "${candidate_specs[@]}"; do
  [[ "$spec" == *=* ]] || fail "candidate must be label=path: $spec"
  raw_label="${spec%%=*}"
  bin_path="${spec#*=}"
  label="$(sanitize_label "$raw_label")"
  candidate_index=$((candidate_index + 1))

  [[ -x "$bin_path" ]] || fail "candidate is not executable: $bin_path"
  bin_canon="$(cd "$(dirname "$bin_path")" && pwd -P)/$(basename "$bin_path")"
  prefix="${prefix_base}-${label}"
  config_path="$evidence_dir/${label}.toml"
  log_path="$evidence_dir/${label}.log"
  version_path="$evidence_dir/${label}.version.txt"
  version_err_path="$evidence_dir/${label}.version.err"
  state_path="$tmpdir/state/${label}.db"
  socket_path="$tmpdir/no-daemon-${label}.sock"
  mkdir -p "$(dirname "$state_path")"

  write_config "$config_path" "$socket_path" "$state_path" "$fixture" "$prefix"

  version_status=0
  if "$bin_canon" --version >"$version_path" 2>"$version_err_path"; then
    version="$(tr '\n' ' ' <"$version_path" | sed 's/[[:space:]]*$//')"
  else
    version_status=$?
    version="version-command-failed"
  fi
  sha256="$(shasum -a 256 "$bin_canon" | awk '{ print $1 }')"

  push_rc=0
  set +e
  RUST_LOG="$rust_log" "$bin_canon" --config "$config_path" push "$fixture" >"$log_path" 2>&1
  push_rc=$?
  set -e

  symlink_result="unknown"
  if [[ "$push_rc" -ne 0 ]]; then
    symlink_result="push_failed"
    blocked_count=$((blocked_count + 1))
  elif grep -Fq "uploaded symlink" "$log_path"; then
    symlink_result="preserved"
    preserved_count=$((preserved_count + 1))
  elif grep -Fq "skipping symlink" "$log_path"; then
    symlink_result="skipped"
    blocked_count=$((blocked_count + 1))
  else
    unknown_count=$((unknown_count + 1))
  fi

  {
    printf 'candidate_%s_label=%s\n' "$candidate_index" "$label"
    printf 'candidate_%s_bin=%s\n' "$candidate_index" "$bin_canon"
    printf 'candidate_%s_version=%s\n' "$candidate_index" "$version"
    printf 'candidate_%s_version_status=%s\n' "$candidate_index" "$version_status"
    printf 'candidate_%s_sha256=%s\n' "$candidate_index" "$sha256"
    printf 'candidate_%s_prefix=%s\n' "$candidate_index" "$prefix"
    printf 'candidate_%s_push_rc=%s\n' "$candidate_index" "$push_rc"
    printf 'candidate_%s_symlink_result=%s\n' "$candidate_index" "$symlink_result"
    printf 'candidate_%s_config=%s\n' "$candidate_index" "$config_path"
    printf 'candidate_%s_log=%s\n' "$candidate_index" "$log_path"
    printf 'candidate_%s_label_safe=%s\n' "$candidate_index" "$label"
    printf '%s_bin=%s\n' "$label" "$bin_canon"
    printf '%s_version=%s\n' "$label" "$version"
    printf '%s_sha256=%s\n' "$label" "$sha256"
    printf '%s_prefix=%s\n' "$label" "$prefix"
    printf '%s_push_rc=%s\n' "$label" "$push_rc"
    printf '%s_symlink_result=%s\n' "$label" "$symlink_result"
  } >>"$result_env"
done

overall_status="passed"
if [[ "$blocked_count" -gt 0 || "$unknown_count" -gt 0 ]]; then
  overall_status="blocked"
fi

{
  printf 'preserved_count=%s\n' "$preserved_count"
  printf 'blocked_count=%s\n' "$blocked_count"
  printf 'unknown_count=%s\n' "$unknown_count"
  printf 'overall_status=%s\n' "$overall_status"
} >>"$result_env"

{
  printf '# TCFS Symlink Package Probe\n\n'
  printf "Created: \`%s\`\n\n" "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  printf "This packet probes candidate \`tcfs\` binaries with \`sync_symlinks = true\`\n"
  printf "against a tiny fixture containing \`target.txt\` and \`link.txt -> target.txt\`.\n\n"
  printf 'It is package/runtime drift evidence only. It does not claim production\n'
  printf 'readiness, Finder/FileProvider readiness, broad repo management, or home\n'
  printf 'directory takeover.\n\n'
  printf -- "- Endpoint: \`%s\`\n" "$endpoint"
  printf -- "- Bucket: \`%s\`\n" "$bucket"
  printf -- "- Prefix base: \`%s\`\n" "$prefix_base"
  printf -- "- Overall status: \`%s\`\n\n" "$overall_status"
  printf 'Candidate results:\n\n'
  for i in $(seq 1 "$candidate_index"); do
    label_line="$(awk -F= -v key="candidate_${i}_label" '$1 == key { print $2 }' "$result_env")"
    version_line="$(awk -F= -v key="candidate_${i}_version" '$1 == key { print $2 }' "$result_env")"
    result_line="$(awk -F= -v key="candidate_${i}_symlink_result" '$1 == key { print $2 }' "$result_env")"
    printf -- "- \`%s\`: \`%s\` (\`%s\`)\n" "$label_line" "$result_line" "$version_line"
  done
  printf '\nFiles:\n\n'
  printf -- "- \`result.env\`: machine-readable verdict, binary versions, and SHA-256s.\n"
  printf -- "- \`fixture.tsv\`: fixture shape and expected symlink target.\n"
  printf -- "- \`<label>.toml\`: per-candidate config with \`sync_symlinks = true\`.\n"
  printf -- "- \`<label>.log\`: per-candidate push output.\n\n"
  printf 'Re-run command shape:\n\n'
  printf '```bash\n'
  printf 'scripts/tcfs-symlink-package-probe.sh \\\n'
  printf '  --endpoint %s \\\n' "$(shell_quote "$endpoint")"
  printf '  --bucket %s \\\n' "$(shell_quote "$bucket")"
  printf '  --prefix-base %s' "$(shell_quote "$prefix_base")"
  for spec in "${candidate_specs[@]}"; do
    printf ' \\\n  --candidate %s' "$(shell_quote "$spec")"
  done
  printf '\n```\n'
} >"$evidence_dir/README.md"

printf 'tcfs symlink package probe evidence: %s\n' "$evidence_dir"
printf 'overall status: %s\n' "$overall_status"

if [[ "$strict" == "1" && "$overall_status" != "passed" ]]; then
  exit 1
fi
