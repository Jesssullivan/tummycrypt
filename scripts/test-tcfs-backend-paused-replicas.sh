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

printf '[OK] tcfs-backend chart declares the TIN-5136 pause and a renderable restore\n'
