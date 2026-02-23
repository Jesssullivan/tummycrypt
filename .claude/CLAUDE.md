# tcfs Development Context

## Quick Start

```bash
# Enter Nix devShell (recommended)
nix develop
# Or with direnv:
echo 'use flake' > .envrc && direnv allow

# Build
~/.cargo/bin/cargo build --workspace

# Test
~/.cargo/bin/cargo test --workspace

# Lint
~/.cargo/bin/cargo fmt --all -- --check
~/.cargo/bin/cargo clippy --workspace --all-targets

# Start dev infrastructure (SeaweedFS + NATS + Prometheus + Grafana)
task dev
```

## Environment Notes

- **Shell**: fish (does NOT support `export VAR=VALUE`; use `env VAR=VALUE command`)
- **Cargo**: Not in PATH on Rocky Linux — always use `~/.cargo/bin/cargo`
- **Linker**: mold is NOT installed outside Nix; do not add to `.cargo/config.toml`
- **Docker**: Do not run docker-compose on yoga (resource-constrained)
- **Rust edition**: 2021 (Rust >= 1.93 required for workspace)

## Workspace Crates

| Crate | Type | Description |
|-------|------|-------------|
| `tcfs-core` | lib | Shared types, config, protobuf (gRPC service definition) |
| `tcfs-crypto` | lib | XChaCha20-Poly1305 encryption, Argon2id KDF, BIP-39 |
| `tcfs-secrets` | lib | SOPS/age decryption, KeePassXC, device identity/registry |
| `tcfs-storage` | lib | OpenDAL S3/SeaweedFS operator + health checks |
| `tcfs-chunks` | lib | FastCDC chunking, BLAKE3 hashing, zstd compression |
| `tcfs-sync` | lib | Sync engine, vector clocks, state cache, NATS JetStream |
| `tcfs-fuse` | lib | Linux FUSE driver (fuse3) |
| `tcfs-cloudfilter` | lib | Windows Cloud Files API (skeleton) |
| `tcfs-sops` | lib | SOPS+age fleet secret propagation |
| `tcfsd` | bin | Daemon: gRPC over Unix socket, FUSE, metrics, systemd |
| `tcfs-cli` | bin | CLI: push, pull, mount, device, status |
| `tcfs-tui` | bin | Terminal UI: ratatui 5-tab dashboard |
| `tcfs-mcp` | bin | MCP server: 8 tools, rmcp 0.16, stdio transport |

## Key Patterns

- **Proto source of truth**: `crates/tcfs-core/src/proto/tcfs.proto` — all crates import via `tcfs_core::proto`
- **Error handling**: `thiserror` for libraries, `anyhow` for binaries
- **Async**: tokio full features, `tracing` for structured logging
- **State cache**: JSON-backed at `{config.sync.state_db}.json`
- **CAS layout**: chunks at `{prefix}/chunks/{hash}`, manifests at `{prefix}/manifests/{file_hash}`
- **Feature gates**: `fuse` feature on tcfs-cli (default on), `nats` feature on tcfs-sync

## Testing

```bash
# All tests
~/.cargo/bin/cargo test --workspace

# Specific crate
~/.cargo/bin/cargo test -p tcfs-sync

# Property-based tests
~/.cargo/bin/cargo test -p tcfs-sync -- conflict
~/.cargo/bin/cargo test -p tcfs-sync --test multi_machine_sim

# With output
~/.cargo/bin/cargo test -- --nocapture
```

## CI

- GitHub Actions: fmt, clippy, test, build, cargo-deny, security audit, nix build
- Docs CI: lychee link check + tectonic PDF build + Jekyll GitHub Pages
- Release: 9 build targets (5 platforms + container + nix + installers + plan)
