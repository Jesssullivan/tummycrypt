# tummycrypt / tcfs

> Under active development. Not yet stable. Expect breaking changes.

Self-hosted encrypted file sync with on-demand hydration. Mounts SeaweedFS as a local directory — files appear as zero-byte `.tc` stubs until accessed, then transparently download and decrypt. FOSS odrive/Dropbox replacement.

## Canonical Home

`Jesssullivan/tummycrypt` is the canonical source repository for tcfs.
If `tinyland-inc/tummycrypt` exists, treat it as a fork or downstream
distribution surface rather than the source of truth for planning, issues,
releases, or contributor workflow.

## Features

- **On-demand hydration**: Files appear as `.tc` stubs, hydrate transparently on open
- **E2E encryption**: XChaCha20-Poly1305 per-chunk, Argon2id KDF, BIP-39 recovery keys
- **Fleet sync**: Multi-machine sync via NATS JetStream with vector clock conflict detection
- **Content-addressed storage**: FastCDC chunking, BLAKE3 hashing, zstd compression
- **Git-safe**: Syncs `.git/` directories as atomic bundles with lock detection
- **Cross-platform**: Linux (FUSE/NFS), macOS (FileProvider/NFS), Windows (Cloud Files API, planned)

## Quick Start

```bash
# Nix devShell (recommended)
nix develop
# Or auto-load the committed devShell + env on cd
direnv allow

# Or manual: install the pinned Rust 1.93.0 toolchain, protobuf compiler, fuse3 (Linux)

# Start local dev infrastructure (SeaweedFS + NATS + Prometheus + Grafana)
task dev

# Build + test
task check
```

## Installation

```bash
# Linux (installer script)
curl -fsSL https://github.com/Jesssullivan/tummycrypt/releases/latest/download/install.sh | sh

# macOS (Homebrew tap from canonical repo)
brew tap Jesssullivan/tummycrypt https://github.com/Jesssullivan/tummycrypt --branch homebrew-tap
brew install tcfs

# Debian/Ubuntu
sudo dpkg -i tcfs-*.deb

# Container (K8s worker mode)
podman pull ghcr.io/jesssullivan/tcfsd:latest

# Nix
nix build github:Jesssullivan/tummycrypt
```

## CLI

```bash
tcfs status                    # Daemon status, device identity, NATS connection
tcfs push <path>               # Upload with encryption + vector clock tick
tcfs pull <manifest> <local>   # Download with conflict detection + decryption
tcfs mount <remote> <target>   # FUSE mount with on-demand hydration
tcfs unsync <path>             # Convert hydrated file back to .tc stub
tcfs device enroll             # Register device with age keypair
tcfs device list               # Show enrolled fleet devices
```

## Binaries

| Binary | Purpose |
|--------|---------|
| `tcfs` | CLI: push, pull, mount, unsync, device management |
| `tcfsd` | Daemon: gRPC, FUSE mounts, NATS fleet sync, Prometheus metrics |
| `tcfs-tui` | Terminal UI: dashboard with sync status, conflicts, mounts |
| `tcfs-mcp` | MCP server: AI agent integration (8 tools, stdio transport) |

## Architecture

18 workspace crates organized in layers:

```
crates/
├── tcfs-core/           # Shared types, config, protobuf (gRPC service)
├── tcfs-crypto/         # XChaCha20-Poly1305, Argon2id, HKDF, BIP-39
├── tcfs-secrets/        # SOPS/age decryption, KeePassXC, device identity
├── tcfs-storage/        # OpenDAL S3/SeaweedFS operator + health checks
├── tcfs-chunks/         # FastCDC chunking, BLAKE3 hashing, zstd compression
├── tcfs-sync/           # Sync engine, vector clocks, NATS JetStream, reconciliation
├── tcfs-auth/           # TOTP, WebAuthn/FIDO2, device enrollment
├── tcfs-vfs/            # Virtual filesystem: hydration, disk cache, negative cache
├── tcfs-fuse/           # Linux FUSE3 driver
├── tcfs-nfs/            # NFS loopback mount (no kernel modules)
├── tcfs-cloudfilter/    # Windows Cloud Files API (planned)
├── tcfs-file-provider/  # macOS/iOS FileProvider FFI (cbindgen + UniFFI)
├── tcfs-sops/           # SOPS+age fleet secret propagation
├── tcfs-dbus/           # D-Bus integration (Linux)
├── tcfsd/               # Daemon binary (gRPC + metrics + systemd)
├── tcfs-cli/            # CLI binary
├── tcfs-tui/            # Terminal UI (ratatui)
└── tcfs-mcp/            # MCP server (rmcp, stdio transport)
```

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the full system design.

## Platform Support

| Feature | Linux | macOS | Windows | iOS |
|---------|-------|-------|---------|-----|
| CLI (push/pull/reconcile) | Full | Full | Planned | - |
| Daemon (gRPC + metrics) | systemd | launchd | Planned | - |
| Filesystem mount | FUSE3 | NFS loopback | Cloud Files API (skeleton) | - |
| FileProvider | - | Full (Finder integration) | - | Read-only |
| Finder/Explorer badges | - | 6 states | - | - |
| D-Bus integration | Full | - | - | - |
| Fleet sync (NATS) | Full | Full | Planned | - |
| E2E encryption | Full | Full | Planned | Full |

See [docs/platform-support.md](docs/platform-support.md) for details.

## Development

```bash
task build          # Build all crates
task test           # Run all tests (424 tests)
task lint           # Clippy + rustfmt
task deny           # License + advisory check
task check          # All of the above
```

See [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md) for setup details and PR workflow.

## Credential Setup

```bash
task sops:init       # Generate age key + configure .sops.yaml
task sops:migrate    # Migrate credentials to SOPS-encrypted files
```

## License

MIT OR Apache-2.0
