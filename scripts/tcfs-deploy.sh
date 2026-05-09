#!/usr/bin/env bash
#
# tcfs-deploy.sh — deploy the tcfs-stack Helm umbrella chart
#
# Usage:
#   ./scripts/tcfs-deploy.sh                    # default chart values
#   ./scripts/tcfs-deploy.sh --dev              # development overlay
#   ./scripts/tcfs-deploy.sh --dry-run          # template only, no install
#   ./scripts/tcfs-deploy.sh --set global.imageTag=v0.12.12
#
# This is the blank TCFS control-plane umbrella path with external SeaweedFS.
# For reconciling an already-existing direct `tcfs-backend` Helm release, use
# `scripts/tcfs-backend-deploy.sh`.
# Set an explicit release tag for proof or production runs; mutable defaults are
# convenience values only.
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
CHART_DIR="${REPO_ROOT}/infra/k8s/charts/tcfs-stack"

RELEASE_NAME="${TCFS_RELEASE_NAME:-tcfs}"
NAMESPACE="${TCFS_NAMESPACE:-tcfs}"
KUBE_CONTEXT="${TCFS_CONTEXT:-${TCFS_KUBE_CONTEXT:-}}"

DEV_MODE=false
DRY_RUN=false
EXTRA_ARGS=()

# ── Parse arguments ──────────────────────────────────────
while [[ $# -gt 0 ]]; do
    case "$1" in
        --dev)
            DEV_MODE=true
            NAMESPACE="${TCFS_NAMESPACE:-tcfs-dev}"
            shift
            ;;
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

# ── Colour helpers ───────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

info()  { printf "${GREEN}[INFO]${NC}  %s\n" "$*"; }
warn()  { printf "${YELLOW}[WARN]${NC}  %s\n" "$*"; }
error() { printf "${RED}[ERROR]${NC} %s\n" "$*" >&2; }

# ── Prerequisite checks ─────────────────────────────────
check_command() {
    if ! command -v "$1" &>/dev/null; then
        error "$1 is required but not found in PATH"
        exit 1
    fi
}

info "Checking prerequisites..."
check_command helm
check_command kubectl

if [[ -n "${KUBE_CONTEXT}" ]]; then
    info "Using Kubernetes context: ${KUBE_CONTEXT}"
    KUBECTL=(kubectl --context "${KUBE_CONTEXT}")
    HELM_CONTEXT_ARGS=(--kube-context "${KUBE_CONTEXT}")
else
    warn "TCFS_CONTEXT not set; using the current kube context"
    KUBECTL=(kubectl)
    HELM_CONTEXT_ARGS=()
fi

# Verify cluster connectivity
if ! "${KUBECTL[@]}" cluster-info &>/dev/null; then
    error "Cannot connect to Kubernetes cluster. Check your kubeconfig."
    exit 1
fi
info "Cluster connection OK"

# ── Ensure namespace exists ──────────────────────────────
if ! "${KUBECTL[@]}" get namespace "${NAMESPACE}" &>/dev/null; then
    info "Namespace '${NAMESPACE}' does not exist — it will be created by the chart"
fi

# ── Update Helm dependencies ────────────────────────────
info "Updating Helm dependencies..."
helm dependency update "${CHART_DIR}"

# ── Build Helm command ──────────────────────────────────
HELM_CMD=(
    helm upgrade --install "${RELEASE_NAME}" "${CHART_DIR}"
    "${HELM_CONTEXT_ARGS[@]}"
    --namespace "${NAMESPACE}"
    --create-namespace
    -f "${CHART_DIR}/values.yaml"
)

if [[ "${DEV_MODE}" == "true" ]]; then
    info "Using development overlay (values-dev.yaml)"
    HELM_CMD+=(-f "${CHART_DIR}/values-dev.yaml")
fi

if [[ "${DRY_RUN}" == "true" ]]; then
    info "Dry-run mode enabled — no changes will be applied"
    HELM_CMD+=(--dry-run --debug)
fi

if [[ ${#EXTRA_ARGS[@]} -gt 0 ]]; then
    HELM_CMD+=("${EXTRA_ARGS[@]}")
fi

# ── Deploy ──────────────────────────────────────────────
info "Deploying tcfs-stack..."
info "  Release:   ${RELEASE_NAME}"
info "  Namespace: ${NAMESPACE}"
info "  Chart:     ${CHART_DIR}"
echo

"${HELM_CMD[@]}"

# ── Post-deploy verification (skip on dry-run) ─────────
if [[ "${DRY_RUN}" == "false" ]]; then
    echo
    info "Waiting for rollout..."
    "${KUBECTL[@]}" rollout status deployment/"${RELEASE_NAME}-tcfs-backend-worker" \
        --namespace "${NAMESPACE}" \
        --timeout=120s 2>/dev/null || warn "tcfs-backend rollout not ready yet"
    echo
    info "Pod status:"
    "${KUBECTL[@]}" get pods --namespace "${NAMESPACE}" -l "app.kubernetes.io/managed-by=Helm"
    echo
    info "Deployment complete."
fi
