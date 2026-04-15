# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.12.0] - 2026-04-08

### Added

- **Finder status surfaces**: macOS FileProvider gained Finder Sync badge support, download progress reporting, and policy-aware excluded or pinned status badges.
- **Conflict UX improvements**: conflict notifications, conflict-copy remote writes, and CLI policy controls for handling sync conflicts.
- **Apple packaging updates**: release artifacts include notarized Apple Silicon `.pkg` installers and bundled `TCFSProvider.app` payloads.

### Changed

- macOS release builds now vendor OpenSSL to avoid code-signing and runtime loader failures during packaging.
- Canonical release assets now ship from `Jesssullivan/tummycrypt` with GitHub Releases, GHCR images, Homebrew tap updates, and Nix cache publication.

## [0.11.1] - 2026-04-08

### Added

- **`tcfs reconcile` CLI**: explicit reconcile command wired to the bidirectional plan-and-execute sync engine introduced in `0.10.0`.
- **Scheduler safety wiring**: `PathLocks` and `FileSyncStatus` now drive daemon scheduling decisions instead of remaining library-only primitives.
- **Watcher blacklist enforcement**: daemon watcher-to-scheduler flow now applies the shared blacklist logic consistently.
- CI step for the sync integration path to exercise the new robustness and reconciliation lanes during automation.

### Changed

- Release pipeline hardening for container build and publish flow, including lowercase GHCR naming fixes.
- Re-enabled Attic cache use in the Nix fleet configuration after the earlier stale-cache disablement.
- FileProvider build script now validates outputs and required configuration before packaging.

### Fixed

- Removed plaintext credential handling from repo and deployment surfaces.
- Documentation refreshed to match the active development state after the `0.10.x` sync-safety push.

## [0.11.0] - 2026-04-07

### Added

- **Read-write FUSE/VFS path**: write, create, unlink, mkdir, rename, and rmdir operations now flow through the SeaweedFS-backed mount path.
- **FUSE write pipeline parity**: FastCDC chunking, transparent `.tc` handling, NATS publish on write, and index-first auto-pull wiring for mounted file edits.
- **Encryption wiring across push paths**: `EncryptionContext` and encrypted manifest support now cover the remaining CLI and daemon upload paths.
- **Daemon self-sufficiency**: daemon startup can provision sockets, local directories, credentials, and unlock flow without external bootstrapping.
- **iOS/FileProvider onboarding work**: QR enrollment improvements, credential-broker support, build-info surfaces, and several FileProvider fixes landed in the Apple lane.

### Changed

- FUSE3 returned as the default mount backend, with the NFS path retained as fallback.
- Stale Attic cache configuration was disabled in Nix surfaces while binary-cache behavior was being corrected.

### Fixed

- Cross-host sync lookup and pull behavior now normalize `rel_path`, repair absolute-path push fallout, and add fallback filename search where older index state exists.
- FUSE hydration now understands JSON v2 manifests and decrypts encrypted chunks with the correct file identity.
- Watcher, daemon, and NATS paths now avoid skipped-upload orphaned index entries and missing prefix propagation.

## [0.10.0] - 2026-04-05

### Added

- **Centralized blacklist** (`tcfs-sync::blacklist`): Consolidates 6 scattered exclusion sites (watcher, engine, VFS, git safety) into a single `Blacklist` type with `check()`, `check_name()`, and `check_path_components()` methods. Configurable glob patterns, hidden dirs, .git policy.
- **Directory reconciliation pipeline** (`tcfs-sync::reconcile`): Plan-then-execute bidirectional sync. `reconcile()` diffs local tree vs remote index producing a `ReconcilePlan` (pure data). `execute_plan()` performs I/O via existing engine primitives. Supports push, pull, delete, conflict detection via vector clocks.
- **Per-folder sync policies** (`tcfs-sync::policy`): `PolicyStore` with `FolderPolicy` (SyncMode, download_threshold, auto_unsync_exempt). Parent-chain walk inheritance — policy on `/project` applies to all descendants.
- **Auto-unsync controller** (`tcfs-sync::auto_unsync`): Background sweep finds files where `last_synced` exceeds configurable max age. Respects PolicyStore exemptions, skips dirty files (unsynced local changes). Removes from state cache only — local files preserved.
- **PathLocks**: Per-path async mutex preventing concurrent push/pull/unsync on the same file. RAII guard with automatic cleanup.
- **FileSyncStatus enum**: 5-state runtime status (NotSynced, Synced, Active, Locked, Conflict) for gRPC/JSON.
- **Unsync dirty-child check**: Before folder unsync, scans `children_with_prefix()` for unsynced modifications. Errors unless `force=true`.
- **Diagnostics RPC**: New gRPC endpoint reporting state cache size, conflict count, NATS seq, auto-unsync eligible count, storage health, uptime.
- **Scheduler observability**: `active()`, `completed()`, `failed()` counters on `SyncScheduler` for operational monitoring.
- `children_with_prefix()` on `StateCacheBackend` for directory-level state queries.
- `list_remote_index()` for fetching remote S3 index entries.
- `parse_index_entry()` for the `manifest_hash=.../size=.../chunks=...` format.
- Config fields: `auto_unsync_max_age_secs`, `auto_unsync_interval_secs`, `auto_unsync_dry_run`.
- CI job for `cargo test -p tcfs-sync --features nats,crypto`.

### Changed

- **BREAKING**: `auth.require_session` now defaults to `true` (was `false`). Add `auth.require_session = false` for dev environments.
- **BREAKING**: `NatsClient::connect()` now accepts `require_tls: bool` parameter. If `nats_tls=true` and URL is `nats://`, auto-upgrades to `tls://`.
- Storage `RetryLayer` now uses exponential backoff factor (2.0x) in addition to existing jitter.
- Scheduler backoff adds ±25% jitter to prevent thundering herd on retries.
- Auto-unsync interval task wired into daemon lifecycle (after session cleanup).

### Fixed

- **Credential file permissions**: TOTP credentials, WebAuthn credentials, session tokens, device registry, and state cache now enforce `chmod 0o600` on write (was umask-dependent).
- **NATS TLS enforcement**: `nats_tls` config option was defined but silently ignored. Now wired into `connect()` with URL scheme upgrade.
- CLI uses config bucket as default push prefix instead of filename.
- NATS rel_path normalization for cross-host state cache lookup.

### Security

- 5 credential write paths now enforce restrictive file permissions (`0o600`).
- NATS connections without TLS log explicit plaintext warning.
- Session authentication required by default for all gRPC calls.

---

## [0.9.3] - 2026-03-20

### Added

- iOS FileProvider: build info section, TestFlight compliance, QR enrollment view.
- Auth credential broker for zero-touch device enrollment.

### Fixed

- FileProvider: EDEADLK hydration deadlock, cbindgen exports, ATS exceptions.
- iOS: QR enrollment + encryption + deep links.

---

## [0.9.0 – 0.9.2] - 2026-03-10 – 2026-03-18

### Added

- Read-write FUSE support: write, create, unlink via SeaweedFS.
- FastCDC chunking for FUSE writes.
- NATS publish on FUSE write + index-first auto-pull.
- Transparent `.tc` suffix for FUSE userspace.
- FUSE directory ops: mkdir, rename, rmdir.
- Vector clock conflict detection in FUSE writes.
- FUSE3 as default mount backend (NFS as fallback).

### Fixed

- Orphaned index entries on skipped uploads.
- NFS: panic detection, timeout handling, in-process mount, sudo on Linux.
- Watcher: skip directories in push path.
- NATS: stream subject filter updates via `create_or_update_stream`.
- VFS: JSON v2 manifest parsing in hydration path.
- Flake: disable stale Attic cache.

### Changed

- Retired legacy FUSE crates (removed 11,418 LOC).

---

## [0.6.0-dev] - Unreleased (pre-0.9.0 development)

### Added

- **RocksDB state cache backend**: `StateCacheBackend` trait with JSON (default) and RocksDB (behind `full` feature) implementations
- **E2E encryption in push/pull pipeline**: `EncryptionContext` wires `tcfs-crypto` into chunk upload/download when `config.crypto.enabled = true`
- **SyncManifest `encrypted_file_key`**: Base64-encoded wrapped per-file key stored in manifest for encrypted files
- **Windows CFAPI wiring**: `tcfs-cloudfilter` provider, hydration, and placeholder modules use `tcfs-sync` manifest parsing and chunk integrity verification
- **macOS FileProvider FFI**: `tcfs-file-provider` exposes C-compatible `extern "C"` functions via cbindgen for Swift consumption
- **Tailscale NATS exposure**: OpenTofu module `tailscale-nats` exposes NATS to tailnet via Tailscale operator (no public IP)
- **Darwin launchd support**: Home Manager module generates `launchd.agents.tcfsd` on macOS, `systemd.user.services.tcfsd` on Linux
- **`syncRoot` option**: Exposed in both NixOS and Home Manager modules for daemon auto-pull target directory
- **`TCFS_S3_ACCESS`/`TCFS_S3_SECRET` env vars**: tcfs-native credential env var names (highest priority in fallback chain)
- **Justfile**: IaC command surface for OpenTofu, Kubernetes, NATS, and build operations
- Encryption round-trip integration tests (`encrypted_roundtrip_test.rs`)
- RocksDB persistence tests (`rocksdb_state_test.rs`)

### Changed

- `tcfs-sync` gains `crypto` feature flag (optional `tcfs-crypto` + `base64` deps)
- `upload_file_with_device()` and `download_file_with_device()` accept optional `EncryptionContext`
- `tcfs-file-provider` crate type changed from lib to `["lib", "staticlib"]` with cbindgen header generation
- Lab fleet examples rewritten from `services.tcfsd` (NixOS) to `programs.tcfs` (Home Manager)
- NATS URL in fleet configs changed to Tailscale MagicDNS (`nats://nats-tcfs:4222`)
- `dist/com.tummycrypt.tcfsd.plist` updated with `--mode daemon` flag and Nix usage guidance
- Fleet deployment docs overhauled: Tailscale NATS, Home Manager startup, corrected env var names
- `just` added to flake.nix devShell

## [0.5.0] - 2026-02-23

### Added

- **ResolveConflict RPC**: Fully wired with keep_local (re-upload manifest with ticked vclock), keep_remote (download remote version), keep_both (rename local + download remote), and defer strategies
- **NATS auto-pull**: State sync loop now downloads remote files automatically in `auto` conflict mode, with vclock comparison and AutoResolver tie-breaking for concurrent edits
- **Hydrate RPC**: Downloads file from `.tc` stub metadata, removes stub after successful hydration
- **Unsync RPC**: Removes path from state cache without deleting remote or local data
- **Watch RPC**: Streams filesystem events (created/modified/deleted) using `notify` crate with recursive watching
- **Mount RPC**: Spawns `tcfs mount` subprocess with active mount tracking
- **Unmount RPC**: Runs `fusermount3 -u` (fallback `fusermount -u`), cleans up tracked subprocess
- `sync_root` config option: local directory root for auto-pull file downloads
- ConflictResolved NATS events published after resolution, merged by remote peers
- 10 new tests: 4 conflict resolution integration tests + 6 vclock comparison unit tests

### Changed

- `spawn_state_sync_loop` now accepts operator, state cache, sync_root, and storage prefix for auto-pull
- `status` RPC reports live `active_mounts` count from tracked subprocess map
- All 11 gRPC RPCs now return meaningful responses (zero `UNIMPLEMENTED` stubs remain)

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

- Rust monorepo with 14 workspace crates
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

[0.5.0]: https://github.com/tinyland-inc/tummycrypt/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/tinyland-inc/tummycrypt/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/tinyland-inc/tummycrypt/compare/v0.2.5...v0.3.0
[0.2.5]: https://github.com/tinyland-inc/tummycrypt/compare/v0.2.1...v0.2.5
[0.2.1]: https://github.com/tinyland-inc/tummycrypt/compare/v0.1.0...v0.2.1
[0.2.0]: https://github.com/tinyland-inc/tummycrypt/releases/tag/v0.2.0
[0.1.0]: https://github.com/tinyland-inc/tummycrypt/releases/tag/v0.1.0
