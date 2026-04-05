# odrive Desktop Client Architecture

Reconstructed from Linux agent debug symbols (unstripped ELF) and `odrive.py` CLI analysis.

## Overview

odrive is a Python 2.7 application packed with PyInstaller. It uses a daemon/client
architecture: a persistent **SyncAgent** daemon handles all sync logic, and a thin
**CLI/Desktop** client communicates via JSON-over-TCP IPC on loopback.

The core sync engine is versioned as **Sync9** and uses an adapter pattern to
support 27 cloud storage providers through a uniform interface.

## System Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                    User Interface Layer                       │
│  odrive.py CLI ──TCP──► ProtocolServer ──► RequestDispatcher │
│  Desktop GUI   ──TCP──►                                      │
│  EventServer (FS events, cloud events)                       │
└────────────────────────────┬────────────────────────────────┘
                             │
┌────────────────────────────▼────────────────────────────────┐
│                    Controller Layer (30+)                     │
│                                                              │
│  Sync Pipeline:     SyncController                           │
│    ├── Expand       (placeholder → real file/dir)            │
│    ├── QueuedExpand (batched expansions)                      │
│    ├── Sync         (download file content)                   │
│    ├── Unsync       (real → placeholder)                      │
│    └── AddCloudChildren (populate dir with placeholders)     │
│                                                              │
│  Refresh Pipeline:  RefreshJobController                     │
│    ├── Compare      (diff local vs remote)                   │
│    ├── GroupByPosition (classify changes)                     │
│    ├── MergeFiles   (resolve differences)                    │
│    ├── Operations   (execute merge decisions)                │
│    ├── LocalAddFolder / RemoteAddFolder                      │
│    └── RefreshChildren (recursive descent)                   │
│                                                              │
│  Feature:  Encryption, Mount, Backup, Trash, Streaming       │
│  Policy:   AutoUnsync, PlaceholderThreshold, FolderSyncRule  │
│  Events:   LocalEvent, RemoteEvent, LocalScan, RemoteScan    │
│  State:    AgentSyncState, SystemStatus, Heartbeat           │
│  Auth:     AuthKeyLogin, AuthorizedUser                      │
└────────────────────────────┬────────────────────────────────┘
                             │
┌────────────────────────────▼────────────────────────────────┐
│                    Service Layer                              │
│  OdriveSyncService, PlaceholderService, PolicyService        │
│  AgentUIService, MacPackageService, odriveService            │
└────────────────────────────┬────────────────────────────────┘
                             │
┌────────────────────────────▼────────────────────────────────┐
│                    Sync9 Engine                               │
│  Sync9Service ──► Sync9Adapter (abstract base)               │
│  SyncTrackingService ──► SyncTrackingNode (tree)             │
│  SyncTrackingValuesV0 (per-node state)                       │
│  FileSyncState, O2Path, StopStatus                           │
└────────────┬───────────────────────────────┬────────────────┘
             │                               │
┌────────────▼────────────┐  ┌──────────────▼─────────────────┐
│   File System Layer      │  │   Cloud Integration Layer       │
│                          │  │                                 │
│  AbstractFileSystemSvc   │  │  SyncAdapterFactory             │
│  NativeFileSystemSvc     │  │    ├── S3Sync9Adapter           │
│  OdriveFSService         │  │    ├── DropboxSync9Adapter      │
│  FSEventService          │  │    ├── GoogleDriveSync9Adapter  │
│    ├── MacFSEventSvc     │  │    ├── OneDriveSync9Adapter     │
│    └── WindowsFSEventSvc │  │    ├── EncryptionSync9Adapter   │
│  Blacklist               │  │    └── ... (27 total)           │
│  FileSystemCodes         │  │                                 │
└──────────────────────────┘  │  *Service (API layer per provider)
                              │    ├── S3Service                │
                              │    ├── DropboxService           │
                              │    └── ...                      │
                              └─────────────────────────────────┘
                                             │
┌────────────────────────────────────────────▼────────────────┐
│                    Database Layer (APSW/SQLite)               │
│                                                              │
│  PropertyDB:        PropertyTable, SyncModeTable,            │
│                     StickySyncTable, ProSyncFolderTable,     │
│                     BackupJobTable, EncryptionEntryTable     │
│                                                              │
│  SyncTrackingDB:    SyncTrackingTable                        │
│                                                              │
│  IntegrationCacheDB: IntegrationCacheTable                   │
└──────────────────────────────────────────────────────────────┘
```

## Key Design Patterns

### 1. Adapter Pattern (Sync9Adapter)
Each cloud provider implements `Sync9Adapter` (sync/metadata logic) paired with
a `*Service` class (raw API calls). `SyncAdapterFactory` selects the correct
adapter at runtime. This two-tier separation means sync logic is provider-agnostic.

### 2. Encryption as Virtual Provider
`EncryptionSync9Adapter` wraps another provider's adapter, implementing encryption
as a transparent layer in the adapter chain rather than a transport concern. This
allows any provider to be encrypted without provider-specific changes.

### 3. Placeholder Lazy-Loading
Files exist as lightweight `.cloud`/`.cloudf` stubs until explicitly expanded.
This is the core UX innovation — no upfront sync of the entire cloud. Thresholds
control automatic expansion by file size.

### 4. Event-Driven Dual Pipeline
- **Local events** (FSEvent/inotify) detect filesystem changes → upload
- **Remote events** (polling/webhooks per provider) detect cloud changes → refresh
- Both feed into the Refresh pipeline for merge resolution

### 5. Client-Side Recursive Sync
The agent only handles single-file operations. Recursive sync is implemented
entirely in the CLI via `os.walk()` with a retry/stall detector. This keeps
the agent stateless with respect to batch operations.

### 6. Tree-Structured State Tracking
`SyncTrackingService` maintains a tree of `SyncTrackingNode` objects, each with
`SyncTrackingValuesV0` state. This mirrors the filesystem hierarchy and tracks
per-node sync status, enabling efficient subtree queries.

## Refresh/Merge Pipeline (Conflict Resolution)

The most complex subsystem. When a refresh is triggered:

```
1. Refresh        → Fetch remote state for a folder
2. Compare        → Diff local filesystem vs remote state
3. GroupByPosition → Classify each item:
                     - Local-only (new local file/folder)
                     - Remote-only (new remote file/folder)
                     - Both (exists in both, may differ)
4. MergeFiles     → For items in both: resolve conflicts
                     - Same content → no-op
                     - Different → conflict handling
5. Operations     → Execute the merge plan:
                     - LocalAddFolder: create local dirs for remote folders
                     - RemoteAddFolder: create remote dirs for local folders
                     - Download/upload changed files
                     - Create conflict copies if needed
6. RefreshChildren → Recurse into subdirectories
```

## IPC Protocol Summary

JSON-over-TCP on `127.0.0.1`, port from `~/.odrive-agent/.oreg`. 25 commands in
three categories: fire-and-forget (7), synchronous (16), binary streaming (2).
One TCP connection per command, no multiplexing. See `analysis/ipc-protocol/`
for full specification.

## Comparison with tummycrypt

| Aspect | odrive | tummycrypt |
|--------|--------|------------|
| Language | Python 2.7 | Rust |
| IPC | JSON-over-TCP | gRPC over UDS |
| Storage | 27 cloud adapters | OpenDAL (S3/SeaweedFS) |
| Sync | Polling + FSEvents | NATS JetStream + vector clocks |
| Encryption | PyCrypto (AES/RSA) via adapter | XChaCha20-Poly1305 + BLAKE3 |
| Chunking | XL file splitting | FastCDC content-defined |
| State | SQLite (APSW) | SQLite (local) + NATS (fleet) |
| Placeholders | .cloud/.cloudf files | FUSE mount (virtual) |
| Conflict | MergeFiles pipeline | Vector clock CRDT |

### Key Patterns to Adopt
1. **Placeholder threshold policies** — auto-download by file size
2. **Folder sync rules** — per-directory sync policies
3. **Auto-unsync** — reclaim disk by reverting old synced files to placeholders
4. **Trash management** — soft-delete with configurable auto-empty
5. **Dual event pipeline** — separate local/remote event handling
6. **Refresh/merge pipeline stages** — structured conflict resolution

### Patterns Already Superior in tummycrypt
1. **CRDT vector clocks** vs odrive's timestamp-based compare
2. **Content-defined chunking (FastCDC)** vs XL file splitting
3. **FUSE mount** vs placeholder files (no extension pollution)
4. **gRPC** vs JSON-over-TCP (typed, streaming, bidirectional)
5. **Fleet sync via NATS** vs single-machine polling
