# tummycrypt / tcfs

**tcfs** (TummyCrypt FileSystem) is a FOSS, self-hosted replacement for proprietary cloud storage
clients (odrive, Dropbox desktop, etc.). It mounts remote SeaweedFS storage as a local FUSE
directory, with files appearing as zero-byte `.tc` stubs until accessed — at which point they
are transparently hydrated on demand.

**Status**: Active development. Core sync (push/pull), FUSE mount, CLI, TUI, K8s worker mode, and cross-platform CI are functional. E2E encryption in progress.

## What it does

- Mounts remote S3/SeaweedFS storage as a local directory
- Files appear as `.tc` stubs (zero bytes) until you open them
- Opening a `.tc` stub triggers on-demand download, replacing stub with real file
- `tcfs unsync <path>` converts a hydrated file back to a stub (reclaims disk)
- Sync is bidirectional, conflict-aware, git-friendly (BLAKE3 hashed, FastCDC chunked)
- Billions of small files, horizontal K8s backend, KEDA auto-scaling

## Architecture

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for full system diagram and component overview.

## Quick Start (local dev)

```bash
# Enter devShell (requires Nix)
nix develop

# Or install tools manually (see scripts/setup-dev.sh)

# Generate TLS certs + start SeaweedFS + NATS + Prometheus + Grafana
task dev

# Verify SeaweedFS is running
curl http://localhost:9333/cluster/status

# Verify NATS is running
nats server ping nats://localhost:4222
```

## Security: Credential Setup

Credentials are managed via SOPS-encrypted files with age keys:

```bash
# 1. Generate age key + configure .sops.yaml
task sops:init

# 2. Migrate credentials to SOPS-encrypted files
task sops:migrate

# 3. Verify encryption works
task sops:decrypt FILE=credentials/seaweedfs-admin.yaml
```

## Repository Structure

```
tummycrypt/
├── Cargo.toml              # Rust workspace root
├── flake.nix               # Nix devShell + packages
├── Taskfile.yaml           # Build tasks (task --list)
├── docker-compose.yml      # Local dev stack
├── .sops.yaml              # SOPS encryption rules
├── crates/                 # Rust workspace members
│   ├── tcfs-core/          # Shared types, config, proto
│   ├── tcfs-secrets/       # SOPS/age/KDBX integration
│   ├── tcfs-storage/       # OpenDAL + SeaweedFS
│   ├── tcfs-chunks/        # FastCDC, BLAKE3, zstd
│   ├── tcfs-sync/          # Sync engine + NATS
│   ├── tcfs-fuse/          # FUSE driver (Linux)
│   ├── tcfs-cloudfilter/   # Windows CFAPI (skeleton)
│   ├── tcfsd/              # Daemon binary
│   ├── tcfs-cli/           # CLI binary (tcfs)
│   └── tcfs-tui/           # TUI binary
├── credentials/            # SOPS-encrypted credentials
├── infra/
│   ├── ansible/            # SeaweedFS Ansible deployment
│   ├── tofu/               # OpenTofu (K8s infrastructure)
│   └── k8s/                # Helm charts + Kustomize
├── nix/                    # Nix modules (NixOS + Home Manager)
├── config/                 # Non-secret configs + examples
├── certs/                  # TLS certificates
├── scripts/                # Dev + ops scripts
└── docs/
    ├── ARCHITECTURE.md     # System design
    ├── PROTOCOL.md         # .tc/.tcf stub file format spec
    └── archive/            # Previous design documents
```

## Development

```bash
task build          # Build all Rust crates
task test           # Run all tests
task lint           # Clippy + rustfmt check
task deny           # License + advisory check
task check          # All of the above
```

## Infrastructure

```bash
task infra:plan ENV=civo     # Preview K8s changes
task infra:apply ENV=civo    # Apply to Civo cluster
task infra:plan ENV=local    # Preview local k3s changes
```

## Peer Projects

- **remote-juggler** (`@tummycrypt/remote-juggler`): KDBX + git identity + MCP tools.
  tcfs integrates with remote-juggler for credential management.

## License

MIT OR Apache-2.0
