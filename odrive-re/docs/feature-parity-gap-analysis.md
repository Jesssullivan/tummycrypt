# Feature Parity Gap Analysis: odrive vs tummycrypt

**Date**: 2026-04-15
**Source**: Reverse engineering of `odriveagent` Linux ELF binary (Python 2.7/PyInstaller)
**Target**: tummycrypt workspace v0.12.1 (Rust, 18 crates)

---

## 1. Executive Summary

### 1.1 Refresh For `v0.12.1`

This document originally anchored parity work to `v0.9.1`. As of April 15,
2026, that baseline is too stale in three important ways:

1. tummycrypt now has clearer release and platform-truth surfaces:
   distribution smoke, Apple surface status, explicit iOS posture, and a named
   macOS Finder/FileProvider reality runbook.
2. Apple FileProvider should no longer be treated as "missing" in the codebase,
   but it also should not be counted as solved parity. It exists as an
   experimental desktop and mobile surface with limited acceptance coverage.
3. The next parity backlog should focus on sync lifecycle semantics, folder
   policy, reconciliation, exclusions, and desktop interaction quality rather
   than broadening product claims prematurely.

The matrix below therefore needs to be read through the current `v0.12.1`
product posture:

- Linux remains the best-supported runtime and the strongest proof surface.
- macOS has real Finder/FileProvider code and packaging, but still lacks
  continuously exercised desktop acceptance.
- iOS remains read-only proof-of-concept despite experimental write hooks in the
  scaffold.

tummycrypt already possesses a fundamentally stronger architecture than odrive in several dimensions: CRDT-based conflict resolution (vector clocks vs timestamp comparison), content-defined chunking (FastCDC vs fixed-size XL splitting), FUSE-based virtual filesystem (vs placeholder file extensions that pollute the namespace), fleet-wide sync via NATS JetStream (vs single-machine polling), and modern cryptography (XChaCha20-Poly1305 with proper key hierarchy vs PyCrypto with plaintext passphrase storage).

However, odrive has ~10 years of iteration on the **sync lifecycle** -- the full expand/sync/unsync/auto-unsync pipeline, the refresh/merge eight-stage reconciliation engine, per-folder sync policies, sticky sync settings, blacklist filtering, trash management, and mature error recovery with per-provider backoff. These are the areas where tummycrypt has significant gaps to close.

The recommended path is not to replicate odrive's architecture (which is a monolithic Python 2.7 codebase with deep tech debt), but to adopt its **behavioral semantics** -- the user-facing features and sync guarantees -- while maintaining tummycrypt's superior infrastructure.

**Critical gaps** (must-have for parity): three-way merge base tracking,
auto-unsync with disk pressure awareness, per-folder sync policies,
blacklist/exclude filtering at the event layer, structured refresh pipeline,
and a more explicit desktop acceptance story for Finder/FileProvider behavior.

**Already superior** in tummycrypt: conflict detection (CRDTs), encryption (modern AEAD), chunking (CDC), transport (NATS), IPC (gRPC), authentication (MFA/WebAuthn), and FUSE mount (no filesystem pollution).

---

## 2. Feature Matrix

### 2.1 Sync Engine

| Feature | odrive | tummycrypt | Status | Notes |
|---------|--------|------------|--------|-------|
| Upload file (push) | Sync9Adapter per provider | `tcfs-sync::engine::upload_file` | **has** | TC uses OpenDAL + FastCDC chunking |
| Download file (pull) | Sync9Adapter per provider | `tcfs-sync::engine::download_file` | **has** | TC reassembles from manifest + chunks |
| Directory tree push | CLI `os.walk()` + retry | `tcfs-sync::engine::push_tree` | **has** | TC has `push_tree_with_device()` |
| Content-addressed dedup | Per-provider ETag check | BLAKE3 manifest dedup | **has** | TC is superior: CAS by content hash |
| Incremental sync (skip unchanged) | SyncTrackingTable mtime+size | `StateCache::needs_sync()` stat+hash | **has** | Both fast-path on stat, TC adds hash verify |
| Manifest format | None (provider-native) | `SyncManifest` v2 JSON + vclock | **has** | TC has versioned manifests with vclock metadata |
| Index entries (path-to-manifest) | SyncTrackingTable SQL | `{prefix}/index/{rel_path}` in S3 | **has** | TC uses S3 keys as index |
| Content integrity verification | HashCalculator (MD5/SHA256) | BLAKE3 per-chunk + reassembled verify | **has** | TC is superior: per-chunk + whole-file verification |

### 2.2 Conflict Resolution

| Feature | odrive | tummycrypt | Status | Notes |
|---------|--------|------------|--------|-------|
| Conflict detection | Three-way metadata compare | Vector clock partial ordering | **superior** | TC uses CRDTs; odrive uses mtime/size/ETag |
| Conflict naming | `filename (conflict YYYY-MM-DD).ext` | `filename.conflict-{device_id}.ext` | **has** | Both rename aside; TC includes device provenance |
| Manual resolution | rename-aside, both preserved | `tcfs resolve` CLI + gRPC `ResolveConflict` | **has** | TC has explicit resolution workflow |
| Auto-resolution strategy | None (always manual) | `AutoResolver` (lexicographic device tiebreak) | **superior** | TC has configurable auto-resolution; odrive does not |
| Three-way merge base storage | `SyncTrackingValuesV0` (last-synced state) | `SyncState` in state cache | **partial** | TC stores last-sync state but does not perform structured three-way diff pipeline |
| Concurrent modification detection | Both-sides-changed vs tracked base | `VectorClock::is_concurrent()` | **superior** | TC is mathematically correct; odrive relies on timestamps |

### 2.3 Sync State Tracking

| Feature | odrive | tummycrypt | Status | Notes |
|---------|--------|------------|--------|-------|
| State persistence | SQLite via APSW (19-col SyncTrackingTable) | JSON file or RocksDB (`StateBackend`) | **has** | TC has two backends; odrive has one (SQLite) |
| File sync state enum | 4 states: NOT_SYNCED, SYNCED, ACTIVE, LOCKED | Implicit via `SyncState` presence/absence | **partial** | TC lacks explicit state machine enum |
| Tree-structured state | `SyncTrackingNode` tree with parent OID | Flat HashMap keyed by path | **missing** | TC does not model parent-child relationships |
| Dual local/remote attribute tracking | LocalAttr + RemoteAttr per node | Single `SyncState` (local-centric) | **missing** | TC only tracks local state; no dual-side model |
| Schema evolution | `_has_column()` + ALTER TABLE | JSON schema evolution via serde defaults | **different** | Both handle versioning; different mechanisms |
| Index lookup by remote URI | `iterate_tracking_values_by_remote_uri` | `get_by_rel_path()` linear scan | **partial** | TC's lookup is O(n); odrive has SQL index |
| Timestamp-based aging queries | `iterate_tracking_values_with_timestamp_older_than` | Not implemented | **missing** | Needed for auto-unsync |

### 2.4 Placeholder / Virtual File System

| Feature | odrive | tummycrypt | Status | Notes |
|---------|--------|------------|--------|-------|
| Placeholder files | `.cloud`/`.cloudf` extensions | Linux `.tc`/`.tcf` stubs plus experimental Apple FileProvider placeholders | **superior** | Linux avoids namespace pollution; Apple path uses FileProvider placeholders rather than suffix-based files |
| Expand (hydrate) | `Expand._expand_file` + `_expand_folder` | Linux `Hydrate` gRPC + `tcfs-vfs::hydrate`; experimental Apple `fetchContents` path | **has** | TC hydrates on Linux today and carries a real FileProvider hydration path on Apple surfaces |
| Unsync (dehydrate) | `Unsync._unsync_item` + dirty check | `Unsync` gRPC call | **partial** | TC lacks dirty-child check before unsync |
| XL file (large file splitting) | `.cloudx` extension, segment transfer | FastCDC chunking (no separate concept) | **different** | TC handles large files natively via CDC; no separate "XL" mode |
| Queued expansion | `QueuedExpand` with `InProgressFiles` concurrency | No explicit queue; direct FUSE hydration | **partial** | TC lacks queued/batched expansion for large dirs |
| Stub metadata | None (extension is the placeholder) | `StubMeta` struct with oid, size, chunks, origin | **superior** | TC stubs carry rich metadata; odrive stubs are empty |
| Disk cache for hydrated content | None (files live on disk after expand) | `DiskCache` LRU with shard dirs | **superior** | TC has explicit cache management |
| Negative dentry cache | None | `NegativeCache` TTL-based | **superior** | Prevents repeated S3 lookups for nonexistent paths |

### 2.5 Sync Policies

| Feature | odrive | tummycrypt | Status | Notes |
|---------|--------|------------|--------|-------|
| Placeholder threshold (auto-download by size) | `PlaceholderThresholdController`, `SyncThresholdInfinite` | Not implemented | **missing** | Critical for UX: auto-download small files |
| Per-folder sync mode | `SyncModeTable` (always/on-demand/never) | Not implemented | **missing** | Per-directory behavior configuration |
| Sticky sync settings | `StickySyncTable` (persistent per-folder config) | Not implemented | **missing** | Settings survive restart |
| Pro sync folders (always-synced dirs) | `ProSyncFolderTable` | Not implemented | **missing** | Designated "always-current" directories |
| Auto-unsync (age-based space reclaim) | `AutoUnsyncController` with timestamp sweep | Not implemented | **missing** | Time/access-based dehydration |
| Auto-unsync disk pressure trigger | Threshold-based check | Not implemented | **missing** | Reclaim space when disk is running low |
| Blacklist/exclusion patterns | `Blacklist` class with glob + regex | `CollectConfig.exclude_patterns` (push only) | **partial** | TC has exclude for push but not for VFS/events |
| Git directory handling | Not applicable | `git_safety` module, bundle vs raw mode | **superior** | TC has specialized .git handling |

### 2.6 Refresh / Reconciliation Pipeline

| Feature | odrive | tummycrypt | Status | Notes |
|---------|--------|------------|--------|-------|
| Eight-stage refresh pipeline | Refresh -> RefreshChildren -> GroupByPosition -> Compare -> FileFormats -> MergeFiles -> Operations -> AddFolder | Not implemented as pipeline | **missing** | TC does point-in-time push/pull but lacks structured reconciliation |
| Local scan controller | `LocalScanController` (periodic full scan) | `FileWatcher` (inotify/FSEvents) | **different** | TC uses kernel events (better); may need periodic scan fallback |
| Remote scan controller | `RemoteScanController` (polling remote state) | NATS JetStream consumer | **superior** | TC gets push notifications; odrive polls |
| Compare stage (local vs remote diff) | `Compare` module (268 strings) | Vclock comparison in `compare_clocks()` | **partial** | TC compares but doesn't produce a structured diff list |
| Group-by-position alignment | `GroupByPosition` (align by name/path) | Not implemented | **missing** | Needed for directory-level reconciliation |
| Merge-files stage | `MergeFiles` (produce merged view) | Not implemented | **missing** | Structured merge decision |
| Operations generation | `Operations` (832 strings, largest module) | Direct upload/download in engine | **partial** | TC executes immediately rather than planning then executing |
| Recursive folder reconciliation | `RefreshChildren` (recursive descent) | Not implemented | **missing** | TC handles individual files, not recursive dir reconciliation |

### 2.7 Event System

| Feature | odrive | tummycrypt | Status | Notes |
|---------|--------|------------|--------|-------|
| Local FS events | `FSEventService` (Mac/Win only, polling on Linux) | `FileWatcher` via `notify` crate (inotify/FSEvents) | **superior** | TC has kernel-level events on all platforms |
| Remote change events | Provider-specific polling + webhooks | NATS JetStream `STATE_UPDATES` stream | **superior** | TC has real-time push notifications fleet-wide |
| Event dispatcher | `EventDispatcher` (routes to controllers) | Direct channel from watcher to scheduler | **partial** | TC lacks formal event dispatcher/router |
| Separate local/remote event controllers | `LocalEventController` + `RemoteEventController` | Single `FileWatcher` for local only | **partial** | TC should separate local vs remote event handling |
| Event debounce/coalesce | Implicit (scan interval) | `WatcherConfig.debounce` (500ms default) | **has** | TC has explicit debounce |
| Watch API for external consumers | `EventServer` (socket push) | `Watch` gRPC streaming RPC | **has** | TC exposes watch events via gRPC |
| Heartbeat / connectivity check | `HeartbeatController`, `SystemStatusController` | `Status` gRPC RPC (storage_ok, nats_ok) | **has** | TC has health checks |

### 2.8 Concurrency / Scheduling

| Feature | odrive | tummycrypt | Status | Notes |
|---------|--------|------------|--------|-------|
| Thread pool executor | `OxygenThreadPoolExecutor` | tokio runtime | **has** | TC uses async; odrive uses threads |
| Concurrency limiter | `InProgressFiles` (max files, max bytes) | `SyncScheduler` with semaphore (max_concurrent) | **partial** | TC limits by task count; odrive also limits by bytes in flight |
| Priority queue | Not explicit | `SyncScheduler` with `BinaryHeap` (High/Normal/Low) | **superior** | TC has explicit priority scheduling |
| Per-item locking | `LockedItem` (prevents concurrent ops on same path) | Not implemented | **missing** | TC can race on same file in concurrent tasks |
| Retry with backoff | `BackoffChecker` per provider, `QueueWithRetries` | `SyncScheduler` exponential backoff + NATS `process_with_retry` | **has** | Both have retry; TC has two mechanisms |
| Throughput monitoring | `minThroughput` threshold, dynamic batch sizing | Not implemented | **missing** | odrive adjusts behavior based on transfer speed |
| Rate limiting response | `secondsToDelayForRateLimiting`, `ConcurrentLimitException` | Not implemented | **missing** | TC lacks provider-side rate limit awareness |
| Stop/cancellation tokens | `StopStatus` propagated through all ops | tokio `CancellationToken` (ad-hoc) | **partial** | TC lacks structured cancellation propagation |

### 2.9 Encryption

| Feature | odrive | tummycrypt | Status | Notes |
|---------|--------|------------|--------|-------|
| Encryption algorithm | PyCrypto AES/Blowfish/DES/ARC4 | XChaCha20-Poly1305 (AEAD) | **superior** | TC uses modern authenticated encryption |
| Key derivation | `Crypto.Protocol.KDF` | Argon2id + HKDF | **superior** | TC uses memory-hard KDF |
| Per-file keys | Unknown | Per-file random key wrapped by master key | **superior** | TC has proper key hierarchy |
| Filename encryption | `EncryptedNameExtension` variants | AES-SIV deterministic encryption | **superior** | TC preserves deterministic lookup |
| Encryption as adapter | `EncryptionSync9Adapter` wraps any provider | Feature-gated `crypto` in sync engine | **different** | odrive's adapter pattern is more composable |
| Passphrase storage | Plaintext in `EncryptionEntryTable` SQLite | `tcfs-secrets`: age/SOPS/KeePassXC, keyring | **superior** | TC never stores passphrases in plaintext |
| Recovery | Unknown | BIP-39 mnemonic phrase (`tcfs-crypto::recovery`) | **superior** | TC has standardized recovery |

### 2.10 Platform Integration

| Feature | odrive | tummycrypt | Status | Notes |
|---------|--------|------------|--------|-------|
| macOS Finder badges | `BadgeRefreshService` + Finder Sync Extension | FileProvider decorations and badge identifiers exist, but acceptance is still experimental | **partial** | Badge and decoration code exists, but visible Finder behavior is not yet a release gate |
| macOS package detection | `MacPackageService`, `is_mac_package_type` | Not implemented | **missing** | .app bundles need special handling |
| Windows Cloud Files | Not present (uses placeholder extensions) | `tcfs-cloudfilter` crate (Windows Cloud Filter API) | **superior** | TC has native Windows integration |
| Apple FileProvider | Not present | `tcfs-file-provider` crate plus packaged macOS host app and experimental iOS scaffold | **partial** | Real code and packaging exist, but current acceptance coverage is still weaker than Linux parity claims would require |
| D-Bus integration | Not present | `tcfs-dbus` crate | **superior** | TC has Linux desktop integration |
| System service management | Registry/LaunchAgent/XDG autostart | `service-manager` crate dependency | **has** | Both handle auto-start |
| Keychain/credential store | `KeyChainService` (Python keyring) | `tcfs-secrets` + `keyring` crate | **has** | Both use platform keychains |
| OS trash integration | `local_move_to_os_trash` | Not implemented | **missing** | TC hard-deletes; should offer trash option |
| Context menu integration | Finder extension / shell namespace | FileProvider action declarations exist (`Free Up Space`, `Always Keep on This Device`), but end-to-end Finder proof is still manual | **partial** | Actions are declared in the extension metadata, but desktop UX proof remains experimental |

### 2.11 Authentication & Multi-Device

| Feature | odrive | tummycrypt | Status | Notes |
|---------|--------|------------|--------|-------|
| API key auth | `AuthKeyLoginController` | `AuthUnlock` gRPC (master key) | **has** | Different mechanism, same goal |
| OAuth flow | `AuthorizedUserController` | Not implemented (MFA instead) | **different** | TC uses device-local auth, not OAuth |
| TOTP 2FA | Not present | `tcfs-auth::totp` (RFC 6238) | **superior** | TC has TOTP enrollment and verification |
| WebAuthn/passkeys | Not present | `tcfs-auth::webauthn` (FIDO2) | **superior** | TC has passkey support |
| Device enrollment | Not applicable (single-machine) | `DeviceEnroll` gRPC + invite system | **superior** | TC has fleet enrollment protocol |
| Session management | Cookie-based per provider | `SessionStore` with device identity | **superior** | TC has typed session management |
| Rate limiting auth | Not present | `RateLimiter` with backoff | **superior** | TC rate-limits auth attempts |

### 2.12 Storage Backend

| Feature | odrive | tummycrypt | Status | Notes |
|---------|--------|------------|--------|-------|
| Multi-provider support | 27 adapters (S3, GDrive, Dropbox, etc.) | OpenDAL (S3/SeaweedFS) | **different** | odrive is breadth-first; TC is depth-first |
| Provider adapter pattern | `Sync9Adapter` abstract base + factory | `opendal::Operator` uniform API | **different** | Both abstract storage; OpenDAL is more extensible |
| Integration cache | `IntegrationCacheTable` (cached remote listings) | FUSE attr cache (5s TTL) + `DiskCache` | **partial** | TC caches content; odrive caches listings |
| Provider health/backoff | Per-provider `BackoffChecker` classes | Not implemented | **missing** | TC needs storage-level retry/backoff |
| Multipart upload | Per-provider (S3 multipart, GDrive resumable) | `tcfs-storage::multipart` | **has** | TC has multipart via OpenDAL |
| SeaweedFS-specific optimizations | Not applicable | `tcfs-storage::seaweedfs` module | **superior** | TC has dedicated SeaweedFS support |

### 2.13 Observability

| Feature | odrive | tummycrypt | Status | Notes |
|---------|--------|------------|--------|-------|
| Telemetry | Mixpanel analytics (opt-out?) | Prometheus metrics (`tcfsd::metrics`) | **superior** | TC uses open standards; odrive phones home |
| Diagnostics dump | `DiagnosticDump`, `ThreadsDump`, `SyncActivityDump` | tracing + structured logging | **partial** | TC lacks on-demand diagnostic dump |
| TUI status view | tray app / web UI | `tcfs-tui` (ratatui) | **has** | TC has terminal UI |
| MCP integration | Not present | `tcfs-mcp` crate (Model Context Protocol) | **superior** | TC has AI agent integration |

---

## 3. Executable Backlog As Of `v0.12.1`

These are the highest-value gaps to close if the goal is production parity with
odrive's sync behavior under the current product posture.

Priority order:

1. sync lifecycle correctness and safety
2. policy and auto-unsync behavior
3. reconciliation and exclusion semantics
4. desktop interaction quality on top of truthful platform claims

### 3.1 Explicit File Sync State Machine

**What odrive has**: `FileSyncState` enum with four states (NOT_SYNCED, SYNCED, ACTIVE, LOCKED) tracked per-file in `SyncTrackingTable`. Transitions are guarded and badges update on change.

**What TC needs**: A formal `SyncState` enum in `tcfs-sync` or `tcfs-core` that the daemon, FUSE layer, and CLI all reference. Current state is implicit (presence in state cache = synced, absence = not synced, no "active" or "locked" states).

**Implementation**: Add to `tcfs-sync::state`:
```rust
enum FileSyncStatus { NotSynced, Synced, Active, Locked, Conflict }
```
Store alongside `SyncState` in the state cache. The FUSE driver should query this to show appropriate behavior (e.g., return EAGAIN for ACTIVE files).

### 3.2 Per-Item Locking (`LockedItem` Equivalent)

**What odrive has**: `LockedItem` wrapper prevents concurrent operations on the same file path. Multiple folders can sync concurrently, but a single file cannot be refreshed + synced simultaneously.

**What TC needs**: A `tokio::sync::RwLock`-based path lock manager. The `SyncScheduler` currently does not check if a task's target path is already being processed.

**Implementation**: Add to `tcfs-sync::scheduler` or a new `tcfs-sync::locks` module:
```rust
struct PathLocks {
    locks: DashMap<PathBuf, Arc<RwLock<()>>>,
}
```
Acquire write lock before push/pull/unsync operations; read lock for status queries.

### 3.3 Auto-Unsync with Timestamp Aging

**What odrive has**: `AutoUnsyncController` runs a background sweep using `NodeTimestamp` column, checking `iterate_tracking_values_with_timestamp_older_than`. Files not accessed within the threshold are dehydrated to placeholders. Delivers OS notification with space saved.

**What TC needs**: A background task in `tcfsd` that:
1. Periodically scans state cache entries by `last_synced` or file atime
2. Checks if files exceed an age threshold
3. Calls the unsync path for eligible files
4. Respects a "pinned" list (equivalent of StickySyncTable)
5. Reports space reclaimed

**Implementation location**: New module `tcfsd::auto_unsync` or extend `tcfs-sync::scheduler`.

### 3.4 Per-Folder Sync Policies

**What odrive has**: Three SQL tables control per-folder behavior:
- `SyncModeTable`: sync mode per folder (always/on-demand/never/streaming)
- `StickySyncTable`: persistent per-folder settings (download threshold, expand subfolders)
- `ProSyncFolderTable`: designated always-synced folders

**What TC needs**: A policy system that maps folder paths to sync behaviors. This should be stored in the state cache (or a separate config file) and consulted by:
- The FUSE driver (should it auto-hydrate?)
- The watcher (should it auto-push changes?)
- The auto-unsync controller (should it dehydrate old files here?)

**Implementation**: Add a `PolicyStore` to `tcfs-core::config` with per-path rules:
```rust
struct FolderPolicy {
    sync_mode: SyncMode,          // Always, OnDemand, Never
    download_threshold: Option<u64>, // auto-hydrate below this size
    auto_unsync_exempt: bool,     // never auto-dehydrate
}
```

### 3.5 Blacklist/Exclude at Event Layer

**What odrive has**: `Blacklist` class applied at three points: FSEvent ingestion, RefreshChildren scan filtering, and Compare/MergeFiles filtering. Prevents `.DS_Store`, `Thumbs.db`, temp files, VCS directories from entering the sync pipeline at all.

**What TC has**: `CollectConfig.exclude_patterns` in `tcfs-sync::engine::collect_files()` (push only). The FUSE/VFS layer has no exclusion logic. The watcher ignores `.git`, `.DS_Store`, `target`, `node_modules` by name match but has no glob support.

**What TC needs**: A centralized `Blacklist` in `tcfs-core` that is consulted by:
1. `FileWatcher` (skip events for excluded paths)
2. `collect_files()` (current behavior, consolidate patterns)
3. `TcfsVfs` (skip excluded entries in readdir)

### 3.6 Structured Refresh/Reconciliation Pipeline

**What odrive has**: An eight-stage pipeline (Refresh -> RefreshChildren -> GroupByPosition -> Compare -> FileFormats -> MergeFiles -> Operations -> AddFolder) that produces a complete reconciliation plan before executing any operations.

**What TC has**: Point operations -- `upload_file`, `download_file`, `push_tree` -- that execute immediately. No directory-level reconciliation that detects remote-only, local-only, and both-modified items in a single pass.

**What TC needs**: A `Reconciler` that:
1. Lists remote index entries for a prefix
2. Walks local filesystem
3. Aligns items by path (like GroupByPosition)
4. Compares via vector clocks (like Compare)
5. Produces an operation plan (upload/download/conflict/delete)
6. Executes the plan

**Implementation**: New module `tcfs-sync::reconcile` consuming `tcfs-sync::engine` primitives.

### 3.7 Unsync Dirty-Child Safety Check

**What odrive has**: Before unsyncing a folder, `_first_item_that_cannot_unsync` iterates children, checks for modified-but-not-uploaded content (`is_content_change`), and prompts the user if dirty children are found.

**What TC has**: `Unsync` gRPC call with a `force` flag but no dirty-child check.

**What TC needs**: Before unsync, scan the subtree for files where `needs_sync()` returns `Some(reason)`. If any dirty children exist and `force` is false, return an error listing the dirty paths.

---

## 4. Lower-Priority Gaps (Nice-to-Have)

### 4.1 Throughput Monitoring and Dynamic Batch Sizing

odrive's `QueuedExpand` monitors `minThroughput` per download, dynamically reduces batch sizes when throughput drops, and delays on rate limits. TC's `SyncScheduler` has fixed `max_concurrent`. Adding adaptive concurrency based on observed throughput would improve behavior on slow or rate-limited connections.

### 4.2 OS Trash Integration

odrive offers `_sendToOsTrash` as an alternative to hard-delete during unsync and conflict resolution. TC should expose this as a configuration option, using platform trash APIs.

### 4.3 macOS Finder Badge Integration

odrive has `BadgeRefreshService` for Finder overlay icons. TC has `tcfs-file-provider` for Apple FileProvider integration which is more modern, but desktop Finder badge overlays (via Finder Sync Extension) would improve the Mac UX for non-FileProvider use cases.

### 4.4 Streaming Mode

odrive has a `StreamingController` for real-time streaming from supported providers. TC could implement this via NATS streaming consumers for live-tail of remote changes.

### 4.5 Backup Job Scheduling

odrive has `BackupJobTable` for scheduled backup definitions (source path, remote destination, settings). TC could add scheduled push jobs via the `tcfs-sync::scheduler`.

### 4.6 On-Demand Diagnostic Dump

odrive generates `DiagnosticDump`, `ThreadsDump`, `SyncActivityDump`, `ExceptionDump` on demand. TC should expose a `Diagnostics` gRPC call that dumps scheduler state, active tasks, NATS consumer lag, cache stats, and recent errors.

### 4.7 Encryption-as-Adapter Composability

odrive's `EncryptionSync9Adapter` wraps any storage adapter, making encryption transparent and composable. TC's encryption is feature-gated in the sync engine with `#[cfg(feature = "crypto")]` blocks throughout `upload_file_with_device` and `download_file_with_device`. A middleware/adapter pattern in TC's OpenDAL operator chain would be cleaner.

---

## 5. Areas Where tummycrypt Is Already Superior

### 5.1 Conflict Resolution (Vector Clocks vs Timestamps)

TC's `VectorClock` in `tcfs-sync::conflict` provides mathematically correct partial ordering of concurrent events. odrive's three-way compare uses mtime/size, which can miss conflicts when clocks are skewed or files are modified within the same second. TC's proptest suite validates CRDT properties (commutativity, associativity, idempotency, antisymmetry).

### 5.2 Content-Defined Chunking (FastCDC vs Fixed Splits)

TC uses FastCDC (`tcfs-chunks::fastcdc`) which produces stable chunk boundaries even when content is inserted or deleted. odrive's XL file splitting uses fixed-size segments, meaning a single-byte insertion shifts all subsequent chunk boundaries, causing full re-upload.

### 5.3 FUSE Mount (Virtual FS vs Placeholder Extensions)

TC's `tcfs-fuse` + `tcfs-vfs` presents cloud files as a native filesystem mount. Files appear with their real names and extensions. odrive pollutes the filesystem with `.cloud`/`.cloudf`/`.cloudx` extensions that confuse applications, break file associations, and require extension-aware tooling.

### 5.4 Fleet Sync via NATS JetStream

TC's `tcfs-sync::nats` provides:
- `SYNC_TASKS` stream (work queue for push/pull/unsync)
- `HYDRATION_EVENTS` stream (FUSE hydration coordination)
- `STATE_UPDATES` stream (hierarchical `STATE.{device}.{event}` subjects)
- Per-device durable consumers with catch-up from last sequence

odrive is single-machine only -- no fleet awareness, no push notifications, no distributed work queue.

### 5.5 Modern Cryptography

TC's `tcfs-crypto` uses:
- XChaCha20-Poly1305 AEAD (vs PyCrypto AES/Blowfish/DES/ARC4)
- Argon2id key derivation (vs generic KDF)
- Per-file random keys wrapped by master key (vs shared passphrase)
- AES-SIV for deterministic filename encryption
- HKDF for domain-separated subkeys
- BIP-39 mnemonic recovery phrases

odrive stores passphrases in plaintext SQLite. TC never persists secrets in plaintext.

### 5.6 Authentication (MFA + Device Enrollment)

TC's `tcfs-auth` provides:
- TOTP (RFC 6238) provider
- WebAuthn/FIDO2/passkey provider
- Device enrollment via cryptographic invite
- Session management with device identity and permissions
- Rate limiting with backoff

odrive has basic API key and OAuth -- no MFA, no device enrollment protocol.

### 5.7 Platform Integration Breadth

TC already has crates for:
- `tcfs-cloudfilter`: Windows Cloud Filter API (native cloud files)
- `tcfs-file-provider`: Apple FileProvider (iOS/macOS Files.app)
- `tcfs-dbus`: Linux D-Bus integration
- `tcfs-nfs`: NFSv3 backend for VFS

odrive only has Finder Sync Extension (macOS) and shell namespace (Windows).

### 5.8 gRPC + Streaming (vs JSON-over-TCP)

TC's gRPC service has:
- Typed protobuf messages (vs untyped JSON)
- Bidirectional streaming for `Push`, `Pull`, `Hydrate`, `Watch`
- Unix domain socket transport (lower overhead)
- Code generation for any language

odrive uses JSON-over-TCP with one connection per command, no multiplexing, no type safety.

### 5.9 Metrics and Observability

TC has Prometheus metrics (`tcfsd::metrics`), structured JSON logging via `tracing`, and an MCP integration (`tcfs-mcp`) for AI agent tooling. odrive sends telemetry to Mixpanel (proprietary, privacy concern).

---

## 6. Recommended Implementation Order

Ordered by value-to-effort ratio, with dependencies noted.

### Phase 1: Sync Safety (prerequisite for all else)

1. **Per-item path locking** (Section 3.2)
   - Effort: 1-2 days
   - Why first: prevents data races in all subsequent features
   - Crate: `tcfs-sync::locks` or extend `tcfs-sync::scheduler`

2. **Explicit FileSyncStatus enum** (Section 3.1)
   - Effort: 1 day
   - Why: foundation for all state-dependent features
   - Crate: `tcfs-sync::state`

3. **Unsync dirty-child check** (Section 3.7)
   - Effort: 0.5 days
   - Why: prevents data loss on folder unsync
   - Crate: `tcfsd::grpc` (Unsync handler)

### Phase 2: Reconciliation Engine

4. **Centralized blacklist** (Section 3.5)
   - Effort: 1-2 days
   - Why: prerequisite for reconciliation (must filter before comparing)
   - Crate: `tcfs-core::blacklist` (new), consumed by `tcfs-sync`, `tcfs-vfs`, `tcfs-fuse`

5. **Directory reconciliation pipeline** (Section 3.6)
   - Effort: 3-5 days
   - Why: the core missing feature for bidirectional sync
   - Crate: `tcfs-sync::reconcile` (new)
   - Depends on: blacklist, path locking, FileSyncStatus

### Phase 3: Sync Policies

6. **Per-folder sync policies** (Section 3.4)
   - Effort: 2-3 days
   - Why: enables placeholder threshold and auto-download
   - Crate: `tcfs-core::policy` (new)

7. **Auto-unsync controller** (Section 3.3)
   - Effort: 2 days
   - Why: automatic space reclamation, high user value
   - Crate: `tcfsd::auto_unsync` (new)
   - Depends on: sync policies, FileSyncStatus

### Phase 4: Robustness

8. **Storage-level backoff/retry** (Section 4.7 related)
   - Effort: 1-2 days
   - Crate: `tcfs-storage` (Tower retry middleware)

9. **Throughput monitoring / adaptive concurrency** (Section 4.1)
   - Effort: 2 days
   - Crate: `tcfs-sync::scheduler`

10. **Diagnostic dump endpoint** (Section 4.6)
    - Effort: 1 day
    - Crate: `tcfsd::grpc` (new RPC)

---

## 7. Architectural Patterns Worth Adopting

### 7.1 Dual Local/Remote Attribute Model

**odrive pattern**: `SyncTrackingNode` stores both `localValues` and `remoteValues` as `SyncTrackingValuesV0`. This enables three-way comparison: base (stored) vs current-local vs current-remote.

**TC adaptation**: Extend `SyncState` to store `remote_blake3`, `remote_size`, `remote_mtime` alongside the existing local fields. This enables the reconciler to detect which side changed without fetching the remote manifest every time.

```rust
struct SyncState {
    // ... existing fields ...
    remote_blake3: String,    // last-known remote hash
    remote_size: u64,         // last-known remote size
    remote_vclock: VectorClock, // last-known remote vclock
}
```

### 7.2 Separate Event Controllers

**odrive pattern**: `LocalEventController`, `RemoteEventController`, and `TrackedEventController` handle different event sources with different processing logic.

**TC adaptation**: The current `FileWatcher` handles local events only. NATS `STATE_UPDATES` events are processed in the daemon worker loop. Formalize this into:
- `LocalEventHandler`: processes `FileWatcher` events -> enqueue push tasks
- `RemoteEventHandler`: processes NATS state events -> enqueue pull tasks
- `TrackedEventHandler`: monitors active operations for completion/failure

### 7.3 Schema Evolution via Column Detection

**odrive pattern**: `_has_column()` with `PRAGMA table_info()` checks for column existence before ALTER TABLE. This allows upgrading from older versions without data loss.

**TC adaptation**: TC's JSON state cache handles this via `#[serde(default)]` attributes (already used for `vclock` and `device_id` fields). The RocksDB backend should apply similar forward-compatible deserialization. If TC ever moves to SQLite, adopt odrive's ALTER TABLE pattern.

### 7.4 Operation Plan-Then-Execute

**odrive pattern**: The refresh pipeline produces a complete plan (list of Operations) before executing any of them. This allows validation, conflict detection, and user confirmation before any side effects.

**TC adaptation**: The reconciler (Section 3.6) should produce a `ReconciliationPlan`:
```rust
struct ReconciliationPlan {
    uploads: Vec<UploadOp>,
    downloads: Vec<DownloadOp>,
    conflicts: Vec<ConflictOp>,
    deletes: Vec<DeleteOp>,
    no_ops: usize,
}
```
This plan can be displayed to the user (via TUI or CLI), modified, and then executed. This is particularly important for first-time sync of large directories.

### 7.5 Queued Expansion with Priority/Retry

**odrive pattern**: `QueuedExpand` with `QueueWithRetries` implements a queue that has `put_front()` (high-priority retry) and `put_back()` (low-priority retry), with `InProgressFiles` limiting concurrent downloads by both count and bytes.

**TC adaptation**: TC's `SyncScheduler` already has priority levels. Extend it with:
- Byte-based concurrency limit (not just task count)
- Front-of-queue retry for recoverable errors
- Back-of-queue retry for rate-limited operations

---

## 8. Anti-Patterns to Avoid

### 8.1 Plaintext Secret Storage

odrive stores encryption passphrases in plaintext in `EncryptionEntryTable` SQLite. This is a critical security flaw. TC's `tcfs-secrets` + `tcfs-crypto` approach (keyring, age, SOPS, never-plaintext) is correct and must be maintained.

### 8.2 Monolithic Provider Coupling

odrive bundles 27 provider adapters in a single binary. Each adapter has its own `BackoffChecker`, `Service` class, and `Sync9Adapter` -- tight coupling that makes the binary large and hard to maintain. TC's OpenDAL abstraction is cleaner; do not add per-provider code paths.

### 8.3 Python 2.7 Thread Pool (GIL)

odrive's `OxygenThreadPoolExecutor` is limited by Python's GIL -- true parallelism only exists for I/O-bound operations. TC's tokio async runtime has no such limitation and should not artificially replicate odrive's threading model.

### 8.4 Polling-Based Remote Detection

odrive polls remote providers at intervals because there is no universal push notification mechanism across 27 providers. TC has NATS JetStream push notifications and should never fall back to polling for remote state changes.

### 8.5 Extension-Based Placeholders

odrive's `.cloud`/`.cloudf`/`.cloudx` extension system pollutes the filesystem namespace, confuses applications, breaks file type associations, and creates nine(!) distinct extension types to handle encryption combinations. TC's FUSE-based virtual filesystem avoids all of these problems and must be preserved.

### 8.6 Implicit State Machine

odrive's `FileSyncState` enum exists but transitions are not formally guarded -- they happen through various code paths in controllers and services. TC should enforce state transitions through a proper state machine (e.g., `typestates` pattern or explicit transition methods that validate preconditions).

### 8.7 Mixpanel Telemetry

odrive's `MixpanelUtil` sends usage data to a third-party analytics service. TC should only use opt-in, self-hosted metrics (Prometheus) and structured logs. Never phone home without explicit user consent.

### 8.8 Timestamp-Based Conflict Detection

odrive's `Compare` module uses mtime/size as the primary conflict signal. This fails with clock skew, sub-second modifications, and copy operations that preserve mtime. TC's vector clock approach is mathematically correct and should remain the primary conflict detection mechanism.

---

## Appendix: Crate-to-Feature Mapping

| tummycrypt Crate | Features It Already Provides | Features It Needs |
|-----------------|------------------------------|-------------------|
| `tcfs-core` | Config, proto, types, error | Blacklist, PolicyStore |
| `tcfs-sync` | Engine, conflict (vclock), NATS, manifest, state, watcher, scheduler | Reconciler, path locks, FileSyncStatus enum, auto-unsync |
| `tcfs-vfs` | VFS trait, TcfsVfs, DiskCache, NegativeCache, stubs, hydration | Blacklist integration, policy-aware readdir |
| `tcfs-fuse` | FUSE PathFilesystem adapter | FileSyncStatus-aware behavior, queued expansion |
| `tcfs-crypto` | E2E encryption, KDF, key hierarchy, name encryption, recovery | Middleware/adapter pattern for composability |
| `tcfs-chunks` | FastCDC, BLAKE3, seekable zstd, delta (stub) | -- (complete) |
| `tcfs-storage` | OpenDAL operator, multipart, SeaweedFS, health | Retry/backoff middleware |
| `tcfs-auth` | TOTP, WebAuthn, sessions, enrollment, rate limiting | -- (complete) |
| `tcfs-secrets` | age, SOPS, KeePassXC integration | -- (complete) |
| `tcfsd` | Daemon, gRPC server, metrics, worker | Auto-unsync controller, diagnostics RPC |
| `tcfs-cli` | CLI client | -- (feature-dependent on daemon) |
| `tcfs-tui` | Terminal UI | Sync status dashboard |
| `tcfs-mcp` | MCP server for AI agents | -- (complete) |
| `tcfs-cloudfilter` | Windows Cloud Filter API | -- (platform-specific) |
| `tcfs-file-provider` | Apple FileProvider | -- (platform-specific) |
| `tcfs-dbus` | Linux D-Bus integration | -- (platform-specific) |
| `tcfs-nfs` | NFSv3 VFS backend | -- (complete) |
