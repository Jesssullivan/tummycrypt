#!/usr/bin/env bash
#
# tcfs-onprem-preflight.sh — read-only authority and mobility check for tcfs
#
# This script intentionally does not mutate Kubernetes. It summarizes the live
# facts needed before choosing between Helm adoption and OpenTofu migration.
#
# Usage:
#   scripts/tcfs-onprem-preflight.sh
#   TCFS_NAMESPACE=tcfs TCFS_CONTEXT=honey scripts/tcfs-onprem-preflight.sh
#
set -euo pipefail

NAMESPACE="${TCFS_NAMESPACE:-tcfs}"
CONTEXT="${TCFS_CONTEXT:-}"

KUBECTL=(kubectl)
HELM=(helm)
if [[ -n "${CONTEXT}" ]]; then
    KUBECTL+=(--context "${CONTEXT}")
    HELM+=(--kube-context "${CONTEXT}")
fi

info() { printf '[INFO] %s\n' "$*"; }
warn() { printf '[WARN] %s\n' "$*"; }

require_command() {
    if ! command -v "$1" >/dev/null 2>&1; then
        printf '[ERROR] %s is required but was not found in PATH\n' "$1" >&2
        exit 1
    fi
}

jsonpath() {
    local resource="$1"
    local path="$2"
    "${KUBECTL[@]}" -n "${NAMESPACE}" get "${resource}" -o "jsonpath=${path}" 2>/dev/null || true
}

service_annotation() {
    local service="$1"
    local key="$2"
    jsonpath "svc/${service}" "{.metadata.annotations.${key//./\\.}}"
}

print_service_summary() {
    local service="$1"
    local expose hostname proxy_class
    expose="$(service_annotation "${service}" 'tailscale.com/expose')"
    hostname="$(service_annotation "${service}" 'tailscale.com/hostname')"
    proxy_class="$(service_annotation "${service}" 'tailscale.com/proxy-class')"

    printf 'service/%s tailscale.expose=%s hostname=%s proxy-class=%s\n' \
        "${service}" \
        "${expose:-MISSING}" \
        "${hostname:-MISSING}" \
        "${proxy_class:-MISSING}"
}

print_pvc_summary() {
    local pvc="$1"
    local storage_class volume
    storage_class="$(jsonpath "pvc/${pvc}" '{.spec.storageClassName}')"
    volume="$(jsonpath "pvc/${pvc}" '{.spec.volumeName}')"

    printf 'pvc/%s storage-class=%s pv=%s\n' \
        "${pvc}" \
        "${storage_class:-MISSING}" \
        "${volume:-MISSING}"

    if [[ -n "${volume}" ]]; then
        "${KUBECTL[@]}" get "pv/${volume}" \
            -o custom-columns='PV:.metadata.name,RECLAIM:.spec.persistentVolumeReclaimPolicy,NODE:.spec.nodeAffinity.required.nodeSelectorTerms[*].matchExpressions[*].values[*],PATH:.spec.hostPath.path' \
            --no-headers 2>/dev/null || true
    fi
}

require_command kubectl
require_command helm

info "Namespace: ${NAMESPACE}"
if [[ -n "${CONTEXT}" ]]; then
    info "Context: ${CONTEXT}"
else
    info "Context: $("${KUBECTL[@]}" config current-context 2>/dev/null || printf unknown)"
fi

info "Helm release state"
"${HELM[@]}" list -n "${NAMESPACE}" || true

info "Core workload health"
"${KUBECTL[@]}" -n "${NAMESPACE}" get pod nats-0 seaweedfs-0 \
    -o custom-columns='NAME:.metadata.name,READY:.status.containerStatuses[*].ready,PHASE:.status.phase,NODE:.spec.nodeName,RESTARTS:.status.containerStatuses[*].restartCount' \
    --ignore-not-found
"${KUBECTL[@]}" -n "${NAMESPACE}" get deploy tcfs-backend-tcfs-backend-worker \
    -o custom-columns='NAME:.metadata.name,READY:.status.readyReplicas,AVAILABLE:.status.availableReplicas,DESIRED:.spec.replicas' \
    --ignore-not-found

info "Tailnet Service exposure"
print_service_summary nats
print_service_summary seaweedfs

info "Local state PVCs"
print_pvc_summary data-nats-0
print_pvc_summary data-seaweedfs-0

info "Authority call"
if ! "${HELM[@]}" list -n "${NAMESPACE}" | grep -q '^tcfs-backend[[:space:]]'; then
    warn "No tcfs-backend Helm release state was found."
fi

nats_proxy_class="$(service_annotation nats 'tailscale.com/proxy-class')"
seaweed_proxy_class="$(service_annotation seaweedfs 'tailscale.com/proxy-class')"
if [[ -z "${nats_proxy_class}" || -z "${seaweed_proxy_class}" ]]; then
    warn "NATS/SeaweedFS tailnet exposure is missing proxy-class ownership."
fi

warn "Do not treat ProxyClass-only movement as a durable fix."
warn "Choose Helm adoption or OpenTofu migration before changing live authority."
