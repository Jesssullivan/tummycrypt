#!/usr/bin/env bash
#
# Regression tests for tcfs-onprem-migration-plan.sh.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="${REPO_ROOT}/scripts/tcfs-onprem-migration-plan.sh"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "${TMPDIR}"' EXIT

FAKE_KUBECTL="${TMPDIR}/kubectl"
cat >"${FAKE_KUBECTL}" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "--context" ]]; then
    shift 2
fi

namespace=""
if [[ "${1:-}" == "-n" ]]; then
    namespace="$2"
    shift 2
fi

[[ "${1:-}" == "get" ]] || exit 1
resource="$2"
shift 2

jsonpath=""
while [[ "$#" -gt 0 ]]; do
    case "$1" in
        -o)
            jsonpath="${2#jsonpath=}"
            shift 2
            ;;
        *)
            shift
            ;;
    esac
done

case "${namespace}:${resource}:${jsonpath}" in
    "tcfs:pvc/data-nats-0:{.spec.storageClassName}") printf 'local-path' ;;
    "tcfs:pvc/data-nats-0:{.spec.volumeName}") printf 'pv-nats' ;;
    "tcfs:pvc/data-seaweedfs-0:{.spec.storageClassName}") printf 'local-path' ;;
    "tcfs:pvc/data-seaweedfs-0:{.spec.volumeName}") printf 'pv-seaweedfs' ;;
    "tcfs:pvc/tcfs-nats-openebs-target:{.spec.storageClassName}")
        if [[ "${FAKE_BAD_TARGET_CLASS:-}" == "1" ]]; then
            printf 'wrong-class'
        else
            printf 'openebs-bumble-messaging-retain'
        fi
        ;;
    "tcfs:pvc/tcfs-seaweedfs-openebs-target:{.spec.storageClassName}") ;;
    ":pv/pv-nats:{.spec.nodeAffinity.required.nodeSelectorTerms[0].matchExpressions[0].values[0]}") printf 'honey' ;;
    ":pv/pv-nats:{.spec.hostPath.path}") printf '/opt/local-path-provisioner/nats data' ;;
    ":pv/pv-nats:{.spec.local.path}") ;;
    ":pv/pv-seaweedfs:{.spec.nodeAffinity.required.nodeSelectorTerms[0].matchExpressions[0].values[0]}") printf 'honey' ;;
    ":pv/pv-seaweedfs:{.spec.hostPath.path}") printf '/opt/local-path-provisioner/seaweedfs' ;;
    ":pv/pv-seaweedfs:{.spec.local.path}") ;;
    *) ;;
esac
EOF
chmod +x "${FAKE_KUBECTL}"

assert_contains() {
    local file="$1"
    local expected="$2"

    if ! grep -Fq "${expected}" "${file}"; then
        printf 'expected to find %s in %s\n' "${expected}" "${file}" >&2
        printf '--- output ---\n' >&2
        cat "${file}" >&2
        exit 1
    fi
}

FACTS_OUT="${TMPDIR}/facts.out"
TCFS_KUBECTL="${FAKE_KUBECTL}" bash "${SCRIPT}" facts >"${FACTS_OUT}"
assert_contains "${FACTS_OUT}" 'source/nats pvc=data-nats-0 pv=pv-nats storage-class=local-path node=honey path=/opt/local-path-provisioner/nats data target-pvc=tcfs-nats-openebs-target target-storage-class=openebs-bumble-messaging-retain target-present=yes'
assert_contains "${FACTS_OUT}" 'source/seaweedfs pvc=data-seaweedfs-0 pv=pv-seaweedfs storage-class=local-path node=honey path=/opt/local-path-provisioner/seaweedfs target-pvc=tcfs-seaweedfs-openebs-target target-storage-class=openebs-bumble-s3-retain target-present=no'

TARGET_PVC_OUT="${TMPDIR}/target-pvc.out"
TCFS_CONTEXT="honey context" TCFS_KUBECTL="${FAKE_KUBECTL}" TCFS_TOFU="/opt/tofu bin/tofu" bash "${SCRIPT}" render-target-pvc-commands >"${TARGET_PVC_OUT}"
assert_contains "${TARGET_PVC_OUT}" "'/opt/tofu bin/tofu' -chdir='infra/tofu/environments/onprem' plan '-var=enable_stateful_migration_target_pvcs=true' '-var=enable_stateful_migration_candidate_workloads=false'"
assert_contains "${TARGET_PVC_OUT}" "'/opt/tofu bin/tofu' -chdir='infra/tofu/environments/onprem' apply '-var=enable_stateful_migration_target_pvcs=true' '-var=enable_stateful_migration_candidate_workloads=false'"
assert_contains "${TARGET_PVC_OUT}" "'${FAKE_KUBECTL}' --context 'honey context' -n 'tcfs' get pvc 'tcfs-nats-openebs-target' 'tcfs-seaweedfs-openebs-target' -o wide"

PODS_OUT="${TMPDIR}/pods.out"
bash "${SCRIPT}" render-import-pods >"${PODS_OUT}"
assert_contains "${PODS_OUT}" 'name: tcfs-nats-openebs-import'
assert_contains "${PODS_OUT}" 'claimName: tcfs-nats-openebs-target'
assert_contains "${PODS_OUT}" 'name: tcfs-seaweedfs-openebs-import'
assert_contains "${PODS_OUT}" 'kubernetes.io/hostname: bumble'

COMMANDS_OUT="${TMPDIR}/commands.out"
TCFS_CONTEXT="honey context" TCFS_KUBECTL="${FAKE_KUBECTL}" bash "${SCRIPT}" render-transfer-commands >"${COMMANDS_OUT}"
assert_contains "${COMMANDS_OUT}" "'${FAKE_KUBECTL}' --context 'honey context' -n 'tcfs' scale statefulset/nats statefulset/seaweedfs --replicas=0"
assert_contains "${COMMANDS_OUT}" "# Transfer nats from honey:/opt/local-path-provisioner/nats data into pod/tcfs-nats-openebs-import:/target"
assert_contains "${COMMANDS_OUT}" "ssh 'root@honey' 'tar -C '\\''/opt/local-path-provisioner/nats data'\\'' -cpf - .' | '${FAKE_KUBECTL}' --context 'honey context' -n 'tcfs' exec -i 'tcfs-nats-openebs-import' -- tar -C /target -xpf -"

CANDIDATE_APPLY_OUT="${TMPDIR}/candidate-apply.out"
TCFS_CONTEXT="honey context" TCFS_KUBECTL="${FAKE_KUBECTL}" TCFS_TOFU="/opt/tofu bin/tofu" bash "${SCRIPT}" render-candidate-apply-commands >"${CANDIDATE_APPLY_OUT}"
assert_contains "${CANDIDATE_APPLY_OUT}" "'/opt/tofu bin/tofu' -chdir='infra/tofu/environments/onprem' plan '-var=enable_stateful_migration_target_pvcs=true' '-var=enable_stateful_migration_candidate_workloads=true' '-var=enable_tailnet_candidate_services=true'"
assert_contains "${CANDIDATE_APPLY_OUT}" "'/opt/tofu bin/tofu' -chdir='infra/tofu/environments/onprem' apply '-var=enable_stateful_migration_target_pvcs=true' '-var=enable_stateful_migration_candidate_workloads=true' '-var=enable_tailnet_candidate_services=true'"
assert_contains "${CANDIDATE_APPLY_OUT}" "'${FAKE_KUBECTL}' --context 'honey context' -n 'tcfs' get service 'nats-openebs-candidate' 'seaweedfs-openebs-candidate' 'nats-tailnet-candidate' 'seaweedfs-tailnet-candidate' -o wide"

SMOKE_OUT="${TMPDIR}/candidate-smoke.out"
TCFS_CONTEXT="honey context" TCFS_KUBECTL="${FAKE_KUBECTL}" TCFS_TAILNET_DOMAIN="tail.example" bash "${SCRIPT}" render-candidate-smoke-commands >"${SMOKE_OUT}"
assert_contains "${SMOKE_OUT}" "'${FAKE_KUBECTL}' --context 'honey context' -n 'tcfs' rollout status 'statefulset/nats-openebs-candidate' --timeout=180s"
assert_contains "${SMOKE_OUT}" "'${FAKE_KUBECTL}' --context 'honey context' -n 'tcfs' get service 'nats-tailnet-candidate' 'seaweedfs-tailnet-candidate' -o wide"
assert_contains "${SMOKE_OUT}" "curl -fsS 'http://nats-tcfs-candidate.tail.example:8222/healthz'"
assert_contains "${SMOKE_OUT}" "curl -fsS 'http://seaweedfs-tcfs-candidate.tail.example:9333/cluster/status'"

CUTOVER_OUT="${TMPDIR}/cutover.out"
TCFS_CONTEXT="honey context" TCFS_KUBECTL="${FAKE_KUBECTL}" TCFS_TOFU="/opt/tofu bin/tofu" bash "${SCRIPT}" render-cutover-commands >"${CUTOVER_OUT}"
assert_contains "${CUTOVER_OUT}" "'${FAKE_KUBECTL}' --context 'honey context' -n 'tcfs' annotate 'service/nats' 'tailscale.com/expose-' 'tailscale.com/hostname-' 'tailscale.com/proxy-class-' --overwrite"
assert_contains "${CUTOVER_OUT}" "'/opt/tofu bin/tofu' -chdir='infra/tofu/environments/onprem' plan '-var=enable_stateful_migration_target_pvcs=true' '-var=enable_stateful_migration_candidate_workloads=true' '-var=enable_tailnet_candidate_services=true' '-var=nats_tailnet_candidate_hostname=nats-tcfs' '-var=seaweedfs_tailnet_candidate_hostname=seaweedfs-tcfs'"
assert_contains "${CUTOVER_OUT}" "curl -fsS 'http://nats-tcfs:8222/healthz'"

ROLLBACK_OUT="${TMPDIR}/rollback.out"
TCFS_CONTEXT="honey context" TCFS_KUBECTL="${FAKE_KUBECTL}" bash "${SCRIPT}" render-rollback-commands >"${ROLLBACK_OUT}"
assert_contains "${ROLLBACK_OUT}" "'${FAKE_KUBECTL}' --context 'honey context' -n 'tcfs' scale 'statefulset/nats-openebs-candidate' 'statefulset/seaweedfs-openebs-candidate' --replicas=0"
assert_contains "${ROLLBACK_OUT}" "'tofu' -chdir='infra/tofu/environments/onprem' apply '-var=enable_stateful_migration_target_pvcs=true' '-var=enable_stateful_migration_candidate_workloads=false' '-var=enable_tailnet_candidate_services=false'"
assert_contains "${ROLLBACK_OUT}" "'${FAKE_KUBECTL}' --context 'honey context' -n 'tcfs' annotate 'service/nats' 'tailscale.com/expose=true' 'tailscale.com/hostname=nats-tcfs' 'tailscale.com/proxy-class-' --overwrite"
assert_contains "${ROLLBACK_OUT}" "'${FAKE_KUBECTL}' --context 'honey context' -n 'tcfs' scale 'deployment/tcfs-backend-tcfs-backend-worker' --replicas=1"

if FAKE_BAD_TARGET_CLASS=1 TCFS_KUBECTL="${FAKE_KUBECTL}" bash "${SCRIPT}" facts >"${TMPDIR}/bad.out" 2>"${TMPDIR}/bad.err"; then
    printf 'expected mismatched target class to fail\n' >&2
    exit 1
fi
assert_contains "${TMPDIR}/bad.err" 'expected openebs-bumble-messaging-retain'

printf 'tcfs-onprem-migration-plan tests passed\n'
