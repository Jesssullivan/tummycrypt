#!/usr/bin/env bash
set -euo pipefail

REMOTE=seaweedfs://100.64.48.53:8333/tcfs/git-repo-canary-oauth-mux-nixpkg-20260515T133843Z/linux-lifecycle/linux-lifecycle
TCFS_BIN_RAW=/nix/store/xq14ldwyl6bbabcggfvc4vrg9ml7pw1c-tcfs-cli-0.12.12/bin/tcfs
LIFECYCLE_SCRIPT=/tmp/tcfs-home-canary-linux-xr-shadow-20260515T133844Z/linux-lifecycle/linux-lifecycle/lazy-hydration-linux-lifecycle-demo.sh
EVIDENCE_DIR=/tmp/tcfs-home-canary-linux-xr-shadow-20260515T133844Z/linux-lifecycle/linux-lifecycle/evidence
CREATE_BUCKET=0

if [[ -n "${TCFS_HONEY_ENV_FILE:-}" ]]; then
  # shellcheck disable=SC1090
  source "$TCFS_HONEY_ENV_FILE"
fi

if [[ "$TCFS_BIN_RAW" == */* ]]; then
  TCFS_BIN_RESOLVED="$TCFS_BIN_RAW"
else
  TCFS_BIN_RESOLVED="$(command -v "$TCFS_BIN_RAW" || true)"
fi
if [[ -z "$TCFS_BIN_RESOLVED" || ! -x "$TCFS_BIN_RESOLVED" ]]; then
  echo "missing executable tcfs binary on honey: $TCFS_BIN_RAW" >&2
  exit 1
fi

mkdir -p "$(dirname "$EVIDENCE_DIR")" "$EVIDENCE_DIR"
args=(
  --remote "$REMOTE"
  --evidence-dir "$EVIDENCE_DIR"
  --tcfs-bin "$TCFS_BIN_RESOLVED"
)
if [[ "$CREATE_BUCKET" == "1" ]]; then
  args+=(--create-bucket)
fi

bash "$LIFECYCLE_SCRIPT" "${args[@]}"
