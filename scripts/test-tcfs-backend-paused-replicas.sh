#!/usr/bin/env bash
#
# Regression test for the honey tcfs cluster pause (operator ruling 2026-09-29
# "Close the tcfs source drift", TIN-5136). The live tcfs workloads were
# scaled to 0 on 2026-09-29T16:45Z; the tcfs-backend chart defaults must keep a
# reconcile from restarting the worker, and the restore path must still render.
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
        -f "${CHART_DIR}/values.yaml" "$@"
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

restored="$(render --set replicaCount=1 --set autoscaling.minReplicas=1)"
[[ "$(printf '%s\n' "${restored}" | field_values replicas)" == "1" ]] \
    || fail "restore render must declare replicas: 1"
[[ "$(printf '%s\n' "${restored}" | field_values minReplicaCount)" == "1" ]] \
    || fail "restore render must declare minReplicaCount: 1"

[[ -f "${PAUSE_DOC}" ]] || fail "missing pause record ${PAUSE_DOC}"
for needle in TIN-5136 "Close the tcfs source drift" data-seaweedfs-0 data-nats-0 \
    tcfs-s3-posture-gateway tcfs-s3-smoke-tunnel; do
    grep -Fq -- "${needle}" "${PAUSE_DOC}" || fail "pause record must mention ${needle}"
done

# Restore order: seaweedfs first, then nats, then the Deployments.
order="$(grep -n -E '^[0-9]+\. ' "${PAUSE_DOC}" | grep -E 'statefulset/(seaweedfs|nats)|tcfs-backend-worker' | cut -d: -f1 | tr '\n' ' ')"
sw="$(grep -n -E '^[0-9]+\. .*statefulset/seaweedfs' "${PAUSE_DOC}" | head -1 | cut -d: -f1)"
nt="$(grep -n -E '^[0-9]+\. .*statefulset/nats' "${PAUSE_DOC}" | head -1 | cut -d: -f1)"
wk="$(grep -n -E '^[0-9]+\. .*tcfs-backend-worker' "${PAUSE_DOC}" | head -1 | cut -d: -f1)"
[[ -n "${sw}" && -n "${nt}" && -n "${wk}" ]] || fail "pause record must list restore steps (found lines: ${order})"
(( sw < nt && nt < wk )) || fail "restore order must be seaweedfs, then nats, then the Deployments"

# Captured manifests for the workloads that had no source declaration.
MANIFEST_DIR="${ROOT}/infra/k8s/honey/tcfs-paused"
expected=(
    "01-seaweedfs-statefulset.yaml:StatefulSet:seaweedfs"
    "02-nats-statefulset.yaml:StatefulSet:nats"
    "03-tcfs-s3-posture-gateway-deployment.yaml:Deployment:tcfs-s3-posture-gateway"
    "04-tcfs-s3-smoke-tunnel-deployment.yaml:Deployment:tcfs-s3-smoke-tunnel"
)
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

printf '[OK] tcfs-backend chart declares the TIN-5136 pause and a renderable restore\n'
printf '[OK] captured tcfs manifests declare replicas 0 without server-set fields\n'
