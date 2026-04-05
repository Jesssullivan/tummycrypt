# odrive Linux Agent: Conflict Resolution & Merge Pipeline

**Binary**: `/tmp/odrive-extract/odriveagent`
**Format**: ELF 64-bit LSB, x86-64, unstripped, PyInstaller-packed Python 2.7
**Analysis date**: 2026-04-04
**Method**: Static string extraction + module hierarchy reconstruction

---

## 1. Refresh/Merge Pipeline — Full Data Flow

The core sync reconciliation logic lives in `src.odrive_app.controllers.refresh_job`
with a clear 8-stage pipeline:

```
RefreshController
  |
  v
RefreshJobController
  |
  +-- 1. Refresh           -- fetch remote state from cloud provider
  +-- 2. RefreshChildren   -- enumerate local + remote children for a folder
  +-- 3. GroupByPosition   -- align local/remote items by name/path position
  +-- 4. Compare           -- diff each local item against its remote counterpart
  +-- 5. FileFormats       -- normalize file format differences (.cloud, .cloudf, etc)
  +-- 6. MergeFiles        -- produce merged view from comparison results
  +-- 7. Operations        -- generate concrete sync operations (upload/download/delete)
  +-- 8. LocalAddFolder    -- handle locally-created folders (push to remote)
  +-- 8b. RemoteAddFolder  -- handle remotely-created folders (create local placeholder)
```

### Stage details

**Stage 1 — Refresh**: Queries the cloud provider adapter (via `Sync9Adapter`) for
the current remote directory listing. Each provider has its own adapter
(`GoogleDriveSync9Adapter`, `S3Sync9Adapter`, `DropboxSync9Adapter`, etc.) that
normalizes the provider's API response into a common `ServiceFileAttributes` format.

**Stage 2 — RefreshChildren**: Collects both local filesystem children (via
`FileSystemService`) and remote children (from Stage 1). The `Blacklist` filter is
applied here to exclude system files before merge.

**Stage 3 — GroupByPosition**: Aligns items by their logical position (name/path)
so that local item "foo.txt" is paired with remote item "foo.txt". This is a
name-based matching strategy — there is no content-addressable alignment.

**Stage 4 — Compare**: For each paired item, compares attributes to determine
the sync state: unchanged, locally modified, remotely modified, both modified
(conflict), locally added, remotely added, locally deleted, remotely deleted.
Uses `FileAttributes` and `ServiceFileAttributes` for the comparison.

**Stage 5 — FileFormats**: Handles odrive's placeholder format conversions.
Files with `.cloud` / `.cloudf` extensions are virtual placeholders. This stage
determines whether a placeholder needs hydration or a real file needs dehydration.

**Stage 6 — MergeFiles**: Produces the authoritative merged state from comparison
results. This is where conflict detection finalizes. The merge result feeds into
operation generation.

**Stage 7 — Operations**: Translates the merged state into concrete sync
operations: upload, download, rename, delete, create-placeholder, expand, unsync.

**Stage 8 — LocalAddFolder / RemoteAddFolder**: Special handling for new folders.
Locally-added folders trigger remote creation. Remotely-added folders trigger
local placeholder creation (or full materialization if auto-sync is enabled).

### Pipeline trigger sources

The refresh pipeline is triggered by:
1. `LocalEventController` — FS events from `FSEventService` (inotify/FSEvents)
2. `RemoteEventController` — push notifications from cloud provider
3. `LocalScanController` — periodic full scan of local directory
4. `RemoteScanController` — periodic full scan of remote directory
5. `HeartbeatController` — keepalive that may trigger light refresh
6. `StreamingController` — real-time streaming from supported providers

All of these feed into `EventDispatcher` which routes to `RefreshJobController`.


## 2. Conflict Detection Mechanism

### Detection signals

odrive uses **metadata-based conflict detection**, not content hashing for
primary detection:

| Signal | Source | Evidence |
|--------|--------|----------|
| `FileAttributes` | Local FS | `src.common.FileAttributes(` — wraps stat() |
| `ServiceFileAttributes` | Remote API | `src.integrations_sub.utility.ServiceFileAttributes(` |
| `SyncTrackingValuesV0` | Local DB | Last-known-good state from previous sync |
| `HashCalculator` | Computed | `src.utility.HashCalculator(` — secondary validation |

The primary conflict detection flow:
1. Record `SyncTrackingValuesV0` (mtime, size, hash) at last successful sync
2. On refresh, compare current local `FileAttributes` against tracking values
3. Compare current remote `ServiceFileAttributes` against tracking values
4. If **both** local and remote differ from tracked values → **conflict**
5. If only one side changed → safe directional sync

### Hash algorithms available

```
Crypto.Hash.MD5(       — legacy, likely for provider compatibility
Crypto.Hash.SHA(       — SHA-1
Crypto.Hash.SHA256(    — SHA-256
src.utility.HashCalculator(  — wrapper that selects appropriate algorithm
```

The `HashCalculator` is used for:
- Post-transfer integrity verification
- Content-change detection when timestamps are unreliable
- ETag comparison for S3-compatible providers (S3 ETags are MD5-based for
  non-multipart uploads)

### Three-way state comparison

The `SyncTrackingNode` and `SyncTrackingValuesV0` classes in
`src.sync9.sync_tracking_service` implement a three-way comparison:

```
                 SyncTrackingValuesV0 (base/last-synced)
                /                      \
    Local FileAttributes        Remote ServiceFileAttributes
        (current)                    (current)
```

This is a classic three-way merge base model:
- **Base → Local changed, Remote unchanged**: Upload (local wins)
- **Base → Remote changed, Local unchanged**: Download (remote wins)
- **Base → Both changed**: Conflict
- **Base → Both unchanged**: No action
- **Base → Local deleted, Remote unchanged**: Remote delete
- **Base → Remote deleted, Local unchanged**: Local delete/unsync


## 3. Conflict Naming Conventions & Resolution Strategies

### Naming conventions

The binary contains standard `rename` and `copy` syscall references but **no
odrive-specific conflict suffix patterns** are visible in string extraction.
From public odrive documentation and behavior, the conflict naming format is:

```
filename (conflict YYYY-MM-DD).ext
```

This pattern is likely embedded in the Python bytecode (not extractable via
`strings` alone) within either:
- `MergeFiles` (generates conflict names)
- `FileFormats` (handles naming transformations)
- `Operations` (applies renames)

### Resolution strategies

Based on the module structure, odrive employs:

1. **Rename-aside**: The conflicting local file is renamed with a conflict
   suffix, and the remote version is downloaded. Both versions are preserved.

2. **No automatic resolution**: odrive does not attempt automatic merge of
   file contents. Both versions are preserved for manual resolution.

3. **Last-writer-wins for non-conflicting changes**: When only one side has
   changed, that side's version propagates without user interaction.

4. **Folder conflicts**: `LocalAddFolder` and `RemoteAddFolder` suggest that
   folder-level conflicts (same name created on both sides) are resolved by
   merge — the folder exists on both sides, and children are recursively
   reconciled.


## 4. Locking Mechanism & Concurrent Access

### Application-level locking

```
src.odrive_app.controllers.utility.LockedItem(
```

`LockedItem` is the primary locking primitive. It wraps individual files or
folders during active sync operations to prevent concurrent modifications
from multiple pipeline stages.

### Concurrency model

```
src.utility.ThreadPoolExecutor(              — bounded thread pool for sync workers
concurrent.futures(                          — Python futures for async operations
concurrent.futures.thread(                   — thread-based executor
concurrent.futures.process(                  — process-based executor (available but
                                               likely unused — GIL constraints)
Queue(                                       — work queue for sync jobs
multiprocessing.queues(                      — IPC queues
multiprocessing.pool(                        — process pool
multiprocessing.synchronize(                 — semaphores, locks, events
```

The concurrency architecture:

```
EventServer (socket listener)
  |
  v
EventDispatcher (routes events to controllers)
  |
  +---> ThreadPoolExecutor
  |       |
  |       +-- Worker 1: RefreshJob (folder A)
  |       +-- Worker 2: RefreshJob (folder B)
  |       +-- Worker 3: SyncController (upload file X)
  |       +-- Worker 4: SyncController (download file Y)
  |       ...
  |
  +---> QueuedExpand (queued placeholder expansion requests)
```

### Locking granularity

Given the `LockedItem` wrapper and `ThreadPoolExecutor`, locking is per-item
(file or folder), not global. This allows:
- Multiple folders to refresh concurrently
- Multiple files to upload/download concurrently
- But a single file cannot be refreshed + synced simultaneously

### ReadWriteLock

```
ecdsa._rwlock(
```

An `rwlock` is present (from the ecdsa library). While this is used for ECDSA
key operations, the pattern may have been adopted in odrive's own code for
read-heavy operations (e.g., reading sync state while only one writer updates it).


## 5. Blacklist / Exclusion System

### Architecture

```
src.file_system_sync9.Blacklist(
```

Single class responsible for all file exclusion logic. Co-located with
`FSEventService`, indicating it's applied at the event-ingestion layer.

### Pattern matching support

- `glob(` — Python glob for wildcard matching (e.g., `*.tmp`, `.DS_Store`)
- `stb_regex` / `stb_regex_matcher` — compiled C regex engine for
  performance-critical pattern matching

### Application points

1. **Event filtering**: `FSEventService` consults `Blacklist` before
   promoting FS events to the sync pipeline
2. **Scan filtering**: `RefreshChildren` filters blacklisted items from
   local directory listings
3. **Merge filtering**: Blacklisted items excluded from `Compare`/`MergeFiles`
   to prevent false conflicts

### Default exclusion patterns (inferred from odrive docs)

- `.DS_Store`, `Thumbs.db`, `desktop.ini` — OS metadata
- `~$*`, `*.tmp`, `*.swp` — temp/lock files
- `.git/`, `.svn/`, `.hg/` — VCS directories
- `*.cloud`, `*.cloudf` — odrive placeholders (not synced to remote)
- `$RECYCLE.BIN`, `.Trashes` — trash directories

Full extraction requires PyInstaller unpacking + .pyc decompilation.


## 6. Trash Lifecycle

### Controller

```
src.odrive_app.controllers.TrashController(
```

Dedicated controller for trash operations, separate from the sync pipeline.

### Auto-unsync

```
src.odrive_app.controllers.AutoUnsyncController(
```

Auto-unsync is odrive's mechanism for automatically converting synced files
back to placeholders after a configurable period. This is related to trash
in that unsync'd files can be "trashed" (remote deletion) separately.

### Lifecycle stages

1. **Local delete**: User deletes a file locally
2. **Trash staging**: File enters odrive trash (not OS trash)
3. **Remote delete pending**: Trash item queued for remote deletion
4. **User confirmation**: odrive prompts for confirmation before remote delete
5. **Remote delete**: File removed from cloud provider
6. **Empty trash**: All pending deletes executed

The `EmptyStream` class (`src.utility.EmptyStream(`) suggests empty/zero-byte
stream handling, possibly for cleaning up placeholder state after trash.

### Trash vs. unsync

| Action | Local effect | Remote effect |
|--------|-------------|---------------|
| Delete | File removed | Queued in trash |
| Unsync | File → placeholder | No change |
| Auto-unsync | File → placeholder (timer) | No change |
| Empty trash | Trash cleared | Files deleted remotely |
| Restore from trash | File restored locally | Cancel pending delete |


## 7. Transfer Mechanics

### Upload

```
requests.uploadstream(        — streaming upload via requests library
ftputil.file_transfer(        — FTP-specific transfer
email.mime.multipart(         — multipart form encoding
```

### Chunking

```
chunk(                        — Python chunk handling
stb_compress_chunk.isra.17    — compiled C chunk compression
stb__alloc_chunk              — chunk memory allocation
stb_alloc_chunk_size          — chunk size management
stb__sort_chunks.isra.0       — chunk ordering
```

The presence of chunk-related STB functions indicates:
- Files are split into chunks for upload
- Chunk size is configurable (`stb_alloc_chunk_size`)
- Chunks may be compressed before transfer

### Provider-specific transfer

Each `Sync9Adapter` handles transfer differently:
- **S3/S3-compat**: Multipart upload for large files (ETags for verification)
- **Google Drive**: Resumable upload protocol
- **Dropbox**: Chunked upload sessions
- **FTP/SFTP**: Sequential stream transfer via paramiko/ftputil
- **WebDAV**: HTTP PUT with chunked transfer encoding
- **Box/OneDrive**: Chunked upload sessions

### Integrity verification

Post-transfer verification uses:
- `HashCalculator` — compute local hash
- Compare against remote ETag/hash from provider
- `Crypto.Hash.MD5` / `Crypto.Hash.SHA256` for hash computation


## 8. Finder/UI Integration

### Badge/overlay system

```
src.common.services.badge(
src.common.services.badge.BadgeRefreshService(
src.odrive_app.controllers.utility.Badging(
```

Three-tier badge architecture:
1. `badge` service package — core badge state management
2. `BadgeRefreshService` — refreshes badge state when sync state changes
3. `Badging` utility — applies badges to files/folders

On macOS this uses Finder Sync Extensions. On Linux, there is no native
file-manager badge integration — the badge system likely operates only
through the tray/web UI.

### Notification system

```
src.system_alert(
src.system_alert.NotificationProvider(
src.system_alert.StorageProvider(
src.system_alert.SystemAlertService(
src.system_alert.notification_providers(
src.system_alert.notification_providers.HipChatNotificationProvider(
src.system_alert.storage_providers(
src.system_alert.storage_providers.S3StorageProvider(
```

The notification system is pluggable:
- `NotificationProvider` — abstract base for notification delivery
- `HipChatNotificationProvider` — sends alerts to HipChat (legacy, pre-Slack)
- `S3StorageProvider` — stores alert data in S3 (for dashboard/web access)
- `SystemAlertService` — orchestrates alert generation and delivery

### UI services

```
src.odrive_app.services.AgentUIService(
src.odrive_app.services.LogUIService(
src.odrive_app.services.UIService(
src.odrive_app.controllers.OpenInWebController(
src.odrive_app.controllers.OpenWebPageController(
src.odrive_app.controllers.ValidActionController(
src.odrive_app.controllers.NotAllowedController(
```

The Linux agent exposes a local web UI (or communicates with the desktop
tray app) via:
- `ProtocolServer` / `ProtocolCommands` — local socket protocol
- `EventServer` — event push to UI
- `ValidActionController` — determines which actions are available per-file
- `NotAllowedController` — restricts actions based on account tier


## 9. Concurrency Model

### Thread pool architecture

```
src.utility.ThreadPoolExecutor(
```

Custom `ThreadPoolExecutor` (not stdlib `concurrent.futures.ThreadPoolExecutor`,
though that module is also imported). This is the primary work scheduler.

### Dispatch model

```
src.odrive_app.dispatchers.EventDispatcher(
src.odrive_app.dispatchers.ProtocolRequestDispatcher(
```

Two dispatchers handle different work types:
1. **EventDispatcher** — routes file system events and remote change
   notifications to the appropriate controller
2. **ProtocolRequestDispatcher** — handles user-initiated commands from the
   tray app / CLI (sync, unsync, refresh, trash, etc.)

### Queue system

```
Queue(
src.odrive_app.controllers.sync.internals.QueuedExpand(
multiprocessing.queues(
```

The `QueuedExpand` pattern indicates placeholder expansion (hydration) is
queued rather than immediate. When a user double-clicks a `.cloud` file:
1. Request enters `QueuedExpand` queue
2. `ThreadPoolExecutor` picks up the job
3. `Expand` performs the download
4. Badge state updates via `BadgeRefreshService`

### Thread diagnostics

```
src.common.SendThreadsDump(
src.exception_handling.diagnostics.DetailedThreadDump(
src.exception_handling.diagnostics.ThreadsDump(
src.exception_handling.diagnostics.SyncActivityDump(
src.exception_handling.diagnostics.DiagnosticDump(
src.exception_handling.diagnostics.ExceptionDump(
```

Comprehensive thread dump facilities for debugging deadlocks and hangs.
`SendThreadsDump` likely sends stack traces to odrive's diagnostic service.

### Server architecture

```
src.odrive_app.servers.EventServer(
src.odrive_app.servers.ProtocolServer(
src.odrive_app.servers.Commands(
src.odrive_app.servers.ProtocolCommands(
```

Two server threads:
1. **EventServer** — listens for push events (FS events, remote notifications)
2. **ProtocolServer** — listens for protocol commands from tray/CLI

Both feed into their respective dispatchers.


## 10. Data Model Summary

### Core entities

| Class | Purpose |
|-------|---------|
| `SyncTrackingNode` | Tree node in sync state — represents a file/folder |
| `SyncTrackingValuesV0` | Stored attributes for a tracked node (v0 schema) |
| `FileSyncState` | Current sync status of a file |
| `FileAttributes` | Local file metadata (stat-based) |
| `ServiceFileAttributes` | Remote file metadata (provider-normalized) |
| `O2Path` | Path abstraction for odrive's virtual namespace |
| `LockedItem` | File/folder under active sync operation |
| `StopStatus` | Cancellation token for sync operations |

### Database tables

| Table | Purpose |
|-------|---------|
| `SyncTrackingTable` | Persistent sync state per file/folder |
| `PropertyTable` | Key-value config store |
| `BackupJobTable` | Scheduled backup job definitions |
| `EncryptionEntryTable` | Per-folder encryption configuration |
| `ProSyncFolderTable` | Pro-tier auto-sync folder list |
| `StickySyncTable` | Files pinned to always-synced state |
| `SyncModeTable` | Per-folder sync mode (manual/auto) |
| `IntegrationCacheTable` | Cached remote state per provider |


## 11. Implications for tummycrypt

### Must-implement from odrive's model

1. **Three-way merge base**: Store `SyncTrackingValuesV0` equivalent at each
   successful sync. Without a base, you can only do two-way diff (which
   cannot distinguish "both modified" from "one modified, one unchanged").

2. **Per-item locking**: `LockedItem` pattern prevents races. tummycrypt's
   gRPC push/pull needs similar per-path locks.

3. **Blacklist at event layer**: Filter unwanted files before they enter the
   sync pipeline, not after. Reduces noise and prevents .DS_Store conflicts.

4. **Queued expansion**: Don't block on file hydration. Queue it and process
   asynchronously via the thread pool.

5. **Backoff per provider**: Each cloud integration has its own
   `BackoffChecker`. tummycrypt's NATS backend should have similar
   per-subject rate limiting.

### Can skip or simplify

1. **Badge/overlay system**: tummycrypt runs headless. No Finder integration
   needed. Status can be exposed via gRPC `sync_status` endpoint.

2. **HipChat/Slack notifications**: Replace with NATS-based event streaming
   or simple log output.

3. **Mixpanel telemetry**: Not needed. Prometheus metrics suffice.

4. **Multi-provider adapter factory**: tummycrypt targets NATS only.
   The `SyncAdapterFactory` + 20+ adapters pattern is unnecessary.

### Architecture alignment

| odrive concept | tummycrypt equivalent |
|---------------|----------------------|
| `RefreshJobController` | `pull` command / FUSE readdir |
| `SyncController` | `push` / `pull` gRPC handlers |
| `EventDispatcher` | NATS JetStream consumer |
| `ProtocolServer` | gRPC server (tcfsd) |
| `ThreadPoolExecutor` | tokio runtime task spawner |
| `SyncTrackingTable` | Index file (JSON/SQLite) |
| `Blacklist` | `.tummycryptignore` or config exclude list |
| `LockedItem` | `tokio::sync::RwLock<PathBuf>` |
| `TrashController` | Soft-delete with TTL in NATS KV |
| `BackoffChecker` | Tower retry middleware |
