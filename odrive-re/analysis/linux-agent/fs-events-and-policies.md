# odrive Linux Agent: FS Events & Policy System Analysis

Binary: `/tmp/odrive-extract/odriveagent`
Format: ELF 64-bit x86-64, unstripped, PyInstaller-packed Python 2.7
Compiler: GCC 4.3.4 (SUSE Linux), targets GNU/Linux 2.6.32
Analysis date: 2026-04-04

---

## 1. Architecture Overview

The agent is a monolithic Python 2.7 application packed with PyInstaller. The codebase
follows an MVC-like architecture with clear separation between:

- **Controllers** (`src.odrive_app.controllers.*`) -- business logic
- **Services** (`src.odrive_app.services.*`) -- persistence, UI, policy
- **Dispatchers** (`src.odrive_app.dispatchers.*`) -- event routing
- **Servers** (`src.odrive_app.servers.*`) -- protocol/event listeners
- **Integrations** (`src.integrations.*`) -- per-provider cloud adapters

The sync engine is versioned as "Sync9" (`src.sync9.*`), indicating at least 9
major iterations of the core sync algorithm.

---

## 2. Filesystem Event Detection

### 2.1 Platform-Specific Event Services

The FS event system uses a strategy pattern with platform-specific implementations:

```
src.file_system_sync9.FSEventService        -- base/interface
src.file_system_sync9.MacFSEventService     -- macOS FSEvents API
src.file_system_sync9.WindowsFSEventService -- Windows ReadDirectoryChangesW
```

**Notable absence**: There is NO `LinuxFSEventService` or `InotifyEventService` class.
The Linux agent appears to rely on **polling/scanning** rather than kernel-level inotify
notifications. This is consistent with the binary targeting GNU/Linux 2.6.32, where
inotify was available but had watch count limitations that would be problematic for a
sync agent managing arbitrary directory trees.

The presence of Mac and Windows FS event service classes in a Linux binary is expected
for a PyInstaller bundle -- it packs the entire codebase regardless of target platform.

### 2.2 Event Controller Separation

Events are split into three distinct controller types:

| Controller | Role |
|---|---|
| `LocalEventController` | Handles filesystem changes detected locally |
| `RemoteEventController` | Handles changes pushed from cloud providers |
| `TrackedEventController` | Handles events for files under active tracking |

This three-way split suggests the agent maintains an event model where:
1. Local FS changes are detected (scan or FS events)
2. Remote changes arrive via provider polling or webhooks
3. Active sync operations generate tracked events for progress/completion

### 2.3 Event Dispatching

```
src.odrive_app.dispatchers.EventDispatcher          -- routes FS/sync events
src.odrive_app.dispatchers.ProtocolRequestDispatcher -- routes CLI/API commands
src.odrive_app.servers.EventServer                   -- listens for events
src.odrive_app.servers.ProtocolServer                -- listens for commands
src.odrive_app.servers.ProtocolCommands              -- command definitions
src.odrive_app.servers.Commands                      -- command implementations
```

The dual-dispatcher architecture separates **events** (async, FS-driven) from
**protocol requests** (sync, user/API-driven). The EventServer likely runs on a
dedicated thread, receiving events from FS watchers or scan results.

---

## 3. Directory Scanning

### 3.1 Scan Controllers

```
src.odrive_app.controllers.LocalScanController  -- scans local filesystem
src.odrive_app.controllers.RemoteScanController -- polls remote cloud state
```

### 3.2 Native Directory Traversal

The binary uses POSIX directory APIs directly via glibc:
- `opendir`, `readdir`, `closedir` -- standard directory iteration
- `stat`, `__xstat` -- file metadata
- `shutil` -- Python high-level file operations

Additionally, STB library functions for recursive directory traversal:
- `stb_readdir_files` / `stb_readdir_files_mask` -- list files with optional glob
- `stb_readdir_subdirs` / `stb_readdir_subdirs_mask` -- list subdirectories
- `stb_readdir_recursive` / `stb_readdir_rec` -- recursive traversal

### 3.3 Scanning Strategy: Full vs Incremental

The refresh job internals reveal the scanning is **incremental with comparison**:

```
refresh_job.internals.Compare       -- compare local vs remote state
refresh_job.internals.MergeFiles    -- merge detected changes
refresh_job.internals.Operations    -- enumerate required operations
refresh_job.internals.Refresh       -- execute refresh
refresh_job.internals.RefreshChildren -- recurse into children
refresh_job.internals.GroupByPosition -- group changes by tree position
refresh_job.internals.LocalAddFolder  -- handle new local folders
refresh_job.internals.RemoteAddFolder -- handle new remote folders
refresh_job.internals.FileFormats     -- handle format conversions
```

The `Compare` + `MergeFiles` + `GroupByPosition` pattern indicates the agent:
1. Enumerates current local state via directory scan
2. Compares against the tracked state in SyncTrackingDB
3. Groups differences by position in the tree
4. Merges files that match between local and remote
5. Generates Operations (sync actions) from the diff
6. Recurses into children for incremental sub-tree refresh

---

## 4. Sync Tracking Database

### 4.1 Schema

The sync state is persisted in SQLite via a layered DB architecture:

```
src.utility.db.DBConnectionManager      -- connection pool
src.utility.db.DBConnection             -- base connection wrapper
src.sync9.sync_tracking_service.internals.db.SyncTrackingDBConnection
src.sync9.sync_tracking_service.internals.db.SyncTrackingDBService
src.sync9.sync_tracking_service.internals.db.tables.SyncTrackingTable
```

The tracking data is modeled as a tree of `SyncTrackingNode` objects with
`SyncTrackingValuesV0` value objects. The "V0" suffix indicates versioned
serialization for forward compatibility.

### 4.2 Property Database (Config Persistence)

A separate property database stores configuration:

```
src.odrive_app.services.db.PropertyDBConnection
src.odrive_app.services.db.PropertyDBService
src.odrive_app.services.db.tables.PropertyTable
```

### 4.3 Full Table Inventory

| Table Class | Purpose |
|---|---|
| `SyncTrackingTable` | Per-file sync state (the main tracking table) |
| `PropertyTable` | Key-value config/settings persistence |
| `StickySyncTable` | Pinned/always-synced paths |
| `SyncModeTable` | Per-folder sync mode (auto-download threshold, etc.) |
| `ProSyncFolderTable` | Premium "Pro Sync" folder designations |
| `BackupJobTable` | Scheduled backup job definitions |
| `EncryptionEntryTable` | Encryption key/config entries |
| `IntegrationCacheTable` | Provider-specific cached metadata |

---

## 5. Policy Engine

### 5.1 Components

```
src.odrive_app.controllers.PolicyController  -- evaluates and applies policies
src.odrive_app.services.PolicyService        -- policy storage and retrieval
```

### 5.2 Threshold System

```
src.odrive_app.controllers.PlaceholderThresholdController
src.sync9.SyncThresholdInfinite
```

The `PlaceholderThresholdController` manages file size thresholds that determine
whether a file is automatically downloaded or left as a placeholder (.cloudf stub).
`SyncThresholdInfinite` is a sentinel value meaning "never auto-download" (always
leave as placeholder).

The threshold system likely evaluates:
- File size against a configurable limit
- Available disk space
- Per-folder override settings stored in `SyncModeTable`

### 5.3 Policy Evaluation Flow

Based on the controller/service split:
1. `PolicyService` loads rules from the property DB
2. `PolicyController` evaluates rules against file attributes
3. `PlaceholderThresholdController` applies size-based decisions
4. Results feed into `SyncController` to determine sync/unsync actions

---

## 6. Auto-Unsync Algorithm

### 6.1 Controller

```
src.odrive_app.controllers.AutoUnsyncController
```

### 6.2 Behavioral Model

Auto-unsync is a premium feature that reclaims local disk space by converting
previously-synced files back into placeholders. The algorithm likely:

1. Monitors disk usage or file access patterns
2. Identifies files that haven't been accessed recently
3. Checks against `StickySyncTable` (pinned files are exempt)
4. Checks against `SyncModeTable` (folder-level overrides)
5. Converts eligible files to placeholders (dehydrates)
6. Updates `SyncTrackingTable` to reflect the dehydrated state

### 6.3 Triggers

Auto-unsync is likely triggered by:
- Disk space pressure (threshold-based)
- Time-based expiry (files not accessed within N days)
- Manual policy configuration (folder-level rules)
- HeartbeatController periodic checks

---

## 7. Sticky Sync / Pin Behavior

### 7.1 Database Table

```
src.odrive_app.services.db.tables.StickySyncTable
```

### 7.2 Semantics

"Sticky sync" (also called "pin") ensures files remain hydrated (fully downloaded)
and are never auto-unsynced. The string `Pin:` was found in the binary, confirming
the pin terminology.

The `StickySyncTable` likely stores:
- Path or node ID of pinned items
- Whether the pin is recursive (pin entire folder tree)
- Timestamp of pin creation
- Source of pin (user action vs policy)

### 7.3 Interaction with SyncMode

```
src.odrive_app.services.db.tables.SyncModeTable
```

The `SyncModeTable` stores per-folder sync mode settings, which interact with
sticky sync to determine the effective behavior:
- "Always sync" mode = implicit recursive pin
- "Never sync" mode = files remain as placeholders
- Default mode = threshold-based auto-download with auto-unsync eligible

---

## 8. Backoff and Rate Limiting

### 8.1 Per-Provider Backoff

Each major cloud provider has a dedicated `BackoffChecker`:

| Provider | Class |
|---|---|
| Generic cloud | `src.integrations.clouddrive.BackoffChecker` |
| Dropbox | `src.integrations.dropbox.BackoffChecker` |
| Google Drive | `src.integrations.googledrive.GoogleDriveBackoffChecker` |
| OneDrive | `src.integrations.onedrive.BackoffChecker` |
| Procore | `src.integrations.procore.BackoffChecker` |
| Slack | `src.integrations.slack.SlackBackoffChecker` |

### 8.2 Strategy

The per-provider backoff checkers likely implement exponential backoff with
provider-specific tuning. Google Drive has a custom `GoogleDriveBackoffChecker`
(named differently from the others), suggesting it has unique rate-limiting
behavior (Google's 403 userRateLimitExceeded requires specific handling).

No explicit debounce/coalesce/jitter strings were found, suggesting:
- Event coalescing may happen implicitly in the refresh job's Compare/Merge phase
- Backoff is the primary rate control mechanism
- The scan-based (polling) architecture on Linux inherently debounces by scan interval

---

## 9. macOS Finder Integration

### 9.1 Badging

```
src.odrive_app.controllers.utility.Badging
src.common.services.badge.BadgeRefreshService
```

The badge system provides visual overlay icons on files in Finder showing sync
status (synced, syncing, error, placeholder). `BadgeRefreshService` handles
updating badge state when file sync status changes.

### 9.2 OS Shell Integration

```
src.common.services.shell.OSShellService
src.odrive_app.services.MacPackageService
src.utility.mac_os_util.MacOSUtil
plistlib(
```

- `OSShellService` -- interacts with the OS shell (Finder extension, context menus)
- `MacPackageService` -- handles macOS .pkg installation/updates
- `MacOSUtil` -- macOS-specific utilities (probably FSEvents, Launch Agent management)
- `plistlib` -- reads/writes macOS property list files (LaunchAgent plists, preferences)

### 9.3 Windows Integration (for comparison)

```
src.utility.win32con
src.utility.win32util
src.utility.win_util.WinUtil
```

Windows has parallel utilities for shell extension integration (overlay icons,
context menus via COM shell namespace extensions).

---

## 10. Internal File Patterns

### 10.1 Placeholder Files

```
src.odrive_app.services.PlaceholderService
src.odrive_app.controllers.utility.RemoteCloudFileFormat
```

odrive uses `.cloudf` and `.cloud` extensions for placeholder files:
- `.cloudf` -- placeholder for a remote file (contains metadata, no content)
- `.cloud` -- placeholder for a remote folder (expandable on access)

### 10.2 Integration-Specific Patterns

The binary references these internal/integration modules:
- `src.integrations.odrive` -- odrive's own cloud storage
- `src.integrations.odrivegateway` -- odrive gateway (aggregation layer)
- `src.integrations.encryption` -- client-side encryption layer

### 10.3 Temp File Handling

```
mkdtemp, mkstemp   -- POSIX temp file creation
tempfile(           -- Python tempfile module
pyi_create_temp_path, pyi_remove_temp_path -- PyInstaller temp management
```

The agent creates temp files for:
- In-progress downloads (atomically renamed on completion)
- PyInstaller runtime extraction
- Diagnostic dumps

---

## 11. Credential and Session Management

### 11.1 Keyring Abstraction

```
src.utility.KeyChainService
keyring.core
keyring.backends.Gnome       -- GNOME Keyring (Linux)
keyring.backends.SecretService -- D-Bus Secret Service (Linux)
keyring.backends.kwallet     -- KDE KWallet (Linux)
keyring.backends.OS_X        -- macOS Keychain
keyring.backends.Windows     -- Windows Credential Store
keyring.backends.file        -- File-based fallback
keyring.backends.keyczar     -- Keyczar encryption backend
keyring.backends.multi       -- Multi-backend fallback chain
keyring.backends.pyfs        -- PyFilesystem backend
keyring.backends.Google      -- Google-specific
```

The `KeyChainService` wraps Python's `keyring` library, which provides a
platform-abstracted credential store. On Linux, it chains through:
1. GNOME Keyring (if available)
2. D-Bus SecretService (freedesktop.org standard)
3. KWallet (if KDE)
4. File-based encrypted fallback

### 11.2 Auth Controllers

```
src.odrive_app.controllers.AuthKeyLoginController    -- API key auth
src.odrive_app.controllers.AuthorizedUserController  -- OAuth user auth
```

Two auth methods:
1. **AuthKey login** -- headless/CLI authentication via API key
2. **Authorized user** -- browser-based OAuth flow

### 11.3 Session Management

```
requests.sessions   -- HTTP session pooling
requests.cookies    -- cookie persistence
requests.auth       -- HTTP auth handlers
cookielib           -- cookie jar management
```

HTTP sessions are managed via the `requests` library with cookie persistence
across requests. Each cloud provider integration likely maintains its own
session with provider-specific auth headers.

### 11.4 SFTP/SSH Auth

```
paramiko.auth_handler   -- SSH authentication
paramiko.rsakey         -- RSA key handling
paramiko.dsskey         -- DSA key handling
paramiko.ecdsakey       -- ECDSA key handling
paramiko.agent          -- SSH agent forwarding
paramiko.kex_gss        -- GSS-API (Kerberos) key exchange
```

The SFTP integration uses paramiko with full SSH key type support and
SSH agent forwarding capability.

---

## 12. Configuration System

### 12.1 Property Persistence

```
src.odrive_app.services.db.PropertyDBService
src.odrive_app.services.db.tables.PropertyTable
ConfigParser(
```

Configuration is stored in two ways:
1. **PropertyTable** in SQLite -- structured key-value store for runtime config
2. **ConfigParser** -- INI-file parsing (likely for initial/default config)

### 12.2 Settings Controllers

```
src.odrive_app.controllers.AdvancedSettingsController
src.odrive_app.controllers.utility.EncryptorConfiguration
```

Advanced settings are exposed through a dedicated controller, suggesting a
settings UI or CLI interface for modifying runtime behavior.

### 12.3 Registry / Startup

```
src.common.startup.RegistryFileService
```

On Windows, this manages Windows Registry entries for auto-start. On Linux,
the equivalent would be XDG autostart desktop files or systemd user services.

---

## 13. Sync Engine (Sync9) Architecture

### 13.1 Core Components

```
src.sync9.Sync9Service              -- main sync engine service
src.sync9.Sync9Adapter              -- base adapter interface
src.sync9.Exceptions                -- sync-specific exceptions
src.sync9.SyncThresholdInfinite     -- infinite threshold sentinel
```

### 13.2 Adapter Pattern

Every cloud provider implements `Sync9Adapter`:

```
Sync9Adapter (base)
  +-- AutodeskSync9Adapter
  +-- B2Sync9Adapter
  +-- BoxSync9Adapter
  +-- CloudDriveSync9Adapter
  +-- DropboxSync9Adapter
  +-- DropboxTeamsSync9Adapter
  +-- EncryptionSync9Adapter    <-- encryption as a "provider"
  +-- FacebookSync9Adapter
  +-- FTPSync9Adapter
  +-- GoogleCloudSync9Adapter
  +-- GoogleCloudxSync9Adapter
  +-- GoogleDriveSync9Adapter
  +-- InstagramSync9Adapter
  +-- odriveSync9Adapter
  +-- OdriveGatewaySync9Adapter
  +-- OneDriveSync9Adapter
  +-- OneDriveForBusinessSync9Adapter
  +-- SharepointSync9Adapter
  +-- ProcoreSync9Adapter
  +-- S3Sync9Adapter
  +-- S3xSync9Adapter
  +-- S3Sync9Adapter (s3compat)
  +-- S3xSync9Adapter (s3compat)
  +-- SFTPSync9Adapter
  +-- SlackSync9Adapter
  +-- WebDAVSync9Adapter
```

**Key insight**: Encryption is implemented as a Sync9Adapter, meaning it wraps
another adapter transparently. This is the "Encryptor" feature -- client-side
encryption that can be layered on top of any storage provider.

### 13.3 Sync Operations

```
sync.internals.Sync          -- execute file sync (hydrate)
sync.internals.Unsync        -- execute file unsync (dehydrate)
sync.internals.Expand        -- expand a .cloud folder placeholder
sync.internals.QueuedExpand  -- batch/queued folder expansion
sync.internals.AddCloudChildren -- populate cloud folder contents
```

### 13.4 File Sync State

```
src.sync_api_common.FileSyncState  -- per-file sync state enum
src.sync_api_common.O2Path         -- path abstraction ("O2" = odrive 2?)
src.sync_api_common.StopStatus     -- stop/cancel state
```

`FileSyncState` likely has values like: Synced, Unsynced, Syncing, Error,
Placeholder, Conflict.

---

## 14. Heartbeat and Health

```
src.odrive_app.controllers.HeartbeatController
src.odrive_app.controllers.SystemStatusController
```

The HeartbeatController periodically:
- Checks connectivity to odrive cloud services
- Reports agent health status
- May trigger periodic scans or policy evaluations
- Sends telemetry via Mixpanel

---

## 15. Analytics and Telemetry

```
src.common.mixpanel.MixpanelUtil
src.common.services.TrackingFileService
```

The agent reports usage analytics to Mixpanel. `TrackingFileService` likely
manages a local queue of tracking events that are batched and sent on heartbeat.

---

## 16. Implications for tummycrypt

### What to replicate
1. **Scan-based sync on Linux** -- odrive does NOT use inotify on Linux; it polls.
   tummycrypt's inotify-based approach (via FUSE) is actually more responsive.
2. **Separate local/remote event controllers** -- clean separation worth adopting.
3. **SyncTrackingDB with versioned values** -- forward-compatible state persistence.
4. **Per-provider backoff** -- each cloud backend needs its own rate limiter.
5. **Encryption-as-adapter** -- layering encryption as a transparent adapter is elegant.
6. **Threshold-based placeholder decisions** -- configurable file size thresholds.
7. **PropertyDB for config** -- SQLite key-value store for runtime configuration.

### What to improve on
1. **No inotify** -- tummycrypt's FUSE layer gives us kernel-level event notification
   that odrive lacks on Linux. This is a significant advantage.
2. **Python 2.7** -- odrive's agent is ancient. No async/await, no type hints.
3. **Monolithic binary** -- PyInstaller bundle is ~11MB. Rust binary can be smaller
   and faster.
4. **Per-provider adapters are tightly coupled** -- 25+ adapters in one binary.
   tummycrypt's NATS-based architecture decouples providers.
5. **No conflict resolution visible** -- The refresh_job Compare/Merge likely
   handles conflicts, but no explicit ConflictResolver class was found.
6. **Mixpanel telemetry** -- privacy concern. tummycrypt should use opt-in telemetry.

### Key design patterns to study further
- The refresh_job pipeline (Compare -> MergeFiles -> GroupByPosition -> Operations)
  is the core sync reconciliation algorithm. Deeper analysis of this pipeline
  would reveal how odrive handles the "three-way merge" problem.
- The `QueuedExpand` pattern suggests folder expansion is asynchronous and
  batched -- important for large directory trees.
- `LockedItem` in the utility module suggests file locking semantics for
  concurrent access protection.
- `SyncAdapterFactory` + `SyncAdapters` suggests runtime adapter selection
  based on provider type.
