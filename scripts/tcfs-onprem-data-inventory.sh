#!/usr/bin/env bash
#
# tcfs-onprem-data-inventory.sh - read-only data inventory for TCFS migration.
#
# This script intentionally does not mutate Kubernetes. It captures the live
# NATS JetStream and SeaweedFS facts needed before moving their stateful data
# from honey-local local-path PVCs to the Tinyland OpenEBS/ZFS classes.
#
# Usage:
#   scripts/tcfs-onprem-data-inventory.sh
#   TCFS_NAMESPACE=tcfs TCFS_CONTEXT=honey scripts/tcfs-onprem-data-inventory.sh
#
set -euo pipefail

NAMESPACE="${TCFS_NAMESPACE:-tcfs}"
CONTEXT="${TCFS_CONTEXT:-}"
NATS_TARGET_STORAGE_CLASS="${TCFS_NATS_TARGET_STORAGE_CLASS:-openebs-bumble-messaging-retain}"
SEAWEEDFS_TARGET_STORAGE_CLASS="${TCFS_SEAWEEDFS_TARGET_STORAGE_CLASS:-openebs-bumble-s3-retain}"

KUBECTL=(kubectl)
if [[ -n "${CONTEXT}" ]]; then
    KUBECTL+=(--context "${CONTEXT}")
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

print_storage_class_summary() {
    local storage_class="$1"

    if ! "${KUBECTL[@]}" get storageclass "${storage_class}" >/dev/null 2>&1; then
        warn "storageclass/${storage_class} is missing"
        return
    fi

    "${KUBECTL[@]}" get "storageclass/${storage_class}" \
        -o custom-columns='NAME:.metadata.name,PROVISIONER:.provisioner,RECLAIM:.reclaimPolicy,BINDING:.volumeBindingMode,EXPAND:.allowVolumeExpansion,POOL:.parameters.poolname,RECORDSIZE:.parameters.recordsize,TOPOLOGY:.allowedTopologies[*].matchLabelExpressions[*].values[*]' \
        --no-headers
}

print_pvc_summary() {
    local pvc="$1"
    local target_storage_class="$2"
    local storage_class size volume

    storage_class="$(jsonpath "pvc/${pvc}" '{.spec.storageClassName}')"
    size="$(jsonpath "pvc/${pvc}" '{.spec.resources.requests.storage}')"
    volume="$(jsonpath "pvc/${pvc}" '{.spec.volumeName}')"

    if [[ -z "${storage_class}" ]]; then
        warn "pvc/${pvc} is missing"
        return
    fi

    printf 'pvc/%s size=%s current-storage-class=%s target-storage-class=%s pv=%s\n' \
        "${pvc}" \
        "${size:-MISSING}" \
        "${storage_class}" \
        "${target_storage_class}" \
        "${volume:-MISSING}"

    if [[ "${storage_class}" != "${target_storage_class}" ]]; then
        warn "pvc/${pvc} is still on ${storage_class}; migration target is ${target_storage_class}"
    fi

    if [[ -n "${volume}" ]]; then
        "${KUBECTL[@]}" get "pv/${volume}" \
            -o custom-columns='PV:.metadata.name,RECLAIM:.spec.persistentVolumeReclaimPolicy,NODE:.spec.nodeAffinity.required.nodeSelectorTerms[*].matchExpressions[*].values[*],HOSTPATH:.spec.hostPath.path,LOCAL:.spec.local.path,CSI:.spec.csi.driver,VOLUMEHANDLE:.spec.csi.volumeHandle' \
            --no-headers 2>/dev/null || warn "could not read pv/${volume}"
    fi
}

print_pod_summary() {
    local pod="$1"

    if ! "${KUBECTL[@]}" -n "${NAMESPACE}" get "pod/${pod}" >/dev/null 2>&1; then
        warn "pod/${pod} is missing"
        return
    fi

    "${KUBECTL[@]}" -n "${NAMESPACE}" get "pod/${pod}" \
        -o custom-columns='NAME:.metadata.name,READY:.status.containerStatuses[*].ready,PHASE:.status.phase,NODE:.spec.nodeName,RESTARTS:.status.containerStatuses[*].restartCount,IP:.status.podIP' \
        --no-headers
    "${KUBECTL[@]}" -n "${NAMESPACE}" get "pod/${pod}" \
        -o jsonpath='{range .spec.containers[*]}container={.name} image={.image} args={.args}{"\n"}{end}{range .spec.volumes[*]}volume={.name} pvc={.persistentVolumeClaim.claimName}{"\n"}{end}'
    printf '\n'
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

pod_exec_inventory() {
    local pod="$1"
    local title="$2"
    local command="$3"

    info "${title}"
    if ! "${KUBECTL[@]}" -n "${NAMESPACE}" get "pod/${pod}" >/dev/null 2>&1; then
        warn "pod/${pod} is missing; skipping ${title}"
        return
    fi

    if ! "${KUBECTL[@]}" -n "${NAMESPACE}" exec "${pod}" -- sh -lc "${command}"; then
        warn "pod/${pod} inventory command failed; keep this gate red until readback is understood"
    fi
}

require_command kubectl

info "Namespace: ${NAMESPACE}"
if [[ -n "${CONTEXT}" ]]; then
    info "Context: ${CONTEXT}"
else
    info "Context: $("${KUBECTL[@]}" config current-context 2>/dev/null || printf unknown)"
fi

info "Target durable storage classes"
print_storage_class_summary "${NATS_TARGET_STORAGE_CLASS}"
print_storage_class_summary "${SEAWEEDFS_TARGET_STORAGE_CLASS}"

info "Current state PVCs and backing PVs"
print_pvc_summary data-nats-0 "${NATS_TARGET_STORAGE_CLASS}"
print_pvc_summary data-seaweedfs-0 "${SEAWEEDFS_TARGET_STORAGE_CLASS}"

info "Stateful workload pods"
print_pod_summary nats-0
print_pod_summary seaweedfs-0

info "Current tailnet Services"
print_service_summary nats
print_service_summary seaweedfs

pod_exec_inventory nats-0 "NATS JetStream disk and monitor inventory" '
printf "tools:"
for tool in nats nats-server wget curl du df find; do
    command -v "${tool}" >/dev/null 2>&1 && printf " %s" "${tool}"
done
printf "\n\n"
df -h /data 2>/dev/null || true
du -sh /data /data/jetstream 2>/dev/null || true
printf "\njetstream directories\n"
find /data -maxdepth 3 -type d 2>/dev/null | sort || true
printf "\nNATS /varz\n"
wget -qO- "http://127.0.0.1:8222/varz" 2>/dev/null | head -c 2400 || true
printf "\nNATS /jsz?streams=true&consumer=true\n"
wget -qO- "http://127.0.0.1:8222/jsz?streams=true&consumer=true" 2>/dev/null | head -c 6000 || true
printf "\n"
'

pod_exec_inventory seaweedfs-0 "SeaweedFS disk and topology inventory" '
printf "tools:"
for tool in weed wget curl du df find; do
    command -v "${tool}" >/dev/null 2>&1 && printf " %s" "${tool}"
done
printf "\n\n"
df -h /data 2>/dev/null || true
du -sh /data /data/filerldb2 2>/dev/null || true
printf "\nseaweed directories\n"
find /data -maxdepth 3 -type d 2>/dev/null | sort || true
printf "\nSeaweedFS master /cluster/status\n"
wget -qO- "http://127.0.0.1:9333/cluster/status" 2>/dev/null | head -c 2400 || true
printf "\nSeaweedFS master /dir/status\n"
wget -qO- "http://127.0.0.1:9333/dir/status" 2>/dev/null | head -c 6000 || true
printf "\n"
'

warn "This is inventory only. Do not delete, patch, or rebind the live PVCs from this script output alone."
warn "Next migration work must preserve rollback for the retained honey local-path PVs and the new OpenEBS/ZFS PVs."
