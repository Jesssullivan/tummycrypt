# Fleet Deployment Guide

Operational guide for deploying tcfs across a multi-machine fleet.
Companion to [RFC 0001: Fleet Sync Integration](../rfc/0001-fleet-sync-integration.md).

## Prerequisites

- tcfs v0.3.0+ installed on all machines
- SeaweedFS S3 reachable from all machines (verified: `dees-appu-bearts:8333`)
- Each machine enrolled: `tcfs device enroll --name $(hostname)`

---

## 1. NATS Access Path

NATS JetStream runs in the Civo K8s cluster (`nats.tcfs.svc.cluster.local:4222`).
Lab machines need external access. Three options:

### Option A: NodePort / LoadBalancer (Simplest)

Expose NATS via Civo LoadBalancer:

```bash
# Create NATS LoadBalancer service
kubectl -n tcfs expose deployment nats \
  --type=LoadBalancer \
  --port=4222 \
  --target-port=4222 \
  --name=nats-external

# Get external IP
kubectl -n tcfs get svc nats-external -o jsonpath='{.status.loadBalancer.ingress[0].ip}'
```

**Pros**: Simplest setup, no VPN needed.
**Cons**: NATS port exposed to internet (add firewall rules), requires static IP, no offline resilience.

Firewall rules (Civo):
```bash
# Allow only lab IPs
civo firewall rule create tcfs-fw \
  --protocol tcp --startport 4222 --endport 4222 \
  --cidr "YOUR_LAB_PUBLIC_IP/32"
```

### Option B: WireGuard Tunnel (Secure)

Route NATS over a WireGuard mesh:

```bash
# On each lab machine
wg-quick up tcfs-mesh

# NATS URL uses WireGuard IP
natsUrl = "nats://10.0.0.1:4222"  # WireGuard peer address
```

**Pros**: Encrypted tunnel, works behind NAT, no exposed ports.
**Cons**: Requires VPN setup on all machines, adds latency, single point of failure if relay goes down.

### Option C: NATS Leaf Node (Recommended)

Run a NATS leaf node on one lab machine that connects upstream to the Civo cluster:

```bash
# Install NATS server on yoga
nix-env -iA nixpkgs.nats-server

# /etc/nats/leaf.conf
leafnodes {
  remotes [
    {
      url: "nats-leaf://nats.tcfs.svc.cluster.local:7422"
      credentials: "/etc/nats/tcfs.creds"
    }
  ]
}

listen: "0.0.0.0:4222"
jetstream {
  store_dir: "/var/lib/nats/jetstream"
  max_mem: 256MB
  max_file: 1GB
}

# Start
nats-server -c /etc/nats/leaf.conf
```

All lab machines connect to `nats://yoga.local:4222` (or `nats://192.168.101.X:4222`).

**Pros**: Lowest latency for LAN operations, offline resilience (leaf buffers messages), single upstream connection to cluster.
**Cons**: Requires running NATS on one lab machine, leaf node is a dependency.

### Connectivity Verification

```bash
# Test NATS connectivity
nats pub test "hello" --server nats://NATS_HOST:4222
nats sub test --server nats://NATS_HOST:4222

# Check JetStream status
nats stream ls --server nats://NATS_HOST:4222

# tcfs daemon connectivity
tcfs status  # Shows NATS connection state
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

tcfs resolves credentials in this order (first match wins):

1. **Environment variables**: `TCFS_S3_ACCESS`, `TCFS_S3_SECRET` (or standard AWS S3 credential env vars)
2. **SOPS-encrypted file**: `~/.config/tcfs/secrets.yaml` (decrypted at runtime)
3. **RemoteJuggler KDBX**: KeePassXC database via `keyring` crate
4. **Config file**: `~/.config/tcfs/config.toml` (plaintext, not recommended)

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
nats_url: "nats://yoga.local:4222"
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
TCFS_NATS_URL=nats://yoga.local:4222
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

### Linux (NixOS)

Already handled by the NixOS module:

```nix
# In host configuration
services.tcfsd = {
  enable = true;
  deviceName = "yoga";
  conflictMode = "interactive";
  natsUrl = "nats://yoga.local:4222";
};
```

This generates a systemd unit with `After=network-online.target` and restart-on-failure.

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

The unit file is at `crates/tcfsd/tcfsd.service` in the repo.

### macOS (launchd)

Install the launchd plist:

```bash
# Copy plist
cp dist/com.tummycrypt.tcfsd.plist ~/Library/LaunchAgents/

# Load (starts immediately and on login)
launchctl load ~/Library/LaunchAgents/com.tummycrypt.tcfsd.plist

# Verify running
launchctl list | grep tcfs
```

To unload:
```bash
launchctl unload ~/Library/LaunchAgents/com.tummycrypt.tcfsd.plist
```

The plist file is at `dist/com.tummycrypt.tcfsd.plist` in the repo.

### Startup Dependencies

```
network-online.target (Linux) / NetworkReady (macOS)
       │
       ▼
   NATS (optional — daemon starts without it, reconnects later)
       │
       ▼
     tcfsd
       │
       ├── gRPC socket: /run/tcfsd.sock (Linux) / /tmp/tcfsd.sock (macOS)
       ├── Metrics: http://localhost:9100/metrics
       └── FUSE mount (if configured)
```

### Troubleshooting

```bash
# Linux: check daemon logs
journalctl -u tcfsd -f
journalctl -u tcfsd --since "5 min ago"

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
