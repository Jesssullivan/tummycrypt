#!/usr/bin/env bash
#
# Static regression tests for the source-only TCFS on-prem candidate workload
# surface. These tests intentionally avoid kubectl/tofu apply; their job is to
# catch authority regressions before an operator reaches a live cluster.
#
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MAIN_TF="${ROOT}/infra/tofu/environments/onprem/main.tf"
VARIABLES_TF="${ROOT}/infra/tofu/environments/onprem/variables.tf"
OUTPUTS_TF="${ROOT}/infra/tofu/environments/onprem/outputs.tf"

assert_contains() {
    local file="$1"
    local needle="$2"

    if ! grep -Fq "${needle}" "${file}"; then
        printf '[ERROR] expected %s to contain: %s\n' "${file}" "${needle}" >&2
        exit 1
    fi
}

assert_not_contains() {
    local file="$1"
    local needle="$2"

    if grep -Fq "${needle}" "${file}"; then
        printf '[ERROR] expected %s not to contain: %s\n' "${file}" "${needle}" >&2
        exit 1
    fi
}

assert_contains "${VARIABLES_TF}" 'variable "enable_stateful_migration_candidate_workloads"'
assert_contains "${MAIN_TF}" 'resource "kubernetes_stateful_set_v1" "nats_candidate"'
assert_contains "${MAIN_TF}" 'resource "kubernetes_stateful_set_v1" "seaweedfs_candidate"'
assert_contains "${MAIN_TF}" 'selector           = local.nats_candidate_selector'
assert_contains "${MAIN_TF}" 'selector           = local.seaweedfs_candidate_selector'
assert_contains "${MAIN_TF}" 'claim_name = var.nats_target_pvc_name'
assert_contains "${MAIN_TF}" 'claim_name = var.seaweedfs_target_pvc_name'
assert_contains "${OUTPUTS_TF}" 'output "stateful_migration_candidate_workloads"'

assert_not_contains "${MAIN_TF}" 'selector           = { app = "nats" }'
assert_not_contains "${MAIN_TF}" 'selector           = { app = "seaweedfs" }'

printf '[INFO] TCFS on-prem candidate workload static checks passed\n'
