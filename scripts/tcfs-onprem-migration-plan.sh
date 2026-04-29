#!/usr/bin/env bash
#
# tcfs-onprem-migration-plan.sh - render downtime migration commands.
#
# This script is intentionally non-mutating. It reads current PVC/PV locality
# and renders the import Pod manifests and two-hop transfer commands needed to
# move TCFS NATS/SeaweedFS data from honey-local local-path PVs to retained
# OpenEBS/ZFS target PVCs.
#
# Usage:
#   scripts/tcfs-onprem-migration-plan.sh facts
#   scripts/tcfs-onprem-migration-plan.sh render-import-pods
#   scripts/tcfs-onprem-migration-plan.sh render-transfer-commands
#
set -euo pipefail

NAMESPACE="${TCFS_NAMESPACE:-tcfs}"
CONTEXT="${TCFS_CONTEXT:-}"
KUBECTL_BIN="${TCFS_KUBECTL:-kubectl}"
TARGET_NODE="${TCFS_TARGET_NODE:-bumble}"
IMPORT_IMAGE="${TCFS_IMPORT_IMAGE:-docker.io/library/busybox:1.36}"

NATS_SOURCE_PVC="${TCFS_NATS_SOURCE_PVC:-data-nats-0}"
NATS_TARGET_PVC="${TCFS_NATS_TARGET_PVC:-tcfs-nats-openebs-target}"
NATS_IMPORT_POD="${TCFS_NATS_IMPORT_POD:-tcfs-nats-openebs-import}"
NATS_TARGET_STORAGE_CLASS="${TCFS_NATS_TARGET_STORAGE_CLASS:-openebs-bumble-messaging-retain}"

SEAWEEDFS_SOURCE_PVC="${TCFS_SEAWEEDFS_SOURCE_PVC:-data-seaweedfs-0}"
SEAWEEDFS_TARGET_PVC="${TCFS_SEAWEEDFS_TARGET_PVC:-tcfs-seaweedfs-openebs-target}"
SEAWEEDFS_IMPORT_POD="${TCFS_SEAWEEDFS_IMPORT_POD:-tcfs-seaweedfs-openebs-import}"
SEAWEEDFS_TARGET_STORAGE_CLASS="${TCFS_SEAWEEDFS_TARGET_STORAGE_CLASS:-openebs-bumble-s3-retain}"

KUBECTL=("${KUBECTL_BIN}")
if [[ -n "${CONTEXT}" ]]; then
    KUBECTL+=(--context "${CONTEXT}")
fi

usage() {
    cat <<'EOF'
Usage:
  tcfs-onprem-migration-plan.sh facts
  tcfs-onprem-migration-plan.sh render-import-pods
  tcfs-onprem-migration-plan.sh render-transfer-commands

This script renders migration evidence and commands only. It does not mutate
Kubernetes, scale workloads, create PVCs, create Pods, or copy data.
EOF
}

die() {
    printf '[ERROR] %s\n' "$*" >&2
    exit 1
}

info() {
    printf '[INFO] %s\n' "$*"
}

shell_quote() {
    local value="$1"
    printf "'%s'" "${value//\'/\'\\\'\'}"
}

kubectl_command() {
    printf '%s' "$(shell_quote "${KUBECTL_BIN}")"
    if [[ -n "${CONTEXT}" ]]; then
        printf ' --context %s' "$(shell_quote "${CONTEXT}")"
    fi
}

jsonpath_cluster() {
    local resource="$1"
    local path="$2"
    "${KUBECTL[@]}" get "${resource}" -o "jsonpath=${path}" 2>/dev/null || true
}

jsonpath_namespaced() {
    local resource="$1"
    local path="$2"
    "${KUBECTL[@]}" -n "${NAMESPACE}" get "${resource}" -o "jsonpath=${path}" 2>/dev/null || true
}

require_command() {
    command -v "${KUBECTL_BIN}" >/dev/null 2>&1 || die "${KUBECTL_BIN} is required but was not found"
}

pvc_storage_class() {
    jsonpath_namespaced "pvc/$1" '{.spec.storageClassName}'
}

pvc_volume() {
    jsonpath_namespaced "pvc/$1" '{.spec.volumeName}'
}

pv_node() {
    jsonpath_cluster "pv/$1" '{.spec.nodeAffinity.required.nodeSelectorTerms[0].matchExpressions[0].values[0]}'
}

pv_path() {
    local volume="$1"
    local path

    path="$(jsonpath_cluster "pv/${volume}" '{.spec.hostPath.path}')"
    if [[ -z "${path}" ]]; then
        path="$(jsonpath_cluster "pv/${volume}" '{.spec.local.path}')"
    fi

    printf '%s' "${path}"
}

target_pvc_storage_class() {
    jsonpath_namespaced "pvc/$1" '{.spec.storageClassName}'
}

source_fact_line() {
    local name="$1"
    local source_pvc="$2"
    local target_pvc="$3"
    local target_storage_class="$4"
    local source_storage_class volume node path target_current_class

    source_storage_class="$(pvc_storage_class "${source_pvc}")"
    [[ -n "${source_storage_class}" ]] || die "pvc/${source_pvc} is missing or unreadable"

    volume="$(pvc_volume "${source_pvc}")"
    [[ -n "${volume}" ]] || die "pvc/${source_pvc} has no bound volume"

    node="$(pv_node "${volume}")"
    [[ -n "${node}" ]] || die "pv/${volume} has no nodeAffinity node value"

    path="$(pv_path "${volume}")"
    [[ -n "${path}" ]] || die "pv/${volume} has no hostPath/local path"

    target_current_class="$(target_pvc_storage_class "${target_pvc}")"
    if [[ -n "${target_current_class}" && "${target_current_class}" != "${target_storage_class}" ]]; then
        die "pvc/${target_pvc} exists with storageClass=${target_current_class}, expected ${target_storage_class}"
    fi

    printf 'source/%s pvc=%s pv=%s storage-class=%s node=%s path=%s target-pvc=%s target-storage-class=%s target-present=%s\n' \
        "${name}" \
        "${source_pvc}" \
        "${volume}" \
        "${source_storage_class}" \
        "${node}" \
        "${path}" \
        "${target_pvc}" \
        "${target_storage_class}" \
        "$([[ -n "${target_current_class}" ]] && printf yes || printf no)"
}

render_facts() {
    info "Namespace: ${NAMESPACE}"
    if [[ -n "${CONTEXT}" ]]; then
        info "Context: ${CONTEXT}"
    fi

    source_fact_line nats "${NATS_SOURCE_PVC}" "${NATS_TARGET_PVC}" "${NATS_TARGET_STORAGE_CLASS}"
    source_fact_line seaweedfs "${SEAWEEDFS_SOURCE_PVC}" "${SEAWEEDFS_TARGET_PVC}" "${SEAWEEDFS_TARGET_STORAGE_CLASS}"
}

render_import_pod() {
    local name="$1"
    local component="$2"
    local target_pvc="$3"

    cat <<EOF
apiVersion: v1
kind: Pod
metadata:
  name: ${name}
  namespace: ${NAMESPACE}
  labels:
    app.kubernetes.io/part-of: tcfs
    app.kubernetes.io/managed-by: manual-downtime-migration
    app.kubernetes.io/component: ${component}-import
    tummycrypt.dev/migration: stateful-openebs-import
spec:
  restartPolicy: Never
  nodeSelector:
    kubernetes.io/hostname: ${TARGET_NODE}
  containers:
    - name: import
      image: ${IMPORT_IMAGE}
      command:
        - sh
        - -lc
        - trap : TERM INT; sleep 86400 & wait
      volumeMounts:
        - name: target
          mountPath: /target
  volumes:
    - name: target
      persistentVolumeClaim:
        claimName: ${target_pvc}
EOF
}

render_import_pods() {
    render_import_pod "${NATS_IMPORT_POD}" nats "${NATS_TARGET_PVC}"
    printf '%s\n' '---'
    render_import_pod "${SEAWEEDFS_IMPORT_POD}" seaweedfs "${SEAWEEDFS_TARGET_PVC}"
}

transfer_command() {
    local label="$1"
    local source_pvc="$2"
    local import_pod="$3"
    local volume node path

    volume="$(pvc_volume "${source_pvc}")"
    [[ -n "${volume}" ]] || die "pvc/${source_pvc} has no bound volume"

    node="$(pv_node "${volume}")"
    [[ -n "${node}" ]] || die "pv/${volume} has no nodeAffinity node value"

    path="$(pv_path "${volume}")"
    [[ -n "${path}" ]] || die "pv/${volume} has no hostPath/local path"

    printf '# Transfer %s from %s:%s into pod/%s:/target\n' "${label}" "${node}" "${path}" "${import_pod}"
    printf 'ssh %s %s | %s -n %s exec -i %s -- tar -C /target -xpf -\n' \
        "$(shell_quote "root@${node}")" \
        "$(shell_quote "tar -C $(shell_quote "${path}") -cpf - .")" \
        "$(kubectl_command)" \
        "$(shell_quote "${NAMESPACE}")" \
        "$(shell_quote "${import_pod}")"
}

render_transfer_commands() {
    cat <<EOF
# Non-mutating render only. Review before running during an approved downtime window.
# Required setup before these commands:
# 1. Quiesce external TCFS writers.
# 2. Apply target PVCs with enable_stateful_migration_target_pvcs=true.
# 3. Apply import pods from:
#    scripts/tcfs-onprem-migration-plan.sh render-import-pods | $(kubectl_command) apply -f -
# 4. Confirm source StatefulSets are scaled down before copying.

$(kubectl_command) -n $(shell_quote "${NAMESPACE}") scale deployment/tcfs-backend-tcfs-backend-worker --replicas=0
$(kubectl_command) -n $(shell_quote "${NAMESPACE}") scale statefulset/nats statefulset/seaweedfs --replicas=0
$(kubectl_command) -n $(shell_quote "${NAMESPACE}") wait --for=delete pod/nats-0 --timeout=180s
$(kubectl_command) -n $(shell_quote "${NAMESPACE}") wait --for=delete pod/seaweedfs-0 --timeout=180s
$(kubectl_command) -n $(shell_quote "${NAMESPACE}") wait --for=condition=Ready $(shell_quote "pod/${NATS_IMPORT_POD}") --timeout=180s
$(kubectl_command) -n $(shell_quote "${NAMESPACE}") wait --for=condition=Ready $(shell_quote "pod/${SEAWEEDFS_IMPORT_POD}") --timeout=180s

EOF

    transfer_command nats "${NATS_SOURCE_PVC}" "${NATS_IMPORT_POD}"
    transfer_command seaweedfs "${SEAWEEDFS_SOURCE_PVC}" "${SEAWEEDFS_IMPORT_POD}"
}

main() {
    local command="${1:-}"

    case "${command}" in
        facts)
            require_command
            render_facts
            ;;
        render-import-pods)
            render_import_pods
            ;;
        render-transfer-commands)
            require_command
            render_transfer_commands
            ;;
        -h|--help|help)
            usage
            ;;
        *)
            usage >&2
            exit 2
            ;;
    esac
}

main "$@"
