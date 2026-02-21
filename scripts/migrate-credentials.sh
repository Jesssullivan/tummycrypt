#!/usr/bin/env bash
# scripts/migrate-credentials.sh
# One-time migration: extract plaintext creds from infra/ansible/inventory/main.yml
# → create SOPS-encrypted credentials/*.yaml files
# → redact infra/ansible/inventory/main.yml (replace values with sops_var refs)
#
# Prerequisites:
#   - Run scripts/sops-init.sh first (age key + .sops.yaml configured)
#   - sops and age CLIs available (nix develop)

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INVENTORY="${REPO_ROOT}/infra/ansible/inventory/main.yml"
CREDS_DIR="${REPO_ROOT}/credentials"

echo "==> TummyCrypt Credential Migration"
echo "    Reading from: ${INVENTORY}"
echo "    Writing to:   ${CREDS_DIR}/"
echo ""

# Check dependencies
for cmd in sops yq; do
    if ! command -v "${cmd}" &>/dev/null; then
        echo "ERROR: ${cmd} not found. Run 'nix develop' or install manually."
        exit 1
    fi
done

# Check .sops.yaml is configured
if grep -q "AGE_PUBLIC_KEY_PLACEHOLDER" "${REPO_ROOT}/.sops.yaml"; then
    echo "ERROR: .sops.yaml still has placeholder key. Run scripts/sops-init.sh first."
    exit 1
fi

# Extract credentials from inventory using yq
echo "==> Extracting credentials from Ansible inventory..."

ACCESS_KEY_JESS=$(yq '.all.vars.tummy_access_key_jess' "${INVENTORY}" 2>/dev/null || echo "")
SECRET_KEY_JESS=$(yq '.all.vars.tummy_secret_key_jess' "${INVENTORY}" 2>/dev/null || echo "")
ACCESS_KEY_KATE=$(yq '.all.vars.tummy_access_key_kate' "${INVENTORY}" 2>/dev/null || echo "")
SECRET_KEY_KATE=$(yq '.all.vars.tummy_secret_key_kate' "${INVENTORY}" 2>/dev/null || echo "")
ACCESS_KEY_FRIENDS=$(yq '.all.vars.tummy_access_key_friends' "${INVENTORY}" 2>/dev/null || echo "")
SECRET_KEY_FRIENDS=$(yq '.all.vars.tummy_secret_key_friends' "${INVENTORY}" 2>/dev/null || echo "")
ACCESS_KEY_PUBLIC=$(yq '.all.vars.tummy_access_key_public' "${INVENTORY}" 2>/dev/null || echo "")
SECRET_KEY_PUBLIC=$(yq '.all.vars.tummy_secret_key_public' "${INVENTORY}" 2>/dev/null || echo "")
RCLONE_PASSWORD1=$(yq '.all.vars.tummy_rclone_password1' "${INVENTORY}" 2>/dev/null || echo "")
RCLONE_PASSWORD2=$(yq '.all.vars.tummy_rclone_password2' "${INVENTORY}" 2>/dev/null || echo "")
JWT_KEY=$(yq '.all.vars.jwt_key' "${INVENTORY}" 2>/dev/null || echo "")

# Write seaweedfs-admin.yaml (unencrypted first, then sops encrypt)
echo "==> Writing credentials/seaweedfs-admin.yaml..."
cat > "${CREDS_DIR}/seaweedfs-admin.yaml" <<EOF
# SeaweedFS admin credentials
# Encrypted with SOPS+age. Non-sensitive fields visible in git diff.
region: us-east-1
endpoint: http://dees-appu-bearts:8333
filer_endpoint: http://192.168.101.146:8888
jwt_signing_key: ${JWT_KEY}
rclone_password1: ${RCLONE_PASSWORD1}
rclone_password2: ${RCLONE_PASSWORD2}
EOF
sops --encrypt --in-place "${CREDS_DIR}/seaweedfs-admin.yaml"
echo "    Encrypted: credentials/seaweedfs-admin.yaml"

# Write per-user credential files
write_user_creds() {
    local username="$1"
    local access_key="$2"
    local secret_key="$3"

    echo "==> Writing credentials/seaweedfs-users/${username}.yaml..."
    cat > "${CREDS_DIR}/seaweedfs-users/${username}.yaml" <<EOF
# SeaweedFS S3 credentials for user: ${username}
# Encrypted with SOPS+age.
username: ${username}
access_key_id: ${access_key}
secret_access_key: ${secret_key}
EOF
    sops --encrypt --in-place "${CREDS_DIR}/seaweedfs-users/${username}.yaml"
    echo "    Encrypted: credentials/seaweedfs-users/${username}.yaml"
}

write_user_creds "jess" "${ACCESS_KEY_JESS}" "${SECRET_KEY_JESS}"
write_user_creds "kate" "${ACCESS_KEY_KATE}" "${SECRET_KEY_KATE}"
write_user_creds "friends" "${ACCESS_KEY_FRIENDS}" "${SECRET_KEY_FRIENDS}"
write_user_creds "public" "${ACCESS_KEY_PUBLIC}" "${SECRET_KEY_PUBLIC}"

# Redact infra/ansible/inventory/main.yml
echo ""
echo "==> Redacting plaintext credentials from infra/ansible/inventory/main.yml..."
cat > "${REPO_ROOT}/infra/ansible/inventory/main.yml" <<'INVENTORY_EOF'
---
# Ansible inventory for TummyCrypt SeaweedFS cluster
# SECURITY: Credentials have been migrated to SOPS-encrypted files.
# Load credentials at runtime:
#   sops exec-env credentials/seaweedfs-admin.yaml 'ansible-playbook ...'
# Or use ansible-vault for per-host secrets.

all:
  vars:
    # Credentials loaded from SOPS-encrypted files (credentials/*.yaml)
    # Use: sops exec-env credentials/seaweedfs-users/jess.yaml 'ansible-playbook ...'
    tummy_access_key_jess: "{{ lookup('env', 'ACCESS_KEY_ID') }}"
    tummy_secret_key_jess: "{{ lookup('env', 'SECRET_ACCESS_KEY') }}"
    tummy_access_key_kate: "{{ lookup('env', 'ACCESS_KEY_ID') }}"
    tummy_secret_key_kate: "{{ lookup('env', 'SECRET_ACCESS_KEY') }}"
    tummy_access_key_friends: "{{ lookup('env', 'ACCESS_KEY_ID') }}"
    tummy_secret_key_friends: "{{ lookup('env', 'SECRET_ACCESS_KEY') }}"
    tummy_rclone_password1: "{{ lookup('env', 'RCLONE_PASSWORD1') }}"
    tummy_rclone_password2: "{{ lookup('env', 'RCLONE_PASSWORD2') }}"
    tummy_access_key_public: "{{ lookup('env', 'ACCESS_KEY_ID') }}"
    tummy_secret_key_public: "{{ lookup('env', 'SECRET_ACCESS_KEY') }}"
    jwt_key: "{{ lookup('env', 'JWT_SIGNING_KEY') }}"

masters:
  hosts:
    192.168.101.249:
    192.168.101.184:
    192.168.101.248:
  vars:
    ip1: 192.168.101.249
    ip2: 192.168.101.184
    ip3: 192.168.101.248

volumes:
  hosts:
    192.168.101.171:
  vars:
    ms_ip1: 192.168.101.249
    ms_ip2: 192.168.101.184
    ms_ip3: 192.168.101.248
    drobo_vol_port: 8080
    hdd_mnt_1: /mnt/usb-Drobo_5C_D0A164802100235-0:0-part1

filers:
  hosts:
    # drobo host:
    192.168.101.146:
  vars:
    ms_ip1: 192.168.101.249
    ms_ip2: 192.168.101.184
    ms_ip3: 192.168.101.248
INVENTORY_EOF

echo "    Redacted: infra/ansible/inventory/main.yml"

echo ""
echo "==> Migration complete!"
echo ""
echo "    Encrypted files:"
echo "      credentials/seaweedfs-admin.yaml"
echo "      credentials/seaweedfs-users/jess.yaml"
echo "      credentials/seaweedfs-users/kate.yaml"
echo "      credentials/seaweedfs-users/friends.yaml"
echo "      credentials/seaweedfs-users/public.yaml"
echo ""
echo "    Redacted: infra/ansible/inventory/main.yml"
echo ""
echo "    IMPORTANT: Also redact the original hosts/main.yml:"
echo "    The plaintext version at hosts/main.yml must be removed or redacted."
echo "    After verifying credentials work, delete hosts/main.yml."
echo ""
echo "    Verify with: sops decrypt credentials/seaweedfs-admin.yaml"
