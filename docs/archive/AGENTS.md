# AGENTS.md

## Overview

TummyCrypt is an experimental storage infrastructure for "friendlabbing" based on SeaweedFS. This is an Ansible-based deployment system that sets up a distributed storage solution with TLS encryption.

## Project Structure

```
tummycrypt/
├── hosts/main.yml              # Inventory file with host definitions and variables
├── deploy.yml                  # Deploy to all hosts
├── masters.yml                 # Deploy to master nodes only
├── filers.yml                  # Deploy to filer nodes only
├── volumes.yml                 # Deploy to volume nodes only
├── darwin.yml                  # Deploy to Darwin/macOS controller
├── roles/                      # Ansible roles
│   ├── seaweedfs-masters/     # SeaweedFS master deployment
│   ├── seaweedfs-filer/       # SeaweedFS filer deployment
│   ├── seaweedfs-volume-drobo/# SeaweedFS volume server deployment
│   └── ubuntu-client/         # Client rclone setup
├── certs/                      # TLS certificates and keys
├── scripts/setup_venv.sh      # Virtual environment setup
└── requirements.txt           # Python dependencies
```

## Deployment Commands

### Setup Environment
```bash
# Set up local Python virtual environment
bash scripts/setup_venv.sh
```

### Deploy to Infrastructure

All deployment commands require `-K` (ask for sudo password) and specify the remote user:

```bash
# Deploy to triplicated masters (3 nodes)
ansible-playbook -i hosts/ -K masters.yml -u "jess"

# Deploy to volume server (e.g., Drobo)
ansible-playbook -i hosts/ -K volumes.yml -u "jess"

# Deploy to filer nodes
ansible-playbook -i hosts/ -K filers.yml -u "jess"

# Deploy to all hosts
ansible-playbook -i hosts/ -K deploy.yml -u "jess"

# Deploy to Darwin/macOS controller
ansible-playbook -i hosts/ -K darwin.yml -u "jess"
```

## Architecture

### Components

1. **Masters (3 nodes)**: SeaweedFS master servers for metadata management
   - Triplicated for redundancy
   - Running sw-master.service

2. **Volumes**: Storage volume servers
   - Currently configured for Drobo storage
   - Running sw-volume.service

3. **Filers**: SeaweedFS filer servers providing S3-compatible API
   - Configured with S3 support
   - Running sw-filer.service

4. **Clients**: Ubuntu clients with rclone mounts
   - Mounted at `~/tummycrypt` via rclone crypt
   - Uses user systemd services

### Network Configuration

- All services run behind firewall with required ports open:
  - 8333/tcp, 1888/tcp, 8888/tcp, 19333/tcp, 9333/tcp (SeaweedFS)
  - 80/tcp, 443/tcp, 18080/tcp, 8080/tcp, 7333/tcp (additional services)

## Configuration

### Host Inventory (`hosts/main.yml`)

Contains:
- Host IP addresses for masters, volumes, and filers
- Access keys and secret keys for different users (jess, kate, friends, public)
- Rclone passwords for encryption
- JWT signing key
- Master IP references used across roles

### Role Defaults

Each role has `defaults/main.yml` with:
- `weed_release_url`: SeaweedFS download URL (version 3.80)
- `uname`: Default user name (jess)
- `reset`: Reset flag (true)
- `branch`: Git branch reference

### Key Variables

- Master IPs: `ip1`, `ip2`, `ip3` (used in templates)
- S3 credentials: `tummy_access_key_*`, `tummy_secret_key_*`
- Encryption passwords: `tummy_rclone_password1`, `tummy_rclone_password2`
- Drobo volume port: `drobo_vol_port`
- Drobo mount path: `hdd_mnt_1`

## SeaweedFS Configuration

### Filer Store

Filer uses LevelDB2 for metadata storage:
- Path: `/etc/swmaster/filerldb2`
- Configured in `roles/seaweedfs-filer/templates/filer.toml.j2`

### S3 Configuration

Filer provides S3-compatible API at endpoint `http://dees-appu-bearts:8333/`

Multiple S3 access keys are configured for different users:
- `jess`: Personal access
- `kate`: Personal access
- `friends`: Shared access
- `public`: Public access

## Certificate Management

TLS certificates are managed in the `certs/` directory:
- `SeaweedFS_CA.*`: Certificate authority files
- `master01.*`: Master node certificates
- `filer01.*`: Filer node certificates
- `volume01.*`: Volume node certificates
- `client01.*`: Client certificates

Certificates are distributed via `certs.tar.gz` during deployment.

## Client Setup

Ubuntu clients receive:
- Rclone installation via official install script
- Rclone config with S3 backend and crypt layer
- Systemd user service for auto-mounting encrypted filesystem
- Mount point: `~/tummycrypt`

### Rclone Configuration

Client mounts use layered configuration:
1. **tummy_blocks**: S3 backend to SeaweedFS
2. **tummyblocks_crypt**: Encrypted layer over S3
   - 50Mi chunk size
   - Two-password encryption scheme
3. **tummy_exposed**: Alias for exposed bucket
4. **tummy_pubis**: Alias within encrypted storage

## Testing and Validation

### Check Service Status
```bash
# On target hosts
systemctl status sw-master.service
systemctl status sw-filer.service
systemctl status sw-volume.service
systemctl --user status TummyMount.service
```

### View Logs
```bash
# Service logs
journalctl -u sw-master.service -b
journalctl -u sw-filer.service -b
journalctl -u sw-volume.service -b
journalctl --user -u TummyMount.service -b
```

### Test Storage
```bash
# Check mount point
ls ~/tummycrypt

# Test rclone
rclone ls tummyblocks_crypt:
```

## Important Gotchas

### Deployment Order Matters

1. Deploy masters first (triplicated setup required before other components)
2. Deploy volumes next
3. Deploy filers last

### SELinux

All roles set SELinux to permissive mode (`setenforce permissive`) before installation. This is done via shell commands in the tasks.

### Firewall Configuration

Firewall ports are opened via `firewall-cmd` with:
```bash
firewall-cmd --permanent --add-port={8333/tcp,1888/tcp,8888/tcp,19333/tcp,9333/tcp,80/tcp,443/tcp,18080/tcp,8080/tcp,7333/tcp}
firewall-cmd --reload
```

### Certificate Distribution

Certificates are copied as `certs.tar.gz` and unpacked on target hosts. The tarball must exist in the playbook directory before deployment.

### Service Management

After role execution, services are:
- Stopped during setup (where applicable)
- Started via systemd
- Enabled for auto-start on boot
- Systemd daemon reloaded (`systemctl daemon-reload`)

### Variable Usage

Variables from inventory are used across templates:
- Master IPs referenced as `{{ ms_ip1 }}`, `{{ ms_ip2 }}`, `{{ ms_ip3 }}`
- User name as `{{ uname }}`
- Credentials and passwords from `hosts/main.yml`

### RedHat/CentOS/Fedora Focus

The playbooks are designed for RPM-based distributions:
- Use `dnf` for package installation
- Use `firewall-cmd` for firewall management
- Install `wget` via `dnf -y install wget`

### Ansible Linter Diagnostics

The LSP shows schema errors for tasks (missing `block` property and `become: yes` string instead of boolean). These are false positives from the YAML schema validator - the Ansible tasks work correctly despite these warnings.

## Dependencies

### Python
- `ansible`: Core automation framework
- `cryptography`: For certificate/encryption operations
- `ansible-lint`: Linting Ansible playbooks
- `ansible-vault`: Secret management

### External Tools
- `weed` (SeaweedFS binary): Downloaded during deployment
- `rclone`: Installed on client machines
- `firewall-cmd`: For firewall management
- `systemctl`: For service management

## Common Tasks

### Update SeaweedFS Version

Edit `weed_release_url` in role defaults files:
- `roles/seaweedfs-masters/defaults/main.yml`
- `roles/seaweedfs-filer/defaults/main.yml`
- `roles/seaweedfs-volume-drobo/defaults/main.yml`

### Add New Hosts

1. Add IP to appropriate host group in `hosts/main.yml`
2. Update IP variables (`ip1`, `ip2`, `ip3`) if adding masters
3. Run the appropriate playbook

### Regenerate Certificates

Regenerate certificates in `certs/` directory, then:
```bash
tar -czf certs.tar.gz certs/
```

Run deployment playbooks to distribute new certificates.

### Troubleshooting Mount Issues

On client machines:
1. Check service status: `systemctl --user status TummyMount.service`
2. View logs: `journalctl --user -u TummyMount.service -b`
3. Verify rclone config: `cat ~/.config/rclone/rclone.conf`
4. Test S3 connectivity manually with rclone

## Development Notes

- No test suite is present
- Linting is available via `ansible-lint` (install via requirements.txt)
- Playbooks use `shell` module extensively for complex operations
- Services are installed to `/usr/local/bin/weed`
- Configuration files go to `/etc/seaweedfs/`
- Systemd services installed to `/usr/lib/systemd/system/`
