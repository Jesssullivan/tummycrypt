#!/usr/bin/env bash
set -euo pipefail

# TIN-3277 landing gate: prove real ciphertext parity for the paths healed by
# the out-of-band self-rewrite fix (crates/tcfs-sync/src/reconcile.rs,
# self_rewrite_retick_applies / checked-remote-byte self-heal, merged via
# PR #580) before treating any of the 7 previously-stuck paths as resolved.
#
# Size parity is NOT sufficient: age encryption is non-deterministic (a fresh
# ephemeral key per encryption), so two ciphertexts of identical plaintext
# differ byte-for-byte and can coincidentally match in length. This gate
# SSHes into each host, decrypts the path locally on that host with an
# operator-supplied identity file that already lives there, and compares only
# the resulting SHA-256 of the PLAINTEXT across hosts. Ciphertext bytes and
# plaintext bytes are never transferred off either host or printed; only the
# digest crosses the wire.
#
# This is an operator-run tool: it requires an interactive SSH agent /
# passphrase for the identity file on each host and is not invoked by CI or
# by any agent session. Read-only: it runs `age -d` and `sha256sum`, nothing
# that writes.

usage() {
  cat <<'EOF'
Usage: scripts/tin3277-ciphertext-parity-gate.sh [options] [-- PATH...]

Options:
  --host-a HOST            SSH target for the first device (default: neo)
  --host-b HOST            SSH target for the second device (default: honey)
  --identity-a PATH        Path to the age identity file ON host-a
  --identity-b PATH        Path to the age identity file ON host-b
  --sync-root PATH         tcfs sync root on both hosts
                            (default: ~/tcfs)
  --ssh BIN                ssh binary to use (default: ssh; override for tests)
  -h, --help                Show this help

PATH... are paths relative to --sync-root. Default (the 7 TIN-3277 paths):
  secrets/api/github_token.age
  secrets/api/anthropic.age
  secrets/api/gitlab_token.age
  secrets/api/crates_io_token.age
  secrets/infrastructure/tailscale_auth_key.age
  secrets/.manifest.toml
  dotfiles/tcfs/devices.json

Exit status: 0 iff every path's plaintext SHA-256 matches on both hosts.
Non-.age paths (secrets/.manifest.toml, dotfiles/tcfs/devices.json) are
hashed directly (not decrypted) since they are not age containers.
EOF
}

HOST_A="neo"
HOST_B="honey"
IDENTITY_A=""
IDENTITY_B=""
SYNC_ROOT="tcfs"
SSH_BIN="ssh"
PATHS=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --host-a) HOST_A="$2"; shift 2 ;;
    --host-b) HOST_B="$2"; shift 2 ;;
    --identity-a) IDENTITY_A="$2"; shift 2 ;;
    --identity-b) IDENTITY_B="$2"; shift 2 ;;
    --sync-root) SYNC_ROOT="$2"; shift 2 ;;
    --ssh) SSH_BIN="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    --) shift; while [[ $# -gt 0 ]]; do PATHS+=("$1"); shift; done ;;
    *) PATHS+=("$1"); shift ;;
  esac
done

if [[ ${#PATHS[@]} -eq 0 ]]; then
  PATHS=(
    "secrets/api/github_token.age"
    "secrets/api/anthropic.age"
    "secrets/api/gitlab_token.age"
    "secrets/api/crates_io_token.age"
    "secrets/infrastructure/tailscale_auth_key.age"
    "secrets/.manifest.toml"
    "dotfiles/tcfs/devices.json"
  )
fi

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

[[ -n "$IDENTITY_A" ]] || die "--identity-a is required (path to age identity ON $HOST_A)"
[[ -n "$IDENTITY_B" ]] || die "--identity-b is required (path to age identity ON $HOST_B)"

# Remote command: print sha256 of plaintext for one path. .age paths are
# decrypted first with the identity that already lives on that host;
# non-.age paths (plaintext by design, e.g. secrets/.manifest.toml,
# dotfiles/tcfs/devices.json) are hashed as-is. Missing files print MISSING.
remote_hash() {
  local host="$1" identity="$2" root="$3" relpath="$4"
  local full="\$HOME/${root}/${relpath}"
  local remote_script
  if [[ "$relpath" == *.age ]]; then
    remote_script="f=$full; i=$identity; [ -f \"\$f\" ] || { echo MISSING; exit 0; }; age -d -i \"\$i\" \"\$f\" 2>/dev/null | sha256sum | cut -d' ' -f1"
  else
    remote_script="f=$full; [ -f \"\$f\" ] || { echo MISSING; exit 0; }; sha256sum \"\$f\" | cut -d' ' -f1"
  fi
  "$SSH_BIN" "$host" "$remote_script"
}

overall_status=0
printf '%-55s %-10s\n' "path" "result"
printf '%-55s %-10s\n' "----" "------"

for relpath in "${PATHS[@]}"; do
  hash_a="$(remote_hash "$HOST_A" "$IDENTITY_A" "$SYNC_ROOT" "$relpath" || echo ERROR)"
  hash_b="$(remote_hash "$HOST_B" "$IDENTITY_B" "$SYNC_ROOT" "$relpath" || echo ERROR)"

  if [[ "$hash_a" == "ERROR" || "$hash_b" == "ERROR" ]]; then
    result="ERROR"
    overall_status=1
  elif [[ "$hash_a" == "MISSING" && "$hash_b" == "MISSING" ]]; then
    result="MISSING-BOTH"
    overall_status=1
  elif [[ "$hash_a" == "MISSING" || "$hash_b" == "MISSING" ]]; then
    result="MISSING-ONE"
    overall_status=1
  elif [[ "$hash_a" == "$hash_b" ]]; then
    result="PARITY"
  else
    result="MISMATCH"
    overall_status=1
  fi

  printf '%-55s %-10s\n' "$relpath" "$result"
done

exit "$overall_status"
