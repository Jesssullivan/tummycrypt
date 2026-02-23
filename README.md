# tummycrypt / tcfs

**tcfs** (TummyCrypt FileSystem) is a FOSS, self-hosted replacement for proprietary cloud storage
clients (odrive, Dropbox desktop, etc.). It mounts remote SeaweedFS storage as a local FUSE
directory, with files appearing as zero-byte `.tc` stubs until accessed — at which point they
are transparently hydrated on demand.

**Status**: Active development. Core sync, FUSE mount, CLI, TUI, MCP server, K8s worker mode, E2E encryption, and multi-machine fleet sync with vector clocks are functional. See [CHANGELOG](CHANGELOG.md) for release history.

## What it does

- Mounts remote S3/SeaweedFS storage as a local directory
- Files appear as `.tc` stubs (zero bytes) until you open them
- Opening a `.tc` stub triggers on-demand download, replacing stub with real file
- `tcfs unsync <path>` converts a hydrated file back to a stub (reclaims disk)
- Sync is bidirectional, conflict-aware, git-friendly (BLAKE3 hashed, FastCDC chunked)
- E2E encryption: XChaCha20-Poly1305 with Argon2id key derivation and BIP-39 recovery
- Multi-machine fleet sync with vector clocks, NATS JetStream, and pluggable conflict resolution
- Billions of small files, horizontal K8s backend, KEDA auto-scaling

## Fleet Sync

tcfs supports multi-machine sync across a device fleet:

- **Device identity**: Each machine enrolls with a UUID and age keypair, stored in an S3-backed registry
- **Vector clocks**: Distributed partial ordering detects concurrent edits without a central coordinator
- **NATS JetStream**: Real-time state events (`FileSynced`, `DeviceOnline`, `ConflictResolved`, etc.) with per-device durable consumers
- **Conflict resolution**: Pluggable modes — `auto` (lexicographic tie-break), `interactive` (CLI/TUI prompt), or `defer` (log and skip)
- **Git-safe sync**: Optional `.git/` directory sync via atomic git bundles with lock detection

```bash
# Enroll this machine
tcfs device enroll --name $(hostname)

# Push a file (vector clock ticks, manifest v2 written)
tcfs push ~/documents/report.pdf

# On another machine: pull with conflict detection
tcfs pull tcfs/default/report.pdf ~/documents/report.pdf
```

## Architecture

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for full system diagram and component overview.

## Quick Start (local dev)

```bash
# Enter devShell (requires Nix)
nix develop

# Or install tools manually (see docs/CONTRIBUTING.md)

# Generate TLS certs + start SeaweedFS + NATS + Prometheus + Grafana
task dev

# Verify SeaweedFS is running
curl http://localhost:9333/cluster/status

# Verify NATS is running
nats server ping nats://localhost:4222
```

## Installation

```bash
# Linux (x86_64) — installer script
curl -fsSL https://github.com/tinyland-inc/tummycrypt/releases/latest/download/install.sh | sh

# macOS (Homebrew)
brew install tinyland-inc/tap/tcfs

# Debian/Ubuntu
sudo dpkg -i tcfs-*.deb

# RPM (Fedora/RHEL/Rocky)
sudo rpm -i tcfsd-*.rpm

# Container (K8s worker mode)
podman pull ghcr.io/tinyland-inc/tcfsd:latest

# Nix
nix build github:tinyland-inc/tummycrypt
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
├── CHANGELOG.md            # Release history
├── flake.nix               # Nix devShell + packages
├── Taskfile.yaml           # Build tasks (task --list)
├── docker-compose.yml      # Local dev stack
├── .sops.yaml              # SOPS encryption rules
├── crates/                 # Rust workspace members (14 crates)
│   ├── tcfs-core/          # Shared types, config, protobuf definitions
│   ├── tcfs-crypto/        # XChaCha20-Poly1305 encryption, key derivation
│   ├── tcfs-secrets/       # SOPS/age/KDBX + device identity/registry
│   ├── tcfs-storage/       # OpenDAL + SeaweedFS operator
│   ├── tcfs-chunks/        # FastCDC chunking, BLAKE3, zstd compression
│   ├── tcfs-sync/          # Sync engine, vector clocks, NATS JetStream
│   ├── tcfs-fuse/          # FUSE driver (Linux)
│   ├── tcfs-cloudfilter/   # Windows CFAPI (skeleton)
│   ├── tcfs-sops/          # SOPS+age fleet secret propagation
│   ├── tcfs-file-provider/ # macOS/iOS FileProvider FFI (RFC 0002)
│   ├── tcfsd/              # Daemon binary (gRPC + metrics + systemd)
│   ├── tcfs-cli/           # CLI binary (tcfs)
│   ├── tcfs-tui/           # TUI binary (ratatui dashboard)
│   └── tcfs-mcp/           # MCP server binary (AI agent integration)
├── credentials/            # SOPS-encrypted credentials
├── infra/
│   ├── ansible/            # SeaweedFS Ansible deployment
│   ├── tofu/               # OpenTofu (K8s infrastructure)
│   └── k8s/                # Helm charts + Kustomize
├── nix/                    # Nix modules (NixOS + Home Manager)
├── examples/lab-fleet/     # Per-machine fleet config fragments
├── config/                 # Non-secret configs + examples
├── scripts/                # Dev + ops scripts
└── docs/
    ├── ARCHITECTURE.md     # System design (LaTeX → PDF)
    ├── PROTOCOL.md         # Wire format, gRPC RPCs (LaTeX → PDF)
    ├── SECURITY.md         # Threat model, encryption (LaTeX → PDF)
    ├── CONTRIBUTING.md     # Development setup, PR workflow
    ├── BENCHMARKS.md       # Performance characteristics
    ├── rfc/                # Design RFCs
    └── archive/            # Previous design documents
```

## CLI Commands

| Command | Description |
|---------|-------------|
| `tcfs status` | Show daemon status, device identity, NATS connection |
| `tcfs config show` | Display active configuration |
| `tcfs push <path>` | Upload files with chunking, encryption, vector clock tick |
| `tcfs pull <remote> <local>` | Download files with conflict detection |
| `tcfs sync-status <path>` | Check sync state of a file |
| `tcfs mount <source> <target>` | FUSE mount with on-demand hydration |
| `tcfs unmount <path>` | Unmount FUSE directory |
| `tcfs unsync <path>` | Convert hydrated file back to `.tc` stub |
| `tcfs device enroll` | Generate keypair and register in S3 |
| `tcfs device list` | Show all enrolled devices |
| `tcfs device revoke <name>` | Mark a device as revoked |
| `tcfs device status` | Show this device's identity |

## Binaries

| Binary | Purpose |
|--------|---------|
| `tcfs` | CLI: push, pull, sync-status, mount, unmount, unsync, device management |
| `tcfsd` | Daemon: 11 gRPC RPCs, FUSE mounts, NATS state sync, Prometheus metrics, systemd notify |
| `tcfs-tui` | Terminal UI: 5-tab dashboard (Dashboard, Config, Mounts, Secrets, Conflicts) |
| `tcfs-mcp` | MCP server: 8 tools for AI agent integration (stdio transport) |

## Development

```bash
task build          # Build all Rust crates
task test           # Run all tests (150 tests + 18 proptest properties)
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

- **[remote-juggler](https://github.com/tinyland-inc/remote-juggler)**: Git identity management + KDBX credential resolution.
  tcfs integrates with remote-juggler for credential fallback via KeePassXC.

## License

MIT OR Apache-2.0
