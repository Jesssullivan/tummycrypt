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
#   scripts/tcfs-onprem-migration-plan.sh render-target-pvc-commands
#   scripts/tcfs-onprem-migration-plan.sh render-import-pods
#   scripts/tcfs-onprem-migration-plan.sh render-transfer-commands
#   scripts/tcfs-onprem-migration-plan.sh render-candidate-apply-commands
#   scripts/tcfs-onprem-migration-plan.sh render-candidate-smoke-commands
#   scripts/tcfs-onprem-migration-plan.sh render-cutover-commands
#   scripts/tcfs-onprem-migration-plan.sh render-rollback-commands
#
set -euo pipefail

NAMESPACE="${TCFS_NAMESPACE:-tcfs}"
CONTEXT="${TCFS_CONTEXT:-}"
KUBECTL_BIN="${TCFS_KUBECTL:-kubectl}"
TOFU_BIN="${TCFS_TOFU:-tofu}"
TOFU_DIR="${TCFS_TOFU_DIR:-infra/tofu/environments/onprem}"
TARGET_NODE="${TCFS_TARGET_NODE:-bumble}"
IMPORT_IMAGE="${TCFS_IMPORT_IMAGE:-docker.io/library/busybox:1.36}"
TAILNET_DOMAIN="${TCFS_TAILNET_DOMAIN:-}"

NATS_SOURCE_PVC="${TCFS_NATS_SOURCE_PVC:-data-nats-0}"
NATS_TARGET_PVC="${TCFS_NATS_TARGET_PVC:-tcfs-nats-openebs-target}"
NATS_IMPORT_POD="${TCFS_NATS_IMPORT_POD:-tcfs-nats-openebs-import}"
NATS_TARGET_STORAGE_CLASS="${TCFS_NATS_TARGET_STORAGE_CLASS:-openebs-bumble-messaging-retain}"
NATS_SOURCE_STATEFULSET="${TCFS_NATS_SOURCE_STATEFULSET:-nats}"
NATS_SOURCE_SERVICE="${TCFS_NATS_SOURCE_SERVICE:-nats}"
NATS_CANDIDATE_STATEFULSET="${TCFS_NATS_CANDIDATE_STATEFULSET:-nats-openebs-candidate}"
NATS_CANDIDATE_SERVICE="${TCFS_NATS_CANDIDATE_SERVICE:-nats-openebs-candidate}"
NATS_CANDIDATE_TAILNET_SERVICE="${TCFS_NATS_CANDIDATE_TAILNET_SERVICE:-nats-tailnet-candidate}"
NATS_CANDIDATE_HOSTNAME="${TCFS_NATS_CANDIDATE_HOSTNAME:-nats-tcfs-candidate}"
NATS_CANONICAL_HOSTNAME="${TCFS_NATS_CANONICAL_HOSTNAME:-nats-tcfs}"

SEAWEEDFS_SOURCE_PVC="${TCFS_SEAWEEDFS_SOURCE_PVC:-data-seaweedfs-0}"
SEAWEEDFS_TARGET_PVC="${TCFS_SEAWEEDFS_TARGET_PVC:-tcfs-seaweedfs-openebs-target}"
SEAWEEDFS_IMPORT_POD="${TCFS_SEAWEEDFS_IMPORT_POD:-tcfs-seaweedfs-openebs-import}"
SEAWEEDFS_TARGET_STORAGE_CLASS="${TCFS_SEAWEEDFS_TARGET_STORAGE_CLASS:-openebs-bumble-s3-retain}"
SEAWEEDFS_SOURCE_STATEFULSET="${TCFS_SEAWEEDFS_SOURCE_STATEFULSET:-seaweedfs}"
SEAWEEDFS_SOURCE_SERVICE="${TCFS_SEAWEEDFS_SOURCE_SERVICE:-seaweedfs}"
SEAWEEDFS_CANDIDATE_STATEFULSET="${TCFS_SEAWEEDFS_CANDIDATE_STATEFULSET:-seaweedfs-openebs-candidate}"
SEAWEEDFS_CANDIDATE_SERVICE="${TCFS_SEAWEEDFS_CANDIDATE_SERVICE:-seaweedfs-openebs-candidate}"
SEAWEEDFS_CANDIDATE_TAILNET_SERVICE="${TCFS_SEAWEEDFS_CANDIDATE_TAILNET_SERVICE:-seaweedfs-tailnet-candidate}"
SEAWEEDFS_CANDIDATE_HOSTNAME="${TCFS_SEAWEEDFS_CANDIDATE_HOSTNAME:-seaweedfs-tcfs-candidate}"
SEAWEEDFS_CANONICAL_HOSTNAME="${TCFS_SEAWEEDFS_CANONICAL_HOSTNAME:-seaweedfs-tcfs}"

BACKEND_WORKER_DEPLOYMENT="${TCFS_BACKEND_WORKER_DEPLOYMENT:-tcfs-backend-tcfs-backend-worker}"

KUBECTL=("${KUBECTL_BIN}")
if [[ -n "${CONTEXT}" ]]; then
    KUBECTL+=(--context "${CONTEXT}")
fi

usage() {
    cat <<'EOF'
Usage:
  tcfs-onprem-migration-plan.sh facts
  tcfs-onprem-migration-plan.sh render-target-pvc-commands
  tcfs-onprem-migration-plan.sh render-import-pods
  tcfs-onprem-migration-plan.sh render-transfer-commands
  tcfs-onprem-migration-plan.sh render-candidate-apply-commands
  tcfs-onprem-migration-plan.sh render-candidate-smoke-commands
  tcfs-onprem-migration-plan.sh render-cutover-commands
  tcfs-onprem-migration-plan.sh render-rollback-commands

This script renders migration evidence and commands only. It does not mutate
Kubernetes, scale workloads, create PVCs, create Pods, copy data, or change
tailnet hostnames.
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

tofu_command() {
    printf '%s -chdir=%s' "$(shell_quote "${TOFU_BIN}")" "$(shell_quote "${TOFU_DIR}")"
}

tofu_var_arg() {
    shell_quote "-var=$1"
}

resource_arg() {
    shell_quote "$1/$2"
}

tailnet_host() {
    local hostname="$1"

    if [[ -n "${TAILNET_DOMAIN}" ]]; then
        printf '%s.%s' "${hostname}" "${TAILNET_DOMAIN}"
    else
        printf '%s' "${hostname}"
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

target_pvc_var_args() {
    printf '%s %s' \
        "$(tofu_var_arg "enable_stateful_migration_target_pvcs=true")" \
        "$(tofu_var_arg "enable_stateful_migration_candidate_workloads=false")"
}

candidate_var_args() {
    printf '%s %s %s' \
        "$(tofu_var_arg "enable_stateful_migration_target_pvcs=true")" \
        "$(tofu_var_arg "enable_stateful_migration_candidate_workloads=true")" \
        "$(tofu_var_arg "enable_tailnet_candidate_services=true")"
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

render_target_pvc_commands() {
    cat <<EOF
# Mutating commands for the approved downtime window only. Review the plan
# before running apply. This creates retained target PVCs only; it does not
# start candidate workloads or expose tailnet Services.

$(tofu_command) plan $(target_pvc_var_args)
$(tofu_command) apply $(target_pvc_var_args)
$(kubectl_command) -n $(shell_quote "${NAMESPACE}") get pvc $(shell_quote "${NATS_TARGET_PVC}") $(shell_quote "${SEAWEEDFS_TARGET_PVC}") -o wide
EOF
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
# 2. Apply target PVCs from:
#    scripts/tcfs-onprem-migration-plan.sh render-target-pvc-commands
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

render_candidate_apply_commands() {
    cat <<EOF
# Mutating commands for the approved downtime window only. Run after target
# PVCs exist and data has been copied into them. Review the plan before running
# apply. Candidate tailnet hostnames intentionally remain non-canonical.

$(tofu_command) plan $(candidate_var_args)
$(tofu_command) apply $(candidate_var_args)
$(kubectl_command) -n $(shell_quote "${NAMESPACE}") get statefulset $(shell_quote "${NATS_CANDIDATE_STATEFULSET}") $(shell_quote "${SEAWEEDFS_CANDIDATE_STATEFULSET}") -o wide
$(kubectl_command) -n $(shell_quote "${NAMESPACE}") get service $(shell_quote "${NATS_CANDIDATE_SERVICE}") $(shell_quote "${SEAWEEDFS_CANDIDATE_SERVICE}") $(shell_quote "${NATS_CANDIDATE_TAILNET_SERVICE}") $(shell_quote "${SEAWEEDFS_CANDIDATE_TAILNET_SERVICE}") -o wide
EOF
}

render_candidate_smoke_commands() {
    cat <<EOF
# Non-mutating render only. Run only after target PVCs, copied data, and
# candidate workloads/tailnet Services have been applied during an approved
# downtime window.

$(kubectl_command) -n $(shell_quote "${NAMESPACE}") rollout status $(shell_quote "statefulset/${NATS_CANDIDATE_STATEFULSET}") --timeout=180s
$(kubectl_command) -n $(shell_quote "${NAMESPACE}") rollout status $(shell_quote "statefulset/${SEAWEEDFS_CANDIDATE_STATEFULSET}") --timeout=180s
$(kubectl_command) -n $(shell_quote "${NAMESPACE}") get endpoints $(shell_quote "${NATS_CANDIDATE_SERVICE}") $(shell_quote "${SEAWEEDFS_CANDIDATE_SERVICE}") -o wide
$(kubectl_command) -n $(shell_quote "${NAMESPACE}") get service $(shell_quote "${NATS_CANDIDATE_TAILNET_SERVICE}") $(shell_quote "${SEAWEEDFS_CANDIDATE_TAILNET_SERVICE}") -o wide

curl -fsS $(shell_quote "http://$(tailnet_host "${NATS_CANDIDATE_HOSTNAME}"):8222/healthz")
curl -fsS $(shell_quote "http://$(tailnet_host "${SEAWEEDFS_CANDIDATE_HOSTNAME}"):9333/cluster/status")
EOF
}

render_cutover_commands() {
    cat <<EOF
# Mutating commands for the approved downtime window only. Run these after
# candidate workload and candidate tailnet smoke pass. The OpenTofu plan must
# be reviewed before the apply line is run.

# Remove canonical Tailscale ownership from the old kubectl-applied Services.
$(kubectl_command) -n $(shell_quote "${NAMESPACE}") annotate $(resource_arg service "${NATS_SOURCE_SERVICE}") $(shell_quote "tailscale.com/expose-") $(shell_quote "tailscale.com/hostname-") $(shell_quote "tailscale.com/proxy-class-") --overwrite
$(kubectl_command) -n $(shell_quote "${NAMESPACE}") annotate $(resource_arg service "${SEAWEEDFS_SOURCE_SERVICE}") $(shell_quote "tailscale.com/expose-") $(shell_quote "tailscale.com/hostname-") $(shell_quote "tailscale.com/proxy-class-") --overwrite

# Assign canonical hostnames to the source-owned tailnet Services.
$(tofu_command) plan $(tofu_var_arg "enable_stateful_migration_target_pvcs=true") $(tofu_var_arg "enable_stateful_migration_candidate_workloads=true") $(tofu_var_arg "enable_tailnet_candidate_services=true") $(tofu_var_arg "nats_tailnet_candidate_hostname=${NATS_CANONICAL_HOSTNAME}") $(tofu_var_arg "seaweedfs_tailnet_candidate_hostname=${SEAWEEDFS_CANONICAL_HOSTNAME}")
$(tofu_command) apply $(tofu_var_arg "enable_stateful_migration_target_pvcs=true") $(tofu_var_arg "enable_stateful_migration_candidate_workloads=true") $(tofu_var_arg "enable_tailnet_candidate_services=true") $(tofu_var_arg "nats_tailnet_candidate_hostname=${NATS_CANONICAL_HOSTNAME}") $(tofu_var_arg "seaweedfs_tailnet_candidate_hostname=${SEAWEEDFS_CANONICAL_HOSTNAME}")

$(kubectl_command) -n $(shell_quote "${NAMESPACE}") get service $(shell_quote "${NATS_CANDIDATE_TAILNET_SERVICE}") $(shell_quote "${SEAWEEDFS_CANDIDATE_TAILNET_SERVICE}") -o jsonpath=$(shell_quote "{range .items[*]}{.metadata.name}{\"\\t\"}{.metadata.annotations.tailscale\\.com/hostname}{\"\\t\"}{.metadata.annotations.tailscale\\.com/proxy-class}{\"\\n\"}{end}")
curl -fsS $(shell_quote "http://$(tailnet_host "${NATS_CANONICAL_HOSTNAME}"):8222/healthz")
curl -fsS $(shell_quote "http://$(tailnet_host "${SEAWEEDFS_CANONICAL_HOSTNAME}"):9333/cluster/status")
EOF
}

render_rollback_commands() {
    cat <<EOF
# Mutating rollback commands for the approved downtime window only. Use before
# deleting any old PVC/PV. Review which phase failed before running all lines.

# Stop source-owned candidates and remove their tailnet Services while keeping
# retained target PVCs for forensic comparison.
$(kubectl_command) -n $(shell_quote "${NAMESPACE}") scale $(resource_arg statefulset "${NATS_CANDIDATE_STATEFULSET}") $(resource_arg statefulset "${SEAWEEDFS_CANDIDATE_STATEFULSET}") --replicas=0
$(tofu_command) apply $(tofu_var_arg "enable_stateful_migration_target_pvcs=true") $(tofu_var_arg "enable_stateful_migration_candidate_workloads=false") $(tofu_var_arg "enable_tailnet_candidate_services=false")

# Restore old canonical annotations if cutover already removed them.
$(kubectl_command) -n $(shell_quote "${NAMESPACE}") annotate $(resource_arg service "${NATS_SOURCE_SERVICE}") $(shell_quote "tailscale.com/expose=true") $(shell_quote "tailscale.com/hostname=${NATS_CANONICAL_HOSTNAME}") $(shell_quote "tailscale.com/proxy-class-") --overwrite
$(kubectl_command) -n $(shell_quote "${NAMESPACE}") annotate $(resource_arg service "${SEAWEEDFS_SOURCE_SERVICE}") $(shell_quote "tailscale.com/expose=true") $(shell_quote "tailscale.com/hostname=${SEAWEEDFS_CANONICAL_HOSTNAME}") $(shell_quote "tailscale.com/proxy-class-") --overwrite

# Restart old honey-local stateful workloads and TCFS writers.
$(kubectl_command) -n $(shell_quote "${NAMESPACE}") scale $(resource_arg statefulset "${NATS_SOURCE_STATEFULSET}") $(resource_arg statefulset "${SEAWEEDFS_SOURCE_STATEFULSET}") --replicas=1
$(kubectl_command) -n $(shell_quote "${NAMESPACE}") rollout status $(resource_arg statefulset "${NATS_SOURCE_STATEFULSET}") --timeout=180s
$(kubectl_command) -n $(shell_quote "${NAMESPACE}") rollout status $(resource_arg statefulset "${SEAWEEDFS_SOURCE_STATEFULSET}") --timeout=180s
$(kubectl_command) -n $(shell_quote "${NAMESPACE}") scale $(resource_arg deployment "${BACKEND_WORKER_DEPLOYMENT}") --replicas=1
$(kubectl_command) -n $(shell_quote "${NAMESPACE}") rollout status $(resource_arg deployment "${BACKEND_WORKER_DEPLOYMENT}") --timeout=180s
EOF
}

main() {
    local command="${1:-}"

    case "${command}" in
        facts)
            require_command
            render_facts
            ;;
        render-target-pvc-commands)
            render_target_pvc_commands
            ;;
        render-import-pods)
            render_import_pods
            ;;
        render-transfer-commands)
            require_command
            render_transfer_commands
            ;;
        render-candidate-apply-commands)
            render_candidate_apply_commands
            ;;
        render-candidate-smoke-commands)
            render_candidate_smoke_commands
            ;;
        render-cutover-commands)
            render_cutover_commands
            ;;
        render-rollback-commands)
            render_rollback_commands
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
