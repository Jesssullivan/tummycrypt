# Contributing to tcfs

## Development Setup

### Prerequisites

- Rust 1.93.0 (pinned via `rust-toolchain.toml`, rustup, or Nix)
- protobuf-compiler (`protoc`)
- pkg-config, libssl-dev, libfuse3-dev (Linux)
- [Task](https://taskfile.dev) (task runner)
- [SOPS](https://github.com/getsops/sops) + [age](https://age-encryption.org) (for credential management)

### Quick Start with Nix (recommended)

```bash
# Clone and enter devShell
git clone https://github.com/Jesssullivan/tummycrypt.git
cd tummycrypt
nix develop
# Or auto-load the committed .envrc once:
direnv allow

# Build everything
task build

# Run tests
task test
```

> **System Rust note**: outside the Nix devShell, ensure rustup's cargo bin dir is on `PATH` (for example `source "$HOME/.cargo/env"`). The committed `.envrc` does this automatically when direnv is enabled.
> The repo devShell and `.envrc` also put `target/debug` and `target/release`
> ahead of user-level installs so local smoke commands can use workspace-built
> `tcfs` / `tcfsd` binaries after they have been built. Smoke harnesses print
> the resolved binary paths before checking versions.
> The devShell also pins the shell helper surface used by lazy/Finder proof
> tasks, including `shellcheck`, `jq`, `aws`, `s5cmd`, and `mc`.

### Quick Start without Nix

```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup toolchain install 1.93.0
rustup override set 1.93.0

# Install system dependencies (Debian/Ubuntu)
sudo apt install protobuf-compiler pkg-config libssl-dev libfuse3-dev

# Install system dependencies (Fedora/RHEL/Rocky)
sudo dnf install protobuf-compiler pkg-config openssl-devel fuse3-devel

# Copy and fill environment template
cp .env.example .env
# Edit .env with your SeaweedFS endpoint and credentials

# Build and test
cargo build --workspace
cargo test --workspace
```

### Environment Variables

Copy `.env.example` to `.env` and fill in values. Required for integration tests:

See `.env.example` for the full list of environment variables needed for
integration tests (S3 access key, secret key, endpoint, bucket name).

## Project Structure

The workspace is split into 18 product crates under `crates/` plus the
`tests/e2e` workspace member:

| Crate | Type | Description |
|-------|------|-------------|
| `tcfs-core` | lib | Shared types, config parsing, protobuf definitions |
| `tcfs-crypto` | lib | XChaCha20-Poly1305 encryption, key derivation, BIP-39 |
| `tcfs-auth` | lib | TOTP, WebAuthn/FIDO2, session, and enrollment helpers |
| `tcfs-secrets` | lib | SOPS decryption, age identity, KeePassXC integration |
| `tcfs-storage` | lib | OpenDAL-based S3/SeaweedFS operator |
| `tcfs-chunks` | lib | FastCDC chunking, BLAKE3 hashing, zstd compression |
| `tcfs-sync` | lib | Sync engine, state cache, NATS JetStream |
| `tcfs-vfs` | lib | Shared virtual filesystem, disk cache, stubs, hydration |
| `tcfs-fuse` | lib | Linux FUSE3 mount driver |
| `tcfs-nfs` | lib | NFS loopback mount backend |
| `tcfs-cloudfilter` | lib | Windows Cloud Files API (skeleton) |
| `tcfs-sops` | lib | SOPS+age fleet secret propagation |
| `tcfs-file-provider` | lib | macOS/iOS FileProvider FFI (RFC 0002) |
| `tcfs-dbus` | lib | Linux D-Bus desktop status integration |
| `tcfsd` | bin | Daemon: gRPC, FUSE/NFS, metrics, systemd notify |
| `tcfs-cli` | bin | CLI: push, pull, mount, unmount, status, device management |
| `tcfs-tui` | bin | Interactive terminal UI (ratatui) |
| `tcfs-mcp` | bin | MCP server for AI agent integration |

## Development Workflow

### Building

```bash
task build              # Build all workspace crates
task build:release      # Release build with optimizations
cargo build -p tcfsd    # Build a single crate
```

### Testing

```bash
task test               # Run all tests
cargo test -p tcfs-chunks   # Test a single crate
cargo test -- --nocapture   # Show stdout/stderr
```

### Linting

```bash
task lint               # cargo clippy + rustfmt check
cargo fmt --all         # Auto-format
cargo clippy --workspace --all-targets --fix  # Auto-fix lints
```

### Running Locally

```bash
# Start the dev stack (SeaweedFS + NATS + Prometheus + Grafana)
# This runs docker-compose with the local dev infrastructure
task dev

# In another terminal, run the daemon
cargo run -p tcfsd

# Use the CLI
cargo run -p tcfs-cli -- status
cargo run -p tcfs-cli -- push /path/to/files
cargo run -p tcfs-cli -- mount seaweedfs://localhost:8333/tcfs /tmp/tcfs-mount
```

## Pull Request Guidelines

1. **Branch from** `main` (use `sid/` prefix for feature branches)
2. **Treat `Jesssullivan/tummycrypt` as canonical** for PRs, issues, and release flow — see [Remote Governance](ops/remote-governance.md) for the operational policy
3. **Use org forks as downstreams only** unless a specific distribution task requires them
4. **Run checks locally** before pushing: `task check` (fmt + clippy + test + build)
5. **Keep PRs focused** - one feature or fix per PR
6. **Add tests** for new functionality
7. **Update docs** if you change user-facing behavior
8. CI runs: `cargo fmt --check`, `cargo clippy`, `cargo test`, `cargo-deny`, security audit

## Code Style

- Follow existing patterns in the codebase
- Use `thiserror` for library error types, `anyhow` for binary error handling
- Async runtime: tokio (full features)
- Prefer `tracing` over `log` for structured logging
- Run `cargo fmt` before committing

## License

By contributing, you agree that your contributions will be dual-licensed under MIT and Apache 2.0.
