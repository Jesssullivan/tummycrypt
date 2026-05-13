#!/usr/bin/env bash
#
# tcfs-onprem-cutover-packet.sh - render the downtime cutover packet.
#
# This script is intentionally non-mutating. It validates that the human
# scheduling and ownership gates are named, then prints the exact read-only and
# render-only commands that prepare the TCFS on-prem OpenTofu cutover.
#
set -euo pipefail

CONTEXT="${TCFS_CONTEXT:-honey}"
NAMESPACE="${TCFS_NAMESPACE:-tcfs}"
TRACKERS="${TCFS_TRACKERS:-GitHub #327 / Linear TIN-720}"
DOWNTIME_WINDOW="${TCFS_DOWNTIME_WINDOW:-}"
PREFLIGHT_OWNER="${TCFS_PREFLIGHT_OWNER:-}"
ROLLBACK_OWNER="${TCFS_ROLLBACK_OWNER:-}"
POSTCUT_SMOKE_OWNER="${TCFS_POSTCUT_SMOKE_OWNER:-}"

die() {
    printf '[ERROR] %s\n' "$*" >&2
    exit 1
}

shell_quote() {
    local value="$1"
    printf "'"
    printf '%s' "${value}" | sed "s/'/'\\\\''/g"
    printf "'"
}

require_named_value() {
    local name="$1"
    local value="$2"
    local lowered

    lowered="$(printf '%s' "${value}" | tr '[:upper:]' '[:lower:]')"
    lowered="${lowered#"${lowered%%[![:space:]]*}"}"
    lowered="${lowered%"${lowered##*[![:space:]]}"}"
    case "${lowered}" in
        ""|"tbd"|"tbd "*|"tbd:"*|"tbd,"*|"todo"|"todo "*|"todo:"*|"todo,"*|"unknown"|"unset"|"none"|"n/a"|"na")
            die "${name} must be explicitly named before rendering the cutover packet"
            ;;
    esac
}

command_with_context() {
    local recipe="$1"
    local arg="${2:-}"

    printf 'TCFS_CONTEXT=%s TCFS_NAMESPACE=%s just %s' \
        "$(shell_quote "${CONTEXT}")" \
        "$(shell_quote "${NAMESPACE}")" \
        "${recipe}"
    if [[ -n "${arg}" ]]; then
        printf ' %s' "${arg}"
    fi
    printf '\n'
}

render_command_block() {
    local label="$1"
    local recipe="$2"
    local arg="${3:-}"

    printf '%s:\n' "${label}"
    command_with_context "${recipe}" "${arg}"
    printf '\n'
}

require_named_value "TCFS_DOWNTIME_WINDOW" "${DOWNTIME_WINDOW}"
require_named_value "TCFS_PREFLIGHT_OWNER" "${PREFLIGHT_OWNER}"
require_named_value "TCFS_ROLLBACK_OWNER" "${ROLLBACK_OWNER}"
require_named_value "TCFS_POSTCUT_SMOKE_OWNER" "${POSTCUT_SMOKE_OWNER}"

cat <<EOF
# TCFS on-prem cutover execution packet

metadata:
  context: ${CONTEXT}
  namespace: ${NAMESPACE}
  trackers: ${TRACKERS}
  downtime_window: ${DOWNTIME_WINDOW}
  preflight_owner: ${PREFLIGHT_OWNER}
  rollback_owner: ${ROLLBACK_OWNER}
  post_cut_smoke_owner: ${POSTCUT_SMOKE_OWNER}

stop_lines:
  - Do not run live OpenTofu apply, kubectl apply, scale, annotate, or data copy
    outside the named downtime window.
  - Do not fix the Blahaj tailnet gate with a ProxyClass-only patch to the old
    kubectl-applied NATS or SeaweedFS Services.
  - Do not delete old retained PVCs or PVs during the first cutover pass.
  - Stop and roll back if candidate NATS, SeaweedFS, backend-worker, or
    canonical tailnet smoke fails.

pre-window read-only evidence:
EOF

render_command_block "authority and mobility preflight" onprem-preflight
render_command_block "data inventory" onprem-data-inventory
render_command_block "OpenTofu source validation" onprem-tofu-validate
render_command_block "live migration facts" onprem-migration-plan facts

cat <<EOF
downtime render sequence:
EOF

render_command_block "1. retained target PVC plan/apply commands" onprem-migration-plan render-target-pvc-commands
render_command_block "2. import Pod manifests" onprem-migration-plan render-import-pods
render_command_block "3. quiesce and transfer commands" onprem-migration-plan render-transfer-commands
render_command_block "4. candidate workload and tailnet Service commands" onprem-migration-plan render-candidate-apply-commands
render_command_block "5. candidate smoke commands" onprem-migration-plan render-candidate-smoke-commands
render_command_block "6. canonical tailnet cutover commands" onprem-migration-plan render-cutover-commands
render_command_block "7. rollback commands" onprem-migration-plan render-rollback-commands

cat <<EOF
completion evidence:
  - target NATS and SeaweedFS PVCs are retained OpenEBS/ZFS PVCs.
  - source-owned candidate workloads pass rollout and endpoint smoke.
  - canonical nats-tcfs and seaweedfs-tcfs hostnames pass tailnet smoke.
  - Blahaj tailscale-proxy-placement no longer reports tcfs/nats or
    tcfs/seaweedfs as missing proxy-class ownership.
  - rollback owner records whether old retained PVs remain on hold or are
    approved for later cleanup.
EOF
