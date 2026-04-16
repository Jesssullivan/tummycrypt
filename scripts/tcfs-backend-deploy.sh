#!/usr/bin/env bash
#
# tcfs-backend-deploy.sh — reconcile the direct tcfs-backend Helm release
#
# Usage:
#   ./scripts/tcfs-backend-deploy.sh
#   ./scripts/tcfs-backend-deploy.sh --dry-run
#   TCFS_RELEASE_NAME=tcfs-backend ./scripts/tcfs-backend-deploy.sh --set image.tag=v0.12.2
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
CHART_DIR="${REPO_ROOT}/infra/k8s/charts/tcfs-backend"

RELEASE_NAME="${TCFS_RELEASE_NAME:-tcfs-backend}"
NAMESPACE="${TCFS_NAMESPACE:-tcfs}"
DRY_RUN=false
EXTRA_ARGS=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --dry-run)
            DRY_RUN=true
            shift
            ;;
        *)
            EXTRA_ARGS+=("$1")
            shift
            ;;
    esac
done

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

info()  { printf "${GREEN}[INFO]${NC}  %s\n" "$*"; }
warn()  { printf "${YELLOW}[WARN]${NC}  %s\n" "$*"; }
error() { printf "${RED}[ERROR]${NC} %s\n" "$*" >&2; }

check_command() {
    if ! command -v "$1" >/dev/null 2>&1; then
        error "$1 is required but not found in PATH"
        exit 1
    fi
}

info "Checking prerequisites..."
check_command helm
check_command kubectl

if ! kubectl cluster-info >/dev/null 2>&1; then
    error "Cannot connect to Kubernetes cluster. Check your kubeconfig."
    exit 1
fi

HELM_CMD=(
    helm upgrade --install "${RELEASE_NAME}" "${CHART_DIR}"
    --namespace "${NAMESPACE}"
    --create-namespace
    -f "${CHART_DIR}/values.yaml"
)

if [[ "${DRY_RUN}" == "true" ]]; then
    info "Dry-run mode enabled — no changes will be applied"
    HELM_CMD+=(--dry-run --debug)
fi

if [[ ${#EXTRA_ARGS[@]} -gt 0 ]]; then
    HELM_CMD+=("${EXTRA_ARGS[@]}")
fi

info "Reconciling direct tcfs-backend release..."
info "  Release:   ${RELEASE_NAME}"
info "  Namespace: ${NAMESPACE}"
info "  Chart:     ${CHART_DIR}"
echo

"${HELM_CMD[@]}"

if [[ "${DRY_RUN}" == "false" ]]; then
    DEPLOYMENT_NAME="${RELEASE_NAME}-tcfs-backend-worker"
    SERVICE_ACCOUNT_NAME="${RELEASE_NAME}-tcfs-backend"
    echo
    info "Validating Helm-managed scaffolding..."
    kubectl get serviceaccount "${SERVICE_ACCOUNT_NAME}" --namespace "${NAMESPACE}" \
        >/dev/null 2>&1 || warn "service account ${SERVICE_ACCOUNT_NAME} not visible yet"
    kubectl rollout status deployment/"${DEPLOYMENT_NAME}" \
        --namespace "${NAMESPACE}" \
        --timeout=120s 2>/dev/null || warn "${DEPLOYMENT_NAME} rollout not ready yet"
    echo
    info "Helm release state:"
    helm list --namespace "${NAMESPACE}" || true
fi
