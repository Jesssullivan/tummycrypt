#!/usr/bin/env bash
# scripts/sops-init.sh
# Bootstrap SOPS + age encryption for tummycrypt
# Run this ONCE on initial setup before migrating credentials
#
# Prerequisites: age and sops CLIs must be installed
#   nix develop (preferred) or:
#   Linux: sudo dnf install age sops
#   macOS: brew install age sops

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
AGE_KEY_DIR="${HOME}/.config/sops/age"
AGE_KEY_FILE="${AGE_KEY_DIR}/keys.txt"
SOPS_CONFIG="${REPO_ROOT}/.sops.yaml"

echo "==> TummyCrypt SOPS + age Bootstrap"
echo "    Repo: ${REPO_ROOT}"
echo ""

# Check dependencies
for cmd in age age-keygen sops; do
    if ! command -v "${cmd}" &>/dev/null; then
        echo "ERROR: ${cmd} not found. Run 'nix develop' or install manually."
        exit 1
    fi
done

# Generate age key if not exists
if [[ -f "${AGE_KEY_FILE}" ]]; then
    echo "==> Found existing age key at ${AGE_KEY_FILE}"
    AGE_PUBLIC_KEY=$(grep "^# public key:" "${AGE_KEY_FILE}" | awk '{print $NF}')
else
    echo "==> Generating new age key at ${AGE_KEY_FILE}"
    mkdir -p "${AGE_KEY_DIR}"
    chmod 700 "${AGE_KEY_DIR}"
    age-keygen -o "${AGE_KEY_FILE}"
    chmod 600 "${AGE_KEY_FILE}"
    AGE_PUBLIC_KEY=$(grep "^# public key:" "${AGE_KEY_FILE}" | awk '{print $NF}')
    echo "    Public key: ${AGE_PUBLIC_KEY}"
fi

echo ""
echo "==> Age public key: ${AGE_PUBLIC_KEY}"
echo ""

# Update .sops.yaml with actual public key
if grep -q "AGE_PUBLIC_KEY_PLACEHOLDER" "${SOPS_CONFIG}"; then
    echo "==> Updating .sops.yaml with public key..."
    sed -i "s/AGE_PUBLIC_KEY_PLACEHOLDER/${AGE_PUBLIC_KEY}/g" "${SOPS_CONFIG}"
    echo "    Done. Commit .sops.yaml (public key is safe to commit)."
else
    echo "==> .sops.yaml already contains a real key (no placeholder found)"
fi

echo ""
echo "==> Next steps:"
echo "    1. Run: scripts/migrate-credentials.sh"
echo "    2. Verify: sops decrypt credentials/seaweedfs-admin.yaml"
echo "    3. Commit credentials/ (SOPS-encrypted, safe to commit)"
echo "    4. Share your public key with teammates for multi-recipient setup"
echo ""
echo "    Your age public key (share this with teammates):"
echo "    ${AGE_PUBLIC_KEY}"
