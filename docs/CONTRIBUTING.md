# Contributing to tcfs

## Development Setup

### Prerequisites

- Rust 1.93+ (via rustup or Nix)
- protobuf-compiler (`protoc`)
- pkg-config, libssl-dev, libfuse3-dev (Linux)
- [Task](https://taskfile.dev) (task runner)
- [SOPS](https://github.com/getsops/sops) + [age](https://age-encryption.org) (for credential management)

### Quick Start with Nix (recommended)

```bash
# Clone and enter devShell
git clone https://github.com/tinyland-inc/tummycrypt.git
cd tummycrypt
nix develop    # or: direnv allow

# Build everything
task build

# Run tests
task test
```

### Quick Start without Nix

```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

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

The workspace is split into 10 crates under `crates/`:

| Crate | Type | Description |
|-------|------|-------------|
| `tcfs-core` | lib | Shared types, config parsing, protobuf definitions |
| `tcfs-secrets` | lib | SOPS decryption, age identity, KeePassXC integration |
| `tcfs-storage` | lib | OpenDAL-based S3/SeaweedFS operator |
| `tcfs-chunks` | lib | FastCDC chunking, BLAKE3 hashing, zstd compression |
| `tcfs-sync` | lib | Sync engine, state cache, NATS JetStream |
| `tcfs-fuse` | lib | Linux FUSE driver (fuse3 crate) |
| `tcfs-cloudfilter` | lib | Windows Cloud Files API (skeleton) |
| `tcfsd` | bin | Daemon: gRPC, FUSE, metrics, systemd notify |
| `tcfs-cli` | bin | CLI: push, pull, mount, unmount, status |
| `tcfs-tui` | bin | Interactive terminal UI (ratatui) |

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
task dev

# In another terminal, run the daemon
cargo run -p tcfsd

# Use the CLI
cargo run -p tcfs-cli -- status
cargo run -p tcfs-cli -- push /path/to/files
cargo run -p tcfs-cli -- mount seaweedfs://localhost:8333/tcfs /tmp/tcfs-mount
```

## Pull Request Guidelines

1. **Branch from** `1-build-proxmox-mvp` (current development branch)
2. **Run checks locally** before pushing: `task check` (fmt + clippy + test + build)
3. **Keep PRs focused** - one feature or fix per PR
4. **Add tests** for new functionality
5. **Update docs** if you change user-facing behavior
6. CI runs: `cargo fmt --check`, `cargo clippy`, `cargo test`, `cargo-deny`, security audit

## Code Style

- Follow existing patterns in the codebase
- Use `thiserror` for library error types, `anyhow` for binary error handling
- Async runtime: tokio (full features)
- Prefer `tracing` over `log` for structured logging
- Run `cargo fmt` before committing

## License

By contributing, you agree that your contributions will be dual-licensed under MIT and Apache 2.0.
