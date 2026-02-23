# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.4.0] - 2026-02-23

### Added

- **Benchmarks**: divan benchmark framework for chunking and encryption throughput (#22)
  - FastCDC chunking, BLAKE3 hashing, zstd compress/decompress, XChaCha20-Poly1305 encrypt/decrypt
  - `task bench` command for running all benchmarks
  - `docs/BENCHMARKS.md` populated with real measurements (BLAKE3: 1.39 GB/s, zstd: 1.26 GB/s)
- **Chunk integrity verification**: BLAKE3 hash verified per-chunk on download and against manifest file hash (#23)
- **Graceful shutdown**: SIGTERM/SIGINT handler flushes state cache, publishes DeviceOffline, sends systemd STOPPING=1 (#23)
- **Health endpoints**: `/healthz` (liveness) and `/readyz` (readiness with S3 check) on metrics HTTP server (#23)
- **7 integration tests**: push/pull round-trip, dedup, integrity, tree push, device-aware sync using in-memory backend (#23)
- **Fleet deployment guide**: `docs/ops/fleet-deployment.md` covering NATS access, credential distribution, daemon startup (#22)
- **macOS launchd plist**: `dist/com.tummycrypt.tcfsd.plist` for automatic daemon startup (#22)
- RFC 0002: Darwin File Integration Strategy — FileProvider as primary macOS/iOS path (#21)
- RFC 0003: iOS File Provider with UniFFI bridge design (#22)
- `tcfs-file-provider` crate skeleton for macOS/iOS FileProvider extension (#22)
- `docs/tex/fileprovider.tex` LaTeX design document (#21)

### Changed

- Storage retry improved: 5 retries with jitter (was 3 without jitter) + OpenDAL logging layer (#23)
- gRPC `serve()` now supports graceful shutdown via async signal (#23)
- Metrics server operator handle shared with health endpoint for live readiness checks (#23)

### Fixed

- Resolved RFC 0001 open questions (NATS access path, credential distribution, daemon startup) (#22)

## [0.3.0] - 2026-02-22

### Added

- Multi-machine fleet sync with vector clocks and conflict resolution (#18, #19)
- `VectorClock` implementation with `tick()`, `merge()`, `partial_cmp()`, `is_concurrent()`
- `SyncManifest` v2 (JSON format with vector clocks, backward-compatible v1 text fallback)
- Device identity system with auto-enrollment and S3-backed `DeviceRegistry`
- CLI `device` subcommand: `enroll`, `list`, `revoke`, `status`
- NATS JetStream real-time state sync (`StateEvent` enum with 6 event types)
- Per-device durable NATS consumers with hierarchical subjects (`STATE.{device_id}.{type}`)
- `ResolveConflict` gRPC RPC (11 total RPCs)
- `.git` directory sync safety: lock detection, git bundle mode, cooperative locking
- Config-driven file collection (`sync_git_dirs`, `exclude_patterns`, `sync_hidden_dirs`)
- Interactive conflict resolver in CLI (`keep_local`, `keep_remote`, `keep_both`, `defer`)
- TUI Conflicts tab for pending conflict review
- MCP `resolve_conflict` and `device_status` tools (8 total tools)
- NixOS and Home Manager module options for fleet sync
- `examples/lab-fleet/` with per-machine config fragments
- 18 proptest properties (8 vector clock, 2 crypto round-trip, 5 simulation, 3 integration)
- RFC 0001: Fleet sync integration plan
- LaTeX design documents (Architecture, Protocol, Security) with CI-built PDFs
- Mermaid architecture diagrams in docs site
- Link checking with lychee

### Changed

- CLI `push`/`pull` now use device-aware upload/download with vector clock tracking
- Daemon publishes `DeviceOnline` event on NATS connect and `FileSynced` on push
- `tcfs-sync` NATS feature is now always enabled in `tcfsd` (fleet sync is core)
- Status RPC returns `device_id`, `device_name`, and `conflict_mode`
- Manifest format upgraded from newline-delimited text to JSON (v2)

## [0.2.5] - 2026-02-21

### Fixed

- Bind metrics server to `0.0.0.0` in K8s configmap for health probes (#14)
- Add `imagePullSecrets` for private GHCR container registry (#13)
- Disable KEDA `ScaledObject` and `ServiceMonitor` CRDs in Civo deploy (#12)
- Update container image repo and S3 endpoint for in-cluster SeaweedFS (#11)

### Added

- MCP server (`tcfs-mcp`) for AI agent integration with 6 tools (#10)
- Civo K8s deployment with NATS + SeaweedFS in `tcfs` namespace

## [0.2.1] - 2026-02-21

### Added

- gRPC RPCs: `push` (client-streaming), `pull` (server-streaming), `sync_status` (#9)
- TUI dashboard with 4 tabs: Dashboard, Config, Mounts, Secrets (#7)
- `tcfs-sops` crate for SOPS+age fleet secret propagation (#7)

### Fixed

- macOS `fuse3` `FileAttr` missing `crtime`/`flags` fields
- Darwin `apple_sdk` migration for nixpkgs-unstable 2026
- Homebrew formula retry logic + container build amd64-only (#5)

### Security

- Removed committed TLS certificates and private keys from tracking (#8)

## [0.2.0] - 2026-02-21

### Changed

- Version bump for release pipeline (no functional changes beyond v0.2.1 pre-releases)

## [0.1.0] - 2026-02-21

### Added

- Rust monorepo with 13 workspace crates
- Core daemon (`tcfsd`) with gRPC over Unix domain socket
- CLI (`tcfs`): `status`, `config show`, `push`, `pull`, `sync-status`, `mount`, `unmount`, `unsync`
- FUSE driver for Linux with on-demand hydration via `.tc` stubs
- Windows Cloud Files API skeleton (`tcfs-cloudfilter`)
- E2E encryption: XChaCha20-Poly1305, Argon2id key derivation, BIP-39 recovery
- Content-defined chunking (FastCDC) with BLAKE3 hashing and zstd compression
- Secrets management: SOPS/age decryption, KeePassXC integration
- OpenDAL-based S3/SeaweedFS storage backend
- Sync engine with JSON state cache and NATS JetStream messaging
- K8s worker mode with KEDA auto-scaling
- Prometheus metrics endpoint with systemd `sd_notify(READY=1)`
- Cross-platform release pipeline: Linux x86_64/aarch64, macOS x86_64/aarch64, Windows x86_64
- Container image: `ghcr.io/tinyland-inc/tcfsd` (multi-arch distroless)
- Nix flake with NixOS module and Home Manager module
- Homebrew formula, `.deb`/`.rpm` packages, install scripts
- 77 tests, cargo-deny license/advisory checks, security audit CI

[0.4.0]: https://github.com/tinyland-inc/tummycrypt/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/tinyland-inc/tummycrypt/compare/v0.2.5...v0.3.0
[0.2.5]: https://github.com/tinyland-inc/tummycrypt/compare/v0.2.1...v0.2.5
[0.2.1]: https://github.com/tinyland-inc/tummycrypt/compare/v0.1.0...v0.2.1
[0.2.0]: https://github.com/tinyland-inc/tummycrypt/releases/tag/v0.2.0
[0.1.0]: https://github.com/tinyland-inc/tummycrypt/releases/tag/v0.1.0
