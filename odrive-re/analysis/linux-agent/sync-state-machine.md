# odrive Sync State Machine - Deep Analysis

Reverse-engineered from the Linux agent binary (`odriveagent`, ELF 64-bit, PyInstaller 2.1+ / Python 2.7). Build origin: `/home/ubuntu/hudson/workspace/Odrive_Lnx_Agent_64/` (Jenkins CI).

All class names, method names, SQL schemas, and string constants in this document are extracted directly from the binary's bytecode constants via PYZ archive decompression.

---

## 1. Architecture Overview

The sync engine is called **Sync9** (internal version name). It follows a layered architecture:

```
ProtocolServer / EventServer
        |
ProtocolRequestDispatcher / EventDispatcher
        |
Controllers (sync, refresh, unsync, auto-unsync, ...)
        |
SyncTrackingService (in-memory + SQLite tree)
        |
Sync9Adapter (abstract) -> per-provider adapters
        |
IntegrationService (provider-specific API calls)
```

### Key Architectural Patterns

- **Singleton services**: `src.utility.Singleton` base class
- **Thread pool execution**: `src.utility.ThreadPoolExecutor` (aliased as `OxygenThreadPoolExecutor`)
- **Stop/pause coordination**: `StopStatus` propagated through all operations
- **Locking**: `LockedItem` prevents concurrent operations on the same path
- **Badging**: `BadgeRefreshService` + `Badging` utility updates Finder/Explorer overlays
- **Diagnostics**: `SyncActivityDump`, `DiagnosticDump`, `ExceptionDump`, `ThreadsDump` for debugging

---

## 2. FileSyncState Enum

The `FileSyncState` class (module `src.sync_api_common.FileSyncState`) inherits from `EnumDeprecated` and defines exactly four states:

| State | Meaning |
|-------|---------|
| `NOT_SYNCED` | File exists only as a placeholder (`.cloud`/`.cloudf`) |
| `SYNCED` | File content matches between local and remote |
| `ACTIVE` | File is currently being synced (download/upload in progress) |
| `LOCKED` | File is locked by another operation and cannot be modified |

These states are per-node, not global. The `AgentSyncStateController` manages transitions.

---

## 3. Sync9Codes (Operation Status Codes)

Defined in `src.sync9.Exceptions.Sync9Codes`:

| Code | Name | Description |
|------|------|-------------|
| SS100 | `DELAYED_DELETE` | Item marked for deferred deletion |
| SS101 | `MERGE_CONFLICT` | Local and remote changes conflict |
| SS102 | `IN_PROGRESS` | Operation still running |
| SS103 | `BLACKLISTED` | Item matches blacklist pattern |
| SS104 | `BUSY` | Node is locked by another operation |
| SS105 | `ORPHANED_CLOUD_FILE` | Placeholder with no matching remote |
| SS106 | `SYNC_EXCEPTION` | General sync failure |
| SS107 | `ILLEGAL_FILE_NAME` | Name contains invalid characters |

---

## 4. Placeholder Lifecycle

### 4.1 Extension Types

The `PlaceholderService` manages nine distinct file extension types:

| Extension Type | Purpose |
|----------------|---------|
| `cloudFileExtension` | File placeholder (`.cloud`) |
| `cloudFolderExtension` | Folder placeholder (`.cloudf`) |
| `encryptedNameExtension` | Encrypted name marker |
| `encryptedNameCloudFileExtension` | Encrypted + file placeholder |
| `encryptedNameCloudFolderExtension` | Encrypted + folder placeholder |
| `decryptedNameExtension` | Decrypted name marker |
| `decryptedNameCloudFileExtension` | Decrypted + file placeholder |
| `decryptedNameCloudFolderExtension` | Decrypted + folder placeholder |
| `xlExtension` | XL file segment (`.cloudx`) |

Legacy extensions `.cloudfx` are handled by `purge_cloudx_and_cloudfx` and `exists_cloudx_or_cloudfx` in `SyncTrackingTable` for migration.

### 4.2 Placeholder -> Real File (Expand/Sync)

```
filename.txt.cloud  (NOT_SYNCED, placeholder on disk)
    |
    v  [user double-clicks or CLI sync command]
filename.txt.cloud  (ACTIVE, download begins)
    |
    v  [apply_local_expand_file completes]
filename.txt        (SYNCED, real content on disk)
    |
    v  [tracking values updated in SyncTrackingTable]
```

### 4.3 Real File -> Placeholder (Unsync)

```
filename.txt        (SYNCED, real content on disk)
    |
    v  [user right-clicks "unsync" or auto-unsync triggers]
    |  [_unsync_item checks: not blacklisted, not active, in mount]
    |  [_enforce_unsync_policy checks premium/policy]
    |  [_first_item_that_cannot_unsync checks dirty children]
    v
filename.txt.cloud  (NOT_SYNCED, content removed, placeholder created)
    |
    v  [delete_tracking_values called]
    v  [badge updated via update_badges_on_path]
```

### 4.4 Folder Expand Flow

```
folder_name.cloudf  (NOT_SYNCED folder placeholder)
    |
    v  [Expand._expand_folder]
    |  1. remote_list_folder() -> get children from cloud
    |  2. For each remote child:
    |     - Create local placeholder (child.cloud or child.cloudf)
    |     - add_tracking_values() in SyncTrackingTable
    |     - add_folder_to_light_space() for folder children
    |  3. Enqueue cloud file downloads to cloudFilesQueue
    v
folder_name/        (real directory, children are placeholders)
    |
    v  [QueuedExpand processes the file queue]
    |  - apply_local_expand_files_from_queue() with thread pool
    |  - InProgressFiles tracks concurrent download limits
    |  - Throughput monitoring with minThroughput threshold
    |  - Rate limiting with secondsToDelayForRateLimiting
    v
folder_name/        (children progressively become real files)
```

---

## 5. SyncTrackingNode Tree Structure

### 5.1 Node Class (`SyncTrackingNode`)

Each tracked item is a `SyncTrackingNode` with these fields:

```python
SyncTrackingNode:
    oid         : str (UUID)     # Primary key, globally unique
    localValues : SyncTrackingValuesV0  # Local file state
    remoteValues: SyncTrackingValuesV0  # Remote file state
    parentOid   : str (UUID)     # Parent node's oid (tree structure)
    timestamp   : int            # NodeTimestamp (epoch seconds)
```

### 5.2 Values Class (`SyncTrackingValuesV0`)

Each side (local/remote) stores:

```python
SyncTrackingValuesV0:
    fileAttributes: FileAttributes
    parentId      : unicode       # Provider-specific parent ID

FileAttributes:
    uri        : unicode  # Full path/URI
    id         : unicode  # Provider-specific file ID
    name       : unicode  # File/folder name
    isFolder   : bool     # True for directories
    size       : int      # File size in bytes
    modTime    : int      # Modification time (epoch)
    contentTag : unicode  # Content hash/ETag (schema v2+)
```

### 5.3 Named Tuple for API Return

The `SyncTrackingService` returns tracking data as:

```python
SyncTrackingTuple = namedtuple('SyncTrackingTuple', ['localAttr', 'remoteAttr', 'oid', 'parentOid'])
```

### 5.4 Tree Operations

Key methods on `SyncTrackingService`:

| Method | Purpose |
|--------|---------|
| `add_root_node` | Create the root of a sync tree |
| `add_tracking_values` | Add a child node by local URI |
| `add_tracking_values_by_parent_path` | Add a child by parent path |
| `update_tracking_values` | Update local/remote attrs by local URI |
| `update_tracking_values_by_oid` | Update by OID |
| `update_tracking_values_removing_descendants` | Update + delete subtree |
| `update_local_uri` | Rename/move (updates URI path) |
| `delete_tracking_values` | Remove a node |
| `delete_tracking_values_by_oid` | Remove by OID |
| `delete_children_tracking_values` | Remove all children |
| `delete_children_tracking_values_by_oid` | Remove children by parent OID |
| `get_tracking_values` | Look up by O2Path |
| `get_tracking_values_by_oid` | Look up by OID |
| `iterate_children_tracking_values` | Iterate children of a node |
| `iterate_tracking_values_by_remote_uri` | Find by remote URI |
| `iterate_tracking_values_by_local_sync_id` | Find by local provider ID |
| `iterate_tracking_values_by_remote_sync_id` | Find by remote provider ID |
| `iterate_tracking_values_with_timestamp_older_than` | For auto-unsync aging |
| `purge_cloudx_and_cloudfx` | Clean up deprecated extensions |

### 5.5 Conflict Detection

When adding or updating nodes, the service checks for duplicates:
- `_find_node_by_local_uri`: Detects URI collisions
- `_find_node_by_parent_oid_and_local_name`: Detects name collisions within parent

Warning logged: "Found node with same local uri={}: node={} while adding/updating..."
Warning logged: "Found node with same parentOid and local name={}: node={} while adding/updating..."

If more than one node matches: "More than one node for local uri={}: node={}" -- triggers `send_custom_alert_message`.

---

## 6. SyncTracking Database Schema

The complete schema is persisted in SQLite via APSW (Another Python SQLite Wrapper).

### 6.1 Table: SyncTracking

19 columns total (15 original + 4 added via ALTER TABLE):

| Column | Type | Notes |
|--------|------|-------|
| `oid` | TEXT PRIMARY KEY UNIQUE NOT NULL | UUID v4 |
| `LocalUri` | TEXT | Full local path |
| `LocalId` | TEXT | Provider-specific local ID |
| `LocalName` | TEXT | Filename component |
| `LocalIsFolder` | INTEGER | Boolean |
| `LocalModTime` | INTEGER | Unix epoch |
| `LocalSize` | INTEGER | Bytes |
| `LocalParentId` | TEXT | Provider parent ID |
| `RemoteUri` | TEXT | Full remote URI |
| `RemoteId` | TEXT | Provider-specific remote ID |
| `RemoteName` | TEXT | Remote filename |
| `RemoteIsFolder` | INTEGER | Boolean |
| `RemoteModTime` | INTEGER | Unix epoch |
| `RemoteSize` | INTEGER | Bytes |
| `RemoteParentId` | TEXT | Provider parent ID |
| `ParentOid` | TEXT | (ALTER) Parent node OID |
| `LocalContentTag` | TEXT | (ALTER) Local content hash |
| `RemoteContentTag` | TEXT | (ALTER) Remote content ETag |
| `NodeTimestamp` | INTEGER | (ALTER) For aging/auto-unsync |

### 6.2 Indexes (8 total)

1. `SyncTrackingOidIndex` on `oid`
2. `SyncTrackingParentOidIndex` on `ParentOid`
3. `SyncTrackingNodeLocalIdIndex` on `LocalId`
4. `SyncTrackingNodeRemoteIdIndex` on `RemoteId`
5. `SyncTrackingNodeParentOidAndLocalNameIndex` on `(ParentOid, LocalName)`
6. `SyncTrackingNodeLocalUriIndex` on `LocalUri`
7. `SyncTrackingNodeRemoteUriIndex` on `RemoteUri`
8. `SyncTrackingNodeTimestamp` on `NodeTimestamp`

### 6.3 Schema Evolution

The code uses `_has_column()` with `PRAGMA table_info()` to check for columns and only runs ALTER TABLE if needed. This supports upgrading from older versions without data loss.

---

## 7. Expand Operation Flow (Detailed)

### 7.1 Entry Point: `SyncController.sync()` or `Expand.expand()`

Both converge on the same internal flow. `Sync` adds recursive folder traversal.

### 7.2 File Expand (`_expand_file`)

```
1. Check path validity:
   - has_cloud_file_extension() or has_decrypted_name_cloud_file_extension()
   - path_is_inside_mounts()
2. Get tracking values for the cloud file path
3. Set node to ACTIVE state (set_active)
4. Update badges on path
5. Check for XL file format (has_xl_file_extension -> apply_local_expand_xl_file)
6. Get compare time and remote attributes
7. Call apply_local_expand_file():
   - Download content from remote via adapter
   - Track transfer size: add_transfers_and_size_to_total_count
   - Write to local filesystem
   - Rename: remove .cloud extension
8. Update tracking values with new local attributes
9. Set item in scope, remove ACTIVE state
10. Open file if requested (openFile flag)
11. On error:
    - Handle EncryptorPasswordNotFoundException -> prompt dialog
    - Handle LoginRequestException -> sign-in dialog
    - Handle CorruptXLFileException -> remove cloud extension, add refresh job
    - General exception -> get_waiting_delay, send alert
```

### 7.3 Folder Expand (`_expand_folder`)

```
1. Validate path (has_cloud_folder_extension, path_is_inside_mounts)
2. Get expand folder configuration:
   - resolve_sticky_sync_settings -> StickySyncTable lookup
   - downloadThreshold, expandSubfolders flags
3. Remove cloud folder extension from name
4. Create local directory
5. Start two parallel threads via OxygenThreadPoolExecutor:
   Thread 1: _expand_folder_in_thread
     - remote_list_folder() with pagination (pageToken)
     - For each page of remote children:
       - Classify: cloud files vs cloud folders
       - Create local placeholder for each child
       - add_tracking_values() for each
       - Enqueue files to cloudFilesQueue (Queue)
       - Recursively expand sub-folders if expandSubfolders=True
     - Put QUEUE_END_MARKER when done
   Thread 2: _expand_files_in_thread
     - Pull items from cloudFilesQueue
     - apply_local_expand_file for each
     - Stop on QUEUE_END_MARKER
6. Wait for both futures (interruptible_future_result)
7. Handle first-time setup (firstTimeSetup -> encryptor password dialogs)
8. Apply mac package mode if needed (is_mac_package_type)
9. Update badges, change shell directory
```

### 7.4 Queued Expand (`QueuedExpand`)

The `QueuedExpand` module manages bulk file downloads with:

#### InProgressFiles (concurrency tracker)
```python
InProgressFiles:
    files    : int   # Current count of in-flight downloads
    bytes    : int   # Current bytes in-flight
    maxFiles : int   # Limit from get_max_downloads()
    maxBytes : int   # maxDownloadingBytes config value
    _lock    : RLock # Thread safety
```

Methods: `add()`, `remove()`, `exceeds_limits()`

#### QueueWithRetries (retry queue)
```python
QueueWithRetries:
    _queue              : Queue
    _queueEndedMarker   : sentinel object
    _queueEnded         : bool
    _itemsToRetryFirst  : deque  # High-priority retries
    _itemsToRetryLast   : deque  # Low-priority retries
```

Methods: `get()` (with timeout), `put_front()`, `put_back()`, `is_ended()`

#### Download Configuration
```
maxDownloadingBytes                              # Total bytes concurrently downloading
minThroughput                                    # Minimum acceptable throughput
minFileSizeForThroughputCalculation              # Skip throughput check for small files
maxRetriesOnSystemExceptionBeforeGivingUp        # Retry limit for system errors
maxRetriesOnNonSystemExceptionBeforeGivingUp     # Retry limit for app errors
secondsToDelayForRateLimiting                    # Back-off for rate limits
QUEUE_ITEM_WAIT                                  # Timeout for queue.get()
MAX_ITERATION                                    # Max iterations for recursive sync
```

#### Thread Pool Behavior
```
apply_local_expand_files_from_queue():
  1. Create ExpandFolderThreadpoolExecutor with max_workers from get_max_downloads()
  2. Process items in batches:
     a. Submit download tasks to thread pool
     b. Wait for FIRST_COMPLETED
     c. Check results:
        - ConcurrentLimitException -> reduce max concurrent
        - RetryApplyException -> re-queue with put_front/put_back
        - Bad throughput -> reduce batch size
        - Rate limiting -> delay secondsToDelayForRateLimiting
     d. Track duration and throughput per file
  3. Log: "submit {}", "downloaded file={}, size={}, duration={}, thruput={}"
```

---

## 8. Unsync Operation Flow (Detailed)

### 8.1 Entry Point: `Unsync.unsync()`

```
For each o2Path in o2Paths:
  1. _unsync_item(o2Path, force=False)
  2. On success: log "{} successfully unsynced via user action. {}"
  3. On ServiceStopped: log "unable to be successfully unsynced. Service stopped."
  4. On error: log "unable to be successfully unsynced via user action due to {}"
```

### 8.2 Core: `_unsync_item(o2Path, force)`

```
1. PRE-CHECKS:
   - Not a cloud file already (has_any_cloud_extension)
   - Path is inside defined mounts (path_is_inside_mounts)
   - Not found check (local_find_attributes)
   - Not actively syncing (is_active_path)
   - Folder must be in light space (is_folder_in_light_space)

2. DIRTY CHECK:
   - _first_item_that_cannot_unsync:
     - Iterates children tracking values
     - Compares local vs synced attributes (is_content_change)
     - Checks for dirty (modified) or read-only children
   - If dirty children found and not forced:
     - render_unsync_dirty_dialog -> ask user
     - "There are items that still need to be uploaded. Ex: {}"
   - If force: "User has chosen to force unsync on {}"

3. POLICY CHECK:
   - _enforce_unsync_policy:
     - get_unsync_policy (refresh_policy if needed)
     - Check premium subscription (is_agent)
     - render_premium_required_dialog if not authorized

4. EXECUTE UNSYNC:
   - Set ACTIVE state
   - Get tracking values (localAttr, remoteAttr)
   - Compute placeholder name: get_placeholder_name_for_local()
   - For files:
     - local_delete_file (or local_move_to_os_trash)
     - local_add_empty_file(placeholderName)
     - delete_tracking_values
     - remove_descendant_encryption_entries
   - For folders:
     - Recursively unsync children
     - Move blacklisted items to OS trash
     - remove_folder_from_light_space
     - local_delete_folder or local_move_to_os_trash
     - Folder replaced with .cloudf placeholder
   - Remove ACTIVE, set_recently_synced, set_item_in_scope
   - Update badges
```

### 8.3 Trash Behavior During Unsync

When a folder contains items that cannot be unsynced cleanly:
- "Moved to the OS trash due to unsynced items in the folder."
- "Moved to the OS trash due to forced unsync on the folder."
- "Moved to the OS trash due to unsynced blacklisted items in the folder."

The `_sendToOsTrash` flag controls whether items go to system trash or are hard-deleted.

---

## 9. Auto-Unsync Controller

The `AutoUnsyncController` runs as a background thread that periodically reclaims disk space.

### 9.1 Configuration

Retrieved from `PolicyService`:
- `get_auto_unsync_threshold`: Time-based threshold (seconds since last access/modify)
- `get_auto_unsync_interval`: How often to run the sweep
- `get_auto_unsync_use_access`: Whether to use access time (vs modify time)
- `get_auto_unsync_policy`: Feature availability check

### 9.2 Sweep Algorithm

```
_run_auto_unsync():
  while started:
    1. Check _has_auto_unsync_policy
    2. Get threshold from get_auto_unsync_threshold
    3. Iterate nodes older than threshold:
       iterate_tracking_values_with_timestamp_older_than(now - threshold)
    4. For each candidate node:
       - Get O2Path from local URI
       - Skip if is_active_path or has_active_item_in_path
       - Skip if isFolder (only unsync files)
       - Skip if already has_any_cloud_extension
       - Get local file attributes (check access time if configured)
       - _auto_unsync_file:
         - Compute placeholder name
         - local_delete_file
         - local_add_empty_file(placeholder)
         - delete_tracking_values
         - remove_descendant_encryption_entries
       - Log: "{} auto-unsynced successfully. Last Modified {}. Last Accessed {}. Created Locally {}"
       - Accumulate totalSizeAutoUnsynced, numFilesAutoUnsynced
    5. _deliver_auto_unsync_user_notification (OS notification with space saved)
    6. interruptible_sleep(auto_unsync_interval)
```

### 9.3 User Notification

```
title: get_string("auto_unsync_user_notification_title")
message: get_string("auto_unsync_user_notification_message") or plural variant
  - includes numFiles and humanize_bytes(spaceSaved)
deliver_user_notification(title, message)
```

---

## 10. Sticky Sync (Persistent Sync Settings)

### 10.1 Purpose

When a user syncs a folder with specific settings (download threshold, expand subfolders), those settings are persisted in `StickySyncTable` so the folder maintains those settings across restarts.

### 10.2 Schema

```
Key:   path string (local folder path)
Value: JSON string containing {downloadThreshold, expandSubfolders}
```

### 10.3 Usage in Expand

During `_expand_folder`, the code calls `resolve_sticky_sync_settings` which:
1. Looks up the path in `StickySyncTable`
2. Returns `(downloadThreshold, expandSubfolders)` tuple
3. Falls back to defaults if no sticky setting exists

---

## 11. SyncMode Table

### 11.1 Purpose

Maps local URI paths to sync mode configurations. The value is a JSON string.

### 11.2 Schema

```
Key:   local URI (path)
Value: JSON sync mode configuration
```

This likely controls per-folder behavior like "always sync", "on-demand", or "streaming" modes (referenced by `StreamingController`).

---

## 12. Error Handling and Retry Patterns

### 12.1 Exception Hierarchy

```
Exception
  +-- Sync9Exception
  |     +-- SyncAdapterRequestException
  |     +-- ItemNotFoundException
  |     +-- AdapterOutOfBoundsMoveException
  |     +-- LockException
  |     +-- TrackingValueUpdateException
  |     +-- ParentNotSyncedException
  |     +-- LoginRequestException
  |     |     +-- LinkLoginRequestException
  |     +-- RefreshTokenRequestException
  |     +-- ConnectionInfoEmptyException
  |     +-- EncryptorPasswordNotFoundException
  |     +-- ConflictingMountException
  +-- RequestException (from exception_handling.Exceptions)
  |     +-- SystemException
  |     +-- NetworkException
  |     +-- APIRateLimitingException
  |     +-- AbstractException
  |     +-- ConcurrentLimitException
  +-- LocalAccessException
  +-- LockItemException
  +-- ServiceStoppedException
  +-- CorruptXLFileException
```

### 12.2 Backoff/Retry Pattern

Multiple integrations implement `BackoffChecker`:
- `src.integrations.clouddrive.BackoffChecker`
- `src.integrations.dropbox.BackoffChecker`
- `src.integrations.googledrive.GoogleDriveBackoffChecker`
- `src.integrations.onedrive.BackoffChecker`
- `src.integrations.procore.BackoffChecker`
- `src.integrations.slack.SlackBackoffChecker`

The `QueuedExpand` retry pattern:
```
1. On SystemException during remote_list_folder:
   - Retry up to maxRetriesOnSystemExceptionOnRemoteListFolder
   - Delay: secondsToDelayForSystemExceptionOnRemoteListFolder seconds
2. On APIRateLimitingException:
   - Delay: secondsToDelayForRateLimiting
3. On ConcurrentLimitException:
   - Reduce max concurrent downloads
4. On general retryable exception:
   - "Recoverable exception {} encountered for {}. Resuming."
   - Re-queue with put_front (high priority) or put_back (low priority)
5. On too many exceptions:
   - "Too many exceptions encountered for recursive sync of {}. Stopping."
6. On unexpected exception:
   - "Unexpected exception {} encountered for recursive sync of {}. Stopping."
```

### 12.3 Error UI Flow

```
render_error_ui(errorCode, context):
  - Maps error code to localized string via OdriveStringTable
  - Displays via UIService (AgentUIService or LogUIService)

render_alert_dialog(primaryCode, secondaryCode):
  - Two-part error message display

render_sign_in_dialog(linkUri):
  - Triggered by LoginRequestException

render_premium_required_dialog(topMessage, bottomMessage):
  - Triggered when unsync/auto-unsync requires paid plan

render_unsync_dirty_dialog(cannotUnsyncO2Path):
  - Shows when unsyncing folder with modified children

render_encryptor_enter_password_dialog(encryptionId, encryptionName):
  - Triggered by EncryptorPasswordNotFoundException

render_encryptor_initialize_password_dialog(encryptionId, encryptionName):
  - First-time encryption setup
```

---

## 13. Refresh Job Pipeline

The refresh system detects changes between local and remote:

### 13.1 Components

| Class | Purpose |
|-------|---------|
| `RefreshJobController` | Orchestrates refresh jobs |
| `Refresh` | Core refresh logic |
| `RefreshChildren` | Iterate and refresh child nodes |
| `Compare` | Compare local vs remote attributes |
| `MergeFiles` | Merge conflicting changes |
| `Operations` | Execute file operations (move, rename, delete, upload, download) |
| `FileFormats` | Handle format-specific logic (XL files, etc.) |
| `GroupByPosition` | Group operations by tree position |
| `LocalAddFolder` | Create local folders from remote |
| `RemoteAddFolder` | Create remote folders from local |

### 13.2 Compare Logic

The `Compare` module (268 extracted strings) performs attribute comparison:
- Checks `is_content_change` between local and tracked remote
- Detects renames (same ID, different name)
- Detects moves (same ID, different parent)
- Handles merge conflicts (`Sync9Codes.MERGE_CONFLICT`)

### 13.3 Operations

The `Operations` module (832 extracted strings -- the largest module) handles:
- File upload and download
- Rename and move (local and remote)
- Delete and trash operations
- Conflict resolution with merge
- XL file segment handling

---

## 14. Encryption Layer

### 14.1 Architecture

```
EncryptionSync9Adapter (wraps another Sync9Adapter)
    |
    v
EncryptionController + EncryptorController
    |
    v
EncryptionEntryTable (maps encrypted <-> decrypted names)
    |
    v
Crypto.Cipher.AES / Blowfish / DES / DES3 / ARC4
    +
Crypto.Hash.SHA256
    +
Crypto.Protocol.KDF
```

### 14.2 Encryption Entry Schema

Per-item encryption metadata stored in `EncryptionEntryTable`:
- `localParentUri`: Parent path context
- `decryptedName`: Human-readable name
- `encryptedName`: Encrypted name on remote storage
- `passphrase`: Encryption passphrase (stored locally!)
- `enabled`: Boolean toggle

### 14.3 Encryption in Placeholder Names

Encrypted items have special placeholder extensions:
- `filename.cloud` -> normal placeholder
- `encrypted_name.cloud` -> encrypted file placeholder
- `decrypted_name.cloud` -> decrypted-name file placeholder
- Same patterns with `.cloudf` for folders

---

## 15. O2Path and Mount System

### 15.1 O2Path

`O2Path` (module `src.sync_api_common.O2Path`) is a datastructure wrapping path information. It's imported from `src.utility.datastructure` and aliased. It encapsulates the relationship between local filesystem paths and remote URIs.

### 15.2 Mount System

The `MountController` manages mount points where cloud storage is mapped to local directories. Key operations:
- `path_is_inside_mounts`: Validates a path belongs to a managed mount
- `path_is_mount_or_inside_mounts`: Includes the mount point itself
- `get_o2_path`: Converts local path to O2Path via mount context

---

## 16. Implications for tummycrypt

### 16.1 Critical Interfaces to Replicate

1. **SyncTrackingTable schema**: This is the exact database schema tummycrypt needs for its index. The 19-column design with dual local/remote attribute sets is the core data model.

2. **FileSyncState enum**: Four-state model (NOT_SYNCED, SYNCED, ACTIVE, LOCKED) is sufficient for a sync state machine.

3. **Placeholder extension system**: `.cloud` and `.cloudf` convention. tummycrypt can use its own extensions but should support the same semantic model.

4. **Auto-unsync with timestamp aging**: The `NodeTimestamp` column + `iterate_tracking_values_with_timestamp_older_than` query is the mechanism for automatic space reclamation.

5. **StickySyncTable**: Per-folder persistent sync settings is a user-facing feature to replicate.

### 16.2 Design Differences for tummycrypt

- odrive stores passphrases in plaintext SQLite -- tummycrypt must never do this
- odrive uses Python 2.7 with GIL -- tummycrypt uses Rust with async
- odrive's backoff is per-integration -- tummycrypt can centralize via NATS
- odrive's SyncTrackingTable uses UUID v4 oids -- tummycrypt uses content-addressed hashing
- odrive's thread pool is bounded by InProgressFiles -- tummycrypt can use tokio semaphores

### 16.3 Features Confirmed Present in odrive

- Recursive folder sync with depth control
- XL file (large file) segmented transfer
- Per-provider backoff/retry with rate limit handling
- Merge conflict detection and resolution
- Blacklist pattern matching for excluded files
- OS trash integration (vs hard delete)
- Premium feature gating (unsync, auto-unsync)
- Mac package detection (is_mac_package_type)
- Badge/overlay integration
- Streaming mode (StreamingController -- details TBD)
