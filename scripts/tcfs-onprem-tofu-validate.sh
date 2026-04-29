#!/usr/bin/env bash
#
# Validate the source-only TCFS on-prem OpenTofu migration surface.
#
# This is intentionally non-mutating: it initializes providers with
# -backend=false and runs tofu validate only. It also validates the legacy Civo
# environment because that path consumes the tailscale-nats wrapper.
#
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

require_command() {
    if ! command -v "$1" >/dev/null 2>&1; then
        printf '[ERROR] %s is required but was not found in PATH\n' "$1" >&2
        exit 1
    fi
}

validate_env() {
    local env="$1"
    local dir="${ROOT}/infra/tofu/environments/${env}"

    printf '[INFO] validating infra/tofu/environments/%s\n' "${env}"
    tofu -chdir="${dir}" init -backend=false
    tofu -chdir="${dir}" validate
}

require_command tofu

tofu fmt -check -recursive "${ROOT}/infra/tofu/environments/onprem"
bash "${ROOT}/scripts/test-tcfs-onprem-tofu-candidate-workloads.sh"

validate_env onprem
validate_env civo
