# tcfs Development Context

## Program context — where truth lives

This file covers build/test/navigation only. For what is true and what ships
next, read these in order (each owns a distinct layer; none is duplicated here):

1. [docs/VISION.md](docs/VISION.md) — north star + the claim-tier legend.
2. [docs/PRODUCT.md](docs/PRODUCT.md) — the accepted A→B→C delivery sequence,
   stable-root design, repository ownership.
3. [docs/ops/current.md](docs/ops/current.md) — the authoritative live
   blocker/proof boundary (see its own precedence preamble).
4. [docs/platform-support.md](docs/platform-support.md) — per-client maturity.
5. [docs/release/evidence/README.md](docs/release/evidence/README.md) —
   evidence corpus index.

Tracked work: Linear initiative "Tummycrypt — Daily Driver Track"
(https://linear.app/tinyland/initiative/tummycrypt-daily-driver-track-95eeeb5e7493),
umbrella "Cordillera - Tinyland Remote-Everything Program"
(https://linear.app/tinyland/initiative/cordillera-tinyland-remote-everything-program-15f56b187c19).
Sibling repos: tinyland-inc/rockies (OS adoption seed, TIN-2300),
tinyland-inc/lab (fleet deploy/pins), Jesssullivan/prompts-enqueue
(program ledger, prompts 47/60).

## Quick Start

```bash
# Enter Nix devShell (recommended)
nix develop
# Or with direnv:
direnv allow

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

## Workspace Crates (19 members)

| Crate | Type | Description |
|-------|------|-------------|
| `tcfs-core` | lib | Shared types, config, errors, protobuf (gRPC service definition) |
| `tcfs-auth` | lib | Authentication and authorization providers |
| `tcfs-vfs` | lib | Virtual filesystem trait, disk cache, stub formats, hydration |
| `tcfs-crypto` | lib | XChaCha20-Poly1305 encryption, Argon2id KDF, BIP-39 |
| `tcfs-secrets` | lib | SOPS/age decryption, KeePassXC, device identity/registry |
| `tcfs-sops` | lib | SOPS+age fleet secret propagation |
| `tcfs-storage` | lib | OpenDAL S3/SeaweedFS operator + health checks |
| `tcfs-chunks` | lib | FastCDC chunking, BLAKE3 hashing, zstd compression |
| `tcfs-sync` | lib | Sync engine, vector clocks, state cache, NATS JetStream |
| `tcfs-fuse` | lib | Linux FUSE driver (fuse3) |
| `tcfs-nfs` | lib | NFS loopback server (NFSv3, FUSE-free mount) |
| `tcfs-cloudfilter` | lib | Windows Cloud Files API (CFAPI) provider |
| `tcfs-file-provider` | lib | C FFI bridge for macOS/iOS FileProvider (cbindgen/uniffi) |
| `tcfs-dbus` | lib | D-Bus interface for Linux file sync status |
| `tcfsd` | lib+bin | Daemon: gRPC over Unix socket, FUSE, metrics, systemd. Lib surface exposed for integration tests (see `tcfsd::daemon::test_support`). |
| `tcfs-cli` | lib+bin | CLI: push, pull, mount, device, status, unsync. Lib surface exposes ordering-sensitive command helpers for integration tests. |
| `tcfs-tui` | bin | Terminal UI: ratatui 5-tab dashboard |
| `tcfs-mcp` | bin | MCP server: 7 non-resolution tools, rmcp 0.16, stdio transport |
| `tests/e2e` | test | End-to-end integration test crate |

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

## Agent Coordination

Ground rules when multiple agents (Claude, Codex, or other) touch this repo
concurrently. Adapted from the GFTB multi-agent orchestration pattern
(`site.scaffold` `docs/patterns/multi-agent-orchestration.md` §3, commit
`36c14ae`) and lab's durable-notes rule (`lab` commit `b48d46f7`, `TIN-2520`).

- **Shared-PR lane claims — the `#534` lesson.** Claude-driven feature
  branches live on the personal remote (`origin` = `Jesssullivan/tummycrypt`);
  `codex/sync-origin-main-*` branches/PRs on the org mirror (`tinyland` =
  `tinyland-inc/tummycrypt`) are codex-owned reconciliation lanes — don't
  hand-edit or merge another agent's lane without diffing against the history
  it reconciles.
- **DO-NOT-TOUCH lists.** When a lane is in flight, its PR body should carry
  a short list of the crates it owns (e.g. "this PR owns `crates/tcfs-sync`
  — coordinate before stacking commits").
- **Single merge authority per PR.** One agent (or the operator) merges.
  Other agents report findings as PR comments — never push fixes onto a
  branch you don't own without an explicit handoff. `#534` was churned by
  two agents stacking unverified hardening commits on an already-reviewed
  clean head; each "fix round" cited a fresh adversarial pass but none
  re-ran a local build/test. Verify before stacking; don't self-certify onto
  someone else's lane.
- **Durable notes over scratchpad.** Findings that matter beyond the current
  session go in dated files under `docs/ops/` (the existing ~30-file
  convention), never only in an ephemeral scratchpad or chat context a
  compaction/rotation sweep can wipe. This repo rule outranks a harness
  default that says to park working files in `/tmp` or a scratchpad dir —
  that default covers genuinely transient scratch only (lab's `TIN-2520`
  clause is the canonical statement of this precedence).
- **Instruction precedence.** Repo-root `AGENTS.md` (this file) > the
  nearest in-repo `.claude/CLAUDE.md` overlay > named `docs/ops/` facet docs
  referenced from here or a task > machine-level / home-manager defaults.
