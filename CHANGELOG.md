# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Current release-proof posture is tracked in `docs/release/evidence/` and
`docs/ops/product-reality-and-priority.md`; older entries below may describe
historical release intent rather than the current supported/proven surface.

## [Unreleased]

### Changed

- **Per-device file-key wrapping is now a tri-state `crypto.wrap_mode`**
  (TIN-1417, replaces the `crypto.per_device_wrapping` boolean). Values:
  `master` (DEFAULT — shared-master wrap, byte-identical to the previous
  default), `dual` (EXPAND/transitional — emits BOTH the master wrap and
  per-device wraps; manifest stays v2 and back-compatible), and `per_device`
  (CONTRACT — emits per-device wraps ONLY, drops the master wrap for true
  revocation, and bumps the manifest to v3 so pre-per-device binaries fail
  CLOSED). Back-compat: a legacy `crypto.per_device_wrapping = true` config
  deserializes to `dual` (keeps the master fallback); `false`/absent maps to
  `master`. A present `wrap_mode` is canonical and wins. The daemon REFUSES to
  drop the master wrap (`per_device`) until a roll-call probe confirms every
  active, non-revoked device carries a real age recipient; otherwise it falls
  back to `dual` and warns loudly. Default remains `master`, so existing fleets
  are unaffected until the migration is explicitly enabled.

## [0.12.14] - 2026-06-03

First finalized release of the 0.12.13→0.12.14 line (0.12.13 shipped only as
rc1–rc4 prereleases). Captures the fail-closed secrets deny-set and the
daemon-startup reliability fixes that landed on `main` after `v0.12.13-rc4` —
previously present in deployed builds but in no published tag.

### Security

- **Fail-closed secrets deny-set in the sync `Blacklist`** (TIN-1737,
  `crates/tcfs-sync/src/blacklist.rs`): secret-bearing paths are never synced —
  `.ssh/`, `.gnupg/`, `sops-nix/` directories; `auth.json`, `.netrc`,
  `.pgpass`, `.credentials.json` files; `.env*` files; and `.sqlite*`/`.db*`
  suffixes. Enforced fail-closed so a misconfiguration cannot leak credentials.
- **Per-device file-key wrapping substrate** (TIN-1417, behind a flag,
  default off): age-recipient per-device key wrapping is now available via
  `crypto.per_device_wrapping`; the default remains shared-master so existing
  fleets are unaffected until the migration lands.
- **Enrollment & auth hardening**: single-use enrollment invites; admin-gated
  enrollment control RPCs; device-invite enrollment bootstrap (TIN-1424);
  real local age identities generated on enrollment; auth sessions are now
  persisted and attached to protected RPCs.

### Added

- **Device registry sync during enrollment** (#476): enrolling a device now
  publishes it into the shared device registry.
- **FileProvider config rendering** (#466, TIN-1425): the daemon and CLI
  render the FileProvider config from the active config; `tcfs init` writes a
  config; FileProvider surface contract is asserted in CI (TIN-1547).
- **`natsTls` config option** (default `false`, #473).
- **Storage restore SLO budgets** in CI (TIN-1622).

### Fixed

- **Daemon control-plane startup hangs** (TIN-1758): readiness no longer blocks
  on remote index discovery; the FileProvider socket bind is kept off the
  runtime workers and serves late socket binds — the daemon comes up reliably
  under load.
- **`tcfs pull <abspath>`** writing into a hash-named file in the cwd instead of
  the requested path (#473).
- Hardened large-restore retry observability; avoided a macOS LaunchAgent
  config restart loop (#457).

## [0.12.13] - 2026-05-18

### Added

- **`tcfs index inspect <path>`** (`crates/tcfs-cli`): read-only CLI
  subcommand that reports a single remote-index entry's state in human
  or JSON form. Statuses: `visible`, `missing_index`, `missing_manifest`,
  `preparing_only`, `no_visible_entry`, `parse_error`. Lets operators
  and the smoke harness distinguish "missing remote fixture" from
  "real FileProvider/FUSE read failure" — the diagnostic primitive that
  unblocked the production Dev ID FileProvider proof packet.
- **Production Dev ID FileProvider acceptance** (M10): first green
  end-to-end macOS post-install smoke against a Developer ID
  notarized `.pkg` on the canonical PZM self-hosted runner, covering
  hydrate, evict/rehydrate, mutation, and conflict-status. Run
  `26062554542`. Archived under `docs/release/evidence/macos-postinstall-prod-devid-hydration-...`.
- **`AGENTS.md`**: canonical agent context at the repo root for Claude
  Code, Codex CLI, and other agent tooling. `.claude/CLAUDE.md` remains
  as a secondary machine-local overlay.
- **Linux post-install smoke scaffold** (TIN-1422):
  `scripts/linux-postinstall-smoke.sh` + `scripts/test-linux-postinstall-smoke.sh`
  + `.github/workflows/linux-postinstall-smoke.yml`. Structural analog
  of the macOS harness gated on the same `tcfs index inspect`
  `status=visible` check. Lands the shape; FUSE evict/rehydrate
  semantics, mutation write-back, conflict-status analog, and Fedora
  RPM matrix entry are tracked as TIN-1422 follow-ups.
- **macOS pkg authenticated install mode** + remote pkg notarization
  proof workflow + required-notarized-release gate.

### Changed

- **`scripts/macos-postinstall-smoke.sh`**: new `--seed-expected-file`
  and `--rebuild-domain` flags. Hydration check now gates on
  `tcfs index inspect` reporting `status=visible` before treating a
  FileProvider read timeout as a desktop bug.
- **`.github/workflows/macos-postinstall-smoke.yml`**: derives
  `storage.enforce_tls` from the endpoint scheme (HTTPS→true, HTTP→false)
  so tailnet-internal smoke endpoints work without sacrificing public
  endpoint hardening. New `exercise_dev_id_layered_proof` input runs
  evict/rehydrate + mutation against production Developer ID without
  requiring `fileprovider_testing_mode=true` (whose entitlement only
  ships on Mac App Development profiles). `exercise_conflict_status`
  validation loosened to allow Dev ID layered proof.
- **macOS FileProvider host app**: respects
  `TCFS_FILEPROVIDER_REBUILD_DOMAIN=1` to remove and re-add the
  FileProvider domain before smoke runs (stale-domain diagnostics).
- **`docs/ops/macos-fileprovider-reality.md`**: lede now records the
  2026-05-18 production Dev ID hydration milestone. Historical context
  preserved with dated inline annotations.

### Fixed

- **Git repo canary chunking**: treat git pack indexes as large
  sequential chunks to avoid pathological FastCDC chunk explosion on
  `.pack`/`.idx` files.
- **Restore semantics**: empty directory markers preserved across
  push/pull roundtrips and reconciliation.
- **macOS pkg signing**: p12 passwords accept blank strings during
  signing proofs (matches some operator key-management flows).

## [0.12.3 - 0.12.12] - 2026-04-17 to 2026-05-08

### Added

- **Release evidence packets**: current proof after `0.12.2` is tracked in
  `docs/release/evidence/`, including Linux lifecycle, PZM testing-mode
  FileProvider lifecycle/conflict, Homebrew/Nix, container, and Linux package
  distribution packets.

### Changed

- Public docs now separate packaged artifact smoke, host-proven lifecycle,
  non-production Apple testing-mode evidence, production Finder acceptance, and
  Kubernetes rollout proof.

## [0.12.2] - 2026-04-16

### Added

- **Distribution proof runbook**: added the canonical release smoke matrix and corrected Homebrew tap flow so post-cut install verification has one current operator path.

### Changed

- Apple and iOS support surfaces now point to dated April 15, 2026 evidence instead of generic support claims, and the odrive parity backlog is refreshed against the current Linux-first product posture.

### Fixed

- **macOS release packaging**: release builds now vendor OpenSSL for macOS arm64 artifacts, and release CI fails if `tcfsd` still links a dynamic Homebrew OpenSSL dylib.
- **Sync state keying**: fixed state-key lookup after delete-through-symlink-parent cases so `remove()` and follow-up reads hit the same cached entry.

## [0.12.1] - 2026-04-15

### Added

- **Named live acceptance lane**: `neo-honey` is now the canonical live SeaweedFS + NATS + two-device smoke path, with a documented script and matching e2e naming.
- **Failure-oriented validation**: added targeted coverage for manifest/index crash windows, retry backoff behavior, NATS durable replay semantics, live storage outage recovery, and CLI/gRPC/MCP/FUSE workflow paths.
- **Orphan chunk reporting and cleanup**: reconcile can now surface orphaned remote chunks and clean them up conservatively after a grace period.

### Changed

- Release workflow now signs GHCR images by immutable digest, honors explicit tags on manual proof runs, and no longer lets Apple notarization outages fail the entire release.
- Release notes and platform/docs surfaces now describe Apple notarization as attempted rather than guaranteed, and keep Apple packaging positioned as experimental.

### Fixed

- **Crash-safe rel-path publish**: manifest/index publication now uses recovery-aware staged, preparing, and committed index states with deterministic crash-window recovery.
- **Upload and path correctness**: fixed upload TOCTOU races, Unicode rel-path normalization, gRPC push path traversal rejection, manifest-read retries, and resumable key rotation.
- **State and lifecycle correctness**: fixed StateCache metadata persistence, PathLocks cleanup under contention, orphan cleanup wiring, and rename/delete sync lifecycle handling across CLI and FUSE flows.

## [0.12.0] - 2026-04-08

### Added

- **Finder status surfaces**: macOS FileProvider gained Finder Sync badge support, download progress reporting, and policy-aware excluded or pinned status badges.
- **Conflict UX improvements**: conflict notifications, conflict-copy remote writes, and CLI policy controls for handling sync conflicts.
- **Apple packaging updates**: release artifacts include Apple Silicon `.pkg`
  installers and bundled `TCFSProvider.app` payloads; current notarization
  posture is tracked in the Apple surface docs and later release-proof evidence.

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
- Container image publishing path for `tcfsd`; current registry and
  architecture proof are tracked in the release evidence docs.
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
