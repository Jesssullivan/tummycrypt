#!/usr/bin/env bash
#
# Regression tests for tcfs-onprem-preflight.sh.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="${REPO_ROOT}/scripts/tcfs-onprem-preflight.sh"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "${TMPDIR}"' EXIT

cat >"${TMPDIR}/helm" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "--kube-context" ]]; then
    shift 2
fi

if [[ "${1:-}" == "list" ]]; then
    printf 'NAME\tNAMESPACE\tREVISION\tUPDATED\tSTATUS\tCHART\tAPP VERSION\n'
    exit 0
fi

exit 1
EOF
chmod +x "${TMPDIR}/helm"

cat >"${TMPDIR}/kubectl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "--context" ]]; then
    shift 2
fi

if [[ "${1:-}" == "-n" ]]; then
    shift 2
fi

if [[ "${1:-}" == "config" ]]; then
    printf 'honey\n'
    exit 0
fi

[[ "${1:-}" == "get" ]] || exit 1
resource="${2:-}"
shift 2

output=""
while [[ "$#" -gt 0 ]]; do
    case "$1" in
        -o)
            output="$2"
            shift 2
            ;;
        --ignore-not-found|--no-headers)
            shift
            ;;
        *)
            shift
            ;;
    esac
done

case "${resource}:${output}" in
    "svc/nats:jsonpath={.metadata.annotations.tailscale\\.com/expose}") printf 'true' ;;
    "svc/nats:jsonpath={.metadata.annotations.tailscale\\.com/hostname}") printf 'nats-tcfs' ;;
    "svc/nats:jsonpath={.metadata.annotations.tailscale\\.com/proxy-class}") ;;
    "svc/seaweedfs:jsonpath={.metadata.annotations.tailscale\\.com/expose}") printf 'true' ;;
    "svc/seaweedfs:jsonpath={.metadata.annotations.tailscale\\.com/hostname}") printf 'seaweedfs-tcfs' ;;
    "svc/seaweedfs:jsonpath={.metadata.annotations.tailscale\\.com/proxy-class}") ;;
    "pvc/data-nats-0:jsonpath={.spec.storageClassName}") printf 'local-path' ;;
    "pvc/data-nats-0:jsonpath={.spec.volumeName}") printf 'pv-nats' ;;
    "pvc/data-seaweedfs-0:jsonpath={.spec.storageClassName}") printf 'local-path' ;;
    "pvc/data-seaweedfs-0:jsonpath={.spec.volumeName}") printf 'pv-seaweedfs' ;;
    pv/*:*) printf '%s   Retain   honey   /opt/local-path-provisioner/%s\n' "${resource#pv/}" "${resource#pv/}" ;;
    pod|deploy) ;;
    *) ;;
esac
EOF
chmod +x "${TMPDIR}/kubectl"

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

assert_not_contains() {
    local file="$1"
    local unexpected="$2"

    if grep -Fq "${unexpected}" "${file}"; then
        printf 'did not expect to find %s in %s\n' "${unexpected}" "${file}" >&2
        printf '--- output ---\n' >&2
        cat "${file}" >&2
        exit 1
    fi
}

OUT="${TMPDIR}/preflight.out"
PATH="${TMPDIR}:${PATH}" TCFS_CONTEXT=honey bash "${SCRIPT}" >"${OUT}"

assert_contains "${OUT}" '[WARN] NATS/SeaweedFS tailnet exposure is missing proxy-class ownership.'
assert_contains "${OUT}" '[WARN] Use the source-owned OpenTofu migration path before changing live authority.'
assert_contains "${OUT}" '[WARN] Run storage/data movement and canonical hostname cutover only during an approved downtime window.'
assert_not_contains "${OUT}" 'Choose Helm adoption or OpenTofu migration before changing live authority.'

printf 'tcfs-onprem-preflight tests passed\n'
