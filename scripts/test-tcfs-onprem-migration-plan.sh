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

if FAKE_BAD_TARGET_CLASS=1 TCFS_KUBECTL="${FAKE_KUBECTL}" bash "${SCRIPT}" facts >"${TMPDIR}/bad.out" 2>"${TMPDIR}/bad.err"; then
    printf 'expected mismatched target class to fail\n' >&2
    exit 1
fi
assert_contains "${TMPDIR}/bad.err" 'expected openebs-bumble-messaging-retain'

printf 'tcfs-onprem-migration-plan tests passed\n'
