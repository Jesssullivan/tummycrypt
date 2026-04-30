# tummycrypt / tcfs

> Under active development. Not yet stable. Expect breaking changes.

Self-hosted encrypted file sync with on-demand hydration. Mounted views present remote files as normal names and hydrate on open; offline/dehydrated copies can be represented by small `.tc`/`.tcf` stubs. FOSS odrive/Dropbox replacement.

## Canonical Home

`Jesssullivan/tummycrypt` is the canonical source repository for tcfs.
If `tinyland-inc/tummycrypt` exists, treat it as a fork or downstream
distribution surface rather than the source of truth for planning, issues,
releases, or contributor workflow.

Operational policy: [`docs/ops/remote-governance.md`](docs/ops/remote-governance.md).

## Features

- **On-demand hydration**: Mounted files list as normal names and hydrate transparently on open
- **E2E encryption**: XChaCha20-Poly1305 per-chunk, Argon2id KDF, BIP-39 recovery keys
- **Fleet sync**: Multi-machine sync via NATS JetStream with vector clock conflict detection
- **Content-addressed storage**: FastCDC chunking, BLAKE3 hashing, zstd compression
- **Git-safe**: Syncs `.git/` directories as atomic bundles with lock detection
- **Cross-platform**: Linux is the best-supported runtime; macOS has packaged but still experimental desktop surfaces; Windows remains planned

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
# Linux/macOS tarball convenience installer
# Fast CLI install, but not part of the canonical release-proof surface.
curl -fsSL https://github.com/Jesssullivan/tummycrypt/releases/latest/download/install.sh | sh

# macOS (Homebrew, current manual tap flow)
brew tap --custom-remote Jesssullivan/tummycrypt https://github.com/Jesssullivan/tummycrypt.git
git -C "$(brew --repo Jesssullivan/tummycrypt)" fetch origin homebrew-tap
git -C "$(brew --repo Jesssullivan/tummycrypt)" checkout homebrew-tap
brew install Jesssullivan/tummycrypt/tcfs

# Debian/Ubuntu
sudo dpkg -i tcfsd-*.deb tcfs-*.deb

# RPM (Fedora/RHEL/Rocky, daemon-only today)
sudo rpm -i tcfsd-*.rpm

# Container (K8s worker mode)
podman pull ghcr.io/jesssullivan/tcfsd:latest

# Nix
nix build github:Jesssullivan/tummycrypt
```

For the supported post-release proof contract across Homebrew, `.pkg`, `.deb`,
`.rpm`, container, and Nix, see
[docs/ops/distribution-smoke-matrix.md](docs/ops/distribution-smoke-matrix.md).

## CLI

```bash
tcfs status                    # Daemon status, device identity, NATS connection
tcfs push <path>               # Upload with encryption + vector clock tick
tcfs pull <manifest> <local>   # Download with conflict detection + decryption
tcfs mount <remote> <target>   # FUSE mount with clean-name on-demand hydration
tcfs unsync <path>             # Convert hydrated file back to a physical .tc stub
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

For packaged release proof across Homebrew, `.pkg`, `.deb`, `.rpm`, container,
and Nix surfaces, see [docs/ops/distribution-smoke-matrix.md](docs/ops/distribution-smoke-matrix.md).
For the bar after install succeeds, see
[docs/ops/packaged-install-first-use.md](docs/ops/packaged-install-first-use.md).

## Platform Support

| Feature | Linux | macOS | Windows | iOS |
|---------|-------|-------|---------|-----|
| CLI (push/pull/reconcile) | Full | Full | Planned | - |
| Daemon (gRPC + metrics) | Full | Available, lightly validated | Planned | - |
| Filesystem mount | Full (FUSE3, NFS fallback) | Experimental | Cloud Files API (skeleton) | - |
| FileProvider | - | Experimental | - | Proof-of-concept, read-only |
| Finder/Explorer badges | - | Experimental | - | - |
| D-Bus integration | Full | - | - | - |
| Fleet sync (NATS) | Full | Core path available, not continuously acceptance-tested | Planned | - |
| E2E encryption | Full | Full | Planned | Core crypto path available |

See [docs/platform-support.md](docs/platform-support.md) for details.
For the dated Apple posture, see
[docs/ops/apple-surface-status.md](docs/ops/apple-surface-status.md).

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
