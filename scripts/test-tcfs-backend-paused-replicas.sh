#!/usr/bin/env bash
#
# Regression test for the honey tcfs cluster pause (operator ruling 2026-09-29
# "Close the tcfs source drift", TIN-5136). The live tcfs workloads were
# scaled to 0 on 2026-09-29T16:45Z; the tcfs-backend chart defaults must keep a
# reconcile from restarting the worker. Since 2026-10-15 the old stack is never
# restored (OI-1003-Q153, OI-1003-Q175).
# Renders only; never talks to a cluster.
#
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CHART_DIR="${ROOT}/infra/k8s/charts/tcfs-backend"
PAUSE_DOC="${ROOT}/docs/ops/tcfs-honey-cluster-pause-2026-09-29.md"

fail() {
    printf '[ERROR] %s\n' "$*" >&2
    exit 1
}

command -v helm >/dev/null 2>&1 || fail "helm is required"

render() {
    helm template tcfs-backend "${CHART_DIR}" --namespace tcfs \
        -f "${CHART_DIR}/values.yaml"
}

field_values() {
    # Print the value of every "<key>: N" line in the rendered stream.
    grep -E "^[[:space:]]+$1:[[:space:]]" | awk '{print $2}'
}

paused="$(render)"
[[ "$(printf '%s\n' "${paused}" | field_values replicas)" == "0" ]] \
    || fail "default render must declare the worker Deployment with replicas: 0"
[[ "$(printf '%s\n' "${paused}" | field_values minReplicaCount)" == "0" ]] \
    || fail "default render must keep the KEDA floor at minReplicaCount: 0"

[[ -f "${PAUSE_DOC}" ]] || fail "missing pause record ${PAUSE_DOC}"
for needle in TIN-5136 "Close the tcfs source drift" data-seaweedfs-0 data-nats-0 \
    tcfs-s3-posture-gateway tcfs-s3-smoke-tunnel; do
    grep -Fq -- "${needle}" "${PAUSE_DOC}" || fail "pause record must mention ${needle}"
done

# The old stack is never restored (OI-1003-Q153, OI-1003-Q175): the record
# must say so and must not carry revival commands.
grep -Fq "## Restore: superseded" "${PAUSE_DOC}" || fail "pause record must mark the restore superseded"
for needle in OI-1003-Q153 OI-1003-Q175 TIN-5268; do
    grep -Fq -- "${needle}" "${PAUSE_DOC}" || fail "pause record must cite ${needle}"
done
if grep -Eq 'apply -f infra/k8s/honey/tcfs-paused|scale deployment/|--replicas=1|replicaCount=1' "${PAUSE_DOC}"; then
    fail "pause record must not carry revival commands"
fi

# Captured manifests for the workloads that had no source declaration.
MANIFEST_DIR="${ROOT}/infra/k8s/honey/tcfs-paused"
expected=(
    "01-seaweedfs-statefulset.yaml:StatefulSet:seaweedfs"
    "02-nats-statefulset.yaml:StatefulSet:nats"
)
# Retired 2026-10-15 (OI-1003-Q178): an unauthenticated public tunnel and a
# write-enabled gateway to the old S3 must not come back into source.
for retired in 03-tcfs-s3-posture-gateway-deployment.yaml 04-tcfs-s3-smoke-tunnel-deployment.yaml; do
    [[ ! -e "${MANIFEST_DIR}/${retired}" ]] || fail "${retired} is retired and must stay out of source"
done
if grep -rEq 'cloudflared|trycloudflare' "${MANIFEST_DIR}"; then
    fail "${MANIFEST_DIR} must not declare a tunnel"
fi
[[ "$(find "${MANIFEST_DIR}" -name '*.yaml' | wc -l | tr -d ' ')" == "${#expected[@]}" ]] \
    || fail "${MANIFEST_DIR} must hold exactly the ${#expected[@]} captured manifests"
for entry in "${expected[@]}"; do
    IFS=: read -r file kind name <<<"${entry}"
    path="${MANIFEST_DIR}/${file}"
    [[ -f "${path}" ]] || fail "missing captured manifest ${path}"
    grep -Eq "^kind: ${kind}$" "${path}" || fail "${file} must be kind ${kind}"
    grep -Eq "^  name: ${name}$" "${path}" || fail "${file} must name ${name}"
    grep -Eq '^  namespace: tcfs$' "${path}" || fail "${file} must target namespace tcfs"
    [[ "$(field_values replicas <"${path}")" == "0" ]] || fail "${file} must declare replicas: 0"
    for server_set in '^status:' '^[[:space:]]+status:' 'uid:' 'resourceVersion:' \
        'creationTimestamp:' 'managedFields:' 'generation:' 'last-applied-configuration' \
        'deployment.kubernetes.io/revision' 'clusterIP'; do
        if grep -Eq "${server_set}" "${path}"; then
            fail "${file} still carries server-set field matching ${server_set}"
        fi
    done
    if grep -Eq '^kind: Secret$|^[[:space:]]+(data|stringData):' "${path}"; then
        fail "${file} must not carry Secret material"
    fi
    grep -Fq "${file}" "${PAUSE_DOC}" || fail "pause record must reference ${file}"
done

printf '[OK] tcfs-backend chart declares the TIN-5136 pause; the old-stack restore is superseded\n'
printf '[OK] captured tcfs manifests declare replicas 0 without server-set fields\n'
