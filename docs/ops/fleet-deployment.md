# Fleet Deployment Guide

Operational guide for deploying tcfs across a multi-machine fleet.
Companion to [RFC 0001: Fleet Sync Integration](../rfc/0001-fleet-sync-integration.md).

## Prerequisites

- tcfs v0.3.0+ installed on all machines
- SeaweedFS S3 reachable from all machines (verified: `dees-appu-bearts:8333`)
- Each machine enrolled: `tcfs device enroll --name $(hostname)`
- All fleet machines on the same Tailscale tailnet

---

## 1. NATS Access Path

NATS JetStream runs in the Civo K8s cluster (`nats.tcfs.svc.cluster.local:4222`).
Lab machines access it via the Tailscale operator — no public IP, tailnet only.

### Tailscale Exposure (Recommended)

The `tailscale-nats` OpenTofu module creates a Tailscale-only LoadBalancer service:

```bash
# Deploy via IaC
just tofu-apply env=civo

# Or manually
cd infra/tofu/environments/civo && tofu apply
```

This creates a `LoadBalancer` service with `loadBalancerClass: tailscale` and
hostname annotation `nats-tcfs`. The Tailscale operator picks it up and exposes
NATS as a tailnet device.

Lab machines connect via MagicDNS:
```
nats://nats-tcfs:4222
```

Optionally, add a DNS alias in the Tailscale admin console to use a custom domain:
```
nats://nats.tcfs.tinyland.dev:4222
```

### Connectivity Verification

```bash
# Test NATS connectivity via Tailscale
just nats-status

# Or manually
nats server info --server nats://nats-tcfs:4222

# Check JetStream streams
just nats-streams

# Publish a test ping
just nats-ping

# tcfs daemon connectivity
tcfs status
tcfs sync-status
```

### Fallback Behavior

tcfs works without NATS. If NATS is unreachable:
- Push/pull operations proceed normally (S3 only)
- State events are not published (no real-time notification)
- Other machines must manually `tcfs pull` to see updates
- When NATS reconnects, the daemon automatically resumes event publishing

---

## 2. Credential Distribution

### Credential Precedence

tcfs resolves S3 credentials in this order (first match wins):

1. **SOPS-encrypted file**: `storage.credentials_file` in config.toml (decrypted with age identity)
2. **RemoteJuggler KDBX**: KeePassXC database via `remote-juggler kdbx get` (if `$REMOTE_JUGGLER_IDENTITY` is set)
3. **Environment variables** (in priority order):
   - `TCFS_S3_ACCESS` / `TCFS_S3_SECRET` (tcfs-native, recommended)
   - Standard AWS S3 credential env vars (access key ID / secret)
   - `SEAWEED_ACCESS_KEY` / `SEAWEED_SECRET_KEY` (SeaweedFS-specific)

### Creating Per-Host Age Keys

```bash
# On each machine (one-time setup)
age-keygen -o ~/.config/sops/age/keys.txt

# Show public key (needed for .sops.yaml)
age-keygen -y ~/.config/sops/age/keys.txt
# → age1ql3z7hjy54pw3hyww5ayyfg7zqgvc7w3j2elw8zmrj2kg5sfn9aqmcac8p
```

### Encrypting Credentials with SOPS

Create a SOPS-encrypted secrets file per host:

```bash
# .sops.yaml (in repo root)
creation_rules:
  - path_regex: secrets/hosts/yoga\.yaml$
    age: >-
      age1...yoga_public_key
  - path_regex: secrets/hosts/xoxd-bates\.yaml$
    age: >-
      age1...xoxd_bates_public_key
  - path_regex: secrets/hosts/petting-zoo-mini\.yaml$
    age: >-
      age1...petting_zoo_mini_public_key
```

```yaml
# secrets/hosts/yoga.yaml (before encryption)
s3_access: "<access-credential>"
s3_secret: "<secret-credential>"
s3_endpoint: "http://dees-appu-bearts:8333"
nats_url: "nats://nats-tcfs:4222"
```

```bash
# Encrypt with SOPS
sops --encrypt --in-place secrets/hosts/yoga.yaml
```

### Deploying via sops-nix

For NixOS/Home Manager machines:

```nix
# In crush-dots host config
sops.secrets."tcfs/s3_access" = {
  sopsFile = ./secrets/hosts/yoga.yaml;
  key = "s3_access";
};

# tcfs reads from /run/secrets/tcfs/s3_access
```

### Deploying via Environment Variables

For non-NixOS machines:

```bash
# ~/.config/tcfs/env (sourced by systemd/launchd)
TCFS_S3_ACCESS=<your-access-credential>
TCFS_S3_SECRET=<your-secret-credential>
TCFS_S3_ENDPOINT=http://dees-appu-bearts:8333
TCFS_NATS_URL=nats://nats-tcfs:4222
```

### Credential Rotation

```bash
# 1. Generate new S3 credentials in SeaweedFS
curl -X POST http://dees-appu-bearts:8333/admin/keys \
  -d '{"accessKey":"new-key","secretKey":"new-secret"}'

# 2. Update SOPS secrets on each host
sops secrets/hosts/yoga.yaml
# Edit s3_access and s3_secret values

# 3. Re-encrypt
sops --encrypt --in-place secrets/hosts/yoga.yaml

# 4. Deploy
# NixOS: nixos-rebuild switch
# Non-NixOS: copy env file, restart tcfsd
```

---

## 3. Automatic Daemon Startup

### Home Manager (All Platforms — Recommended)

The Home Manager module handles both Linux (systemd) and macOS (launchd) automatically:

```nix
# In your Home Manager configuration
programs.tcfs = {
  enable = true;
  package = pkgs.tcfsd;
  identity = "~/.config/sops/age/keys.txt";
  deviceName = "yoga";
  conflictMode = "interactive";
  natsUrl = "nats://nats-tcfs:4222";
  syncRoot = "~/tcfs";
  mounts = [
    { remote = "seaweedfs://dees-appu-bearts:8333/tcfs"; local = "~/tcfs"; }
  ];
};
```

On Linux, this creates a `systemd.user.services.tcfsd` unit.
On macOS, this creates a `launchd.agents.tcfsd` agent.

See `examples/lab-fleet/` for per-machine configurations.

### NixOS System Module

For system-level daemon (runs as dedicated user with hardening):

```nix
services.tcfsd = {
  enable = true;
  deviceName = "yoga";
  conflictMode = "interactive";
  natsUrl = "nats://nats-tcfs:4222";
  syncRoot = "/srv/tcfs";
};
```

### Linux (systemd, non-NixOS)

The systemd unit is installed at `/etc/systemd/system/tcfsd.service` or `~/.config/systemd/user/tcfsd.service`:

```bash
# System-level (runs as dedicated user)
sudo systemctl enable tcfsd
sudo systemctl start tcfsd

# User-level
systemctl --user enable tcfsd
systemctl --user start tcfsd
```

### macOS (launchd, non-Nix)

For manual installs without Home Manager:

```bash
# Copy plist
cp dist/com.tummycrypt.tcfsd.plist ~/Library/LaunchAgents/

# Load (starts immediately and on login)
launchctl load ~/Library/LaunchAgents/com.tummycrypt.tcfsd.plist

# Verify running
launchctl list | grep tcfs
```

### Troubleshooting

```bash
# Linux: check daemon logs
journalctl --user -u tcfsd -f
journalctl --user -u tcfsd --since "5 min ago"

# macOS: check daemon logs
tail -f /tmp/tcfsd.stdout.log
tail -f /tmp/tcfsd.stderr.log

# macOS: check launchd status
launchctl list | grep tcfs
# PID > 0 means running, "-" means not running
# Status 0 = exited cleanly, non-zero = error

# Check gRPC socket
tcfs status

# Check NATS connectivity
tcfs sync-status
```

---

## 4. IaC Operations

All Civo infrastructure is managed via OpenTofu. Use the Justfile for common operations:

```bash
# List all recipes
just --list

# Plan and apply infrastructure changes
just tofu-plan
just tofu-apply

# Check cluster status
just k8s-status

# Check NATS
just nats-status
just nats-streams

# View logs
just k8s-logs app=tcfsd
```

The Justfile is at the project root. All recipes use the `civo` environment by default.
