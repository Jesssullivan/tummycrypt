# tcfs — TummyCrypt Filesystem

**FOSS self-hosted odrive replacement**

tcfs is a FUSE-based file sync daemon backed by [SeaweedFS](https://github.com/seaweedfs/seaweedfs) with end-to-end [age](https://age-encryption.org) encryption, content-defined chunking, and on-demand hydration via `.tc` stub files.

## Installation

> **Note:** tcfs is in active development. Binary releases will be published once the first stable release is tagged.

### From Source (recommended for now)

```bash
# Requires Rust 1.93+, protoc, libfuse3-dev
git clone https://github.com/tinyland-inc/tummycrypt.git
cd tummycrypt
cargo build --release
# Binaries: target/release/tcfs, target/release/tcfsd, target/release/tcfs-tui
```

### Nix

```bash
nix build github:tinyland-inc/tummycrypt
# Or enter a devShell:
nix develop github:tinyland-inc/tummycrypt
```

### Container (K8s worker mode)

```bash
podman pull ghcr.io/tinyland-inc/tcfsd:latest
```

### Future Channels (once releases are published)

- **curl installer**: `curl -fsSL .../install.sh | sh`
- **Homebrew**: `brew install tinyland-inc/tap/tcfs`
- **Debian/Ubuntu**: `sudo dpkg -i tcfs-*.deb`
- **RPM**: `sudo rpm -i tcfsd-*.rpm`

## How It Works

1. **Push**: Files are split into content-defined chunks (FastCDC), compressed (zstd), encrypted (age), and uploaded to SeaweedFS via S3.
2. **Pull**: Manifests describe the chunk layout. Chunks are fetched, verified (BLAKE3), decrypted, decompressed, and reassembled.
3. **Mount**: FUSE driver presents remote files as local. Files appear as `.tc` stubs until opened — then they're hydrated on demand.
4. **Unsync**: Convert hydrated files back to stubs, reclaiming disk space while keeping the remote copy.

## Architecture

```
tcfs (CLI)  ───┐
tcfs-tui    ───┤  gRPC / Unix socket
               ├──── tcfsd (daemon) ──── FUSE mount
               │        │
               │        ├── tcfs-secrets (age/SOPS/KeePassXC)
               │        ├── tcfs-chunks  (FastCDC + zstd + BLAKE3)
               │        ├── tcfs-storage (OpenDAL → S3/SeaweedFS)
               │        └── tcfs-sync    (state cache + NATS JetStream)
               │
K8s workers ───┘  (tcfsd --mode=worker, scaled by KEDA)
```

## Binaries

| Binary | Purpose |
|--------|---------|
| `tcfs` | CLI: push, pull, sync-status, mount, unmount, unsync |
| `tcfsd` | Daemon: gRPC socket, FUSE mounts, Prometheus metrics, systemd notify |
| `tcfs-tui` | Terminal UI for interactive file management |

## Documentation

- [Architecture](ARCHITECTURE.md) — detailed system design
- [Protocol](PROTOCOL.md) — wire format, chunk layout, manifest schema
- [Security](SECURITY.md) — threat model, encryption details
- [Contributing](CONTRIBUTING.md) — development setup, PR workflow
- [Benchmarks](BENCHMARKS.md) — performance characteristics

## Platform Support

| Platform | Status | Notes |
|----------|--------|-------|
| Linux x86_64 | Full | FUSE mount, CLI, daemon, TUI |
| Linux aarch64 | Full | FUSE mount, CLI, daemon, TUI |
| macOS (Apple Silicon) | CLI only | FUSE via FUSE-T planned |
| macOS (Intel) | CLI only | FUSE via FUSE-T planned |
| Windows | Planned | Cloud Files API (CFAPI) for native Explorer integration |
| NixOS | Full | Flake + NixOS module + Home Manager module |

## License

Dual-licensed under MIT and Apache 2.0.
