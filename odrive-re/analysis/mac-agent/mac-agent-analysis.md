# macOS Standalone Agent Analysis: odriveSyncAgent-mac-977

## Binary Overview

### odriveagent (sync daemon)

| Property | Value |
|----------|-------|
| File | `odriveagent.app/Contents/MacOS/odriveagent` |
| Size | 8.4 MB |
| Format | Mach-O 64-bit x86_64 executable |
| Packager | PyInstaller (frozen Python 2.7) |
| Code signing | Developer ID: Oxygen Cloud, Inc. (N887K88VYZ) |
| Runtime version | 10.10.0 (hardened) |
| Signed | Feb 6, 2024 |
| Linked libs | Carbon.framework, libSystem, libz, libgcc_s, ApplicationServices |
| App bundle ID | `odriveagent` (minimal identifier) |
| LSBackgroundOnly | true |

### odrive (CLI client)

| Property | Value |
|----------|-------|
| File | `odrive` |
| Size | 5.1 MB |
| Format | Mach-O 64-bit x86_64 executable |
| Packager | PyInstaller (frozen Python 2.7) |
| Code signing | Developer ID: Oxygen Cloud, Inc. (N887K88VYZ) |
| Linked libs | Carbon.framework, libSystem, libz, libgcc_s, ApplicationServices |

Both binaries link the **same libraries** -- no AppKit, Foundation, or Cocoa. This confirms they are purely headless, using only Carbon (required by Python's macOS support) and ApplicationServices.

## Module Map: 100% Parity with Linux Agent

The standalone macOS agent contains **exactly 295 modules**, identical to the Linux agent:

```
Mac modules:   295
Linux modules: 295
Common:        295
Mac-only:      0
Linux-only:    0
```

This confirms the standalone agent is a platform-neutral sync engine. All macOS-specific features (Finder badges, UI, notifications) exist only in the PKG desktop app, not in the standalone agent.

## Architecture Comparison

```
PKG Desktop (full GUI)           Standalone Agent (headless)
========================         ===========================
odrive.app (outer shell)         odriveagent.app
  +-- bin/7666/odriveapp.app       +-- Contents/MacOS/odriveagent
      +-- FinderSyncExtension
      +-- Python.framework       odrive (CLI binary)
      +-- 36 NIB files
      +-- 272 src .so modules    295 src modules (frozen)
      +-- MacOSUIServer          ProtocolServer only
      +-- MacOSExtensionServer
      +-- CFMessagePort IPC
      +-- UserNotifications
      +-- NSStatusItem menu
```

## IPC Protocol

Both the agent and desktop app expose a TCP-based JSON protocol:

### Port Registration
- Agent: `~/.odrive-agent/.oreg`
- Desktop: `~/.odrive/.oreg`

Format of `.oreg`:
```json
{"current": {"protocol": <port_number>}}
```

### Protocol Commands (from odrive.py CLI)

| Command | Class | Description |
|---------|-------|-------------|
| `authenticate` | Authenticate | Provide auth key |
| `deauthorize` | Deauthorize | Remove auth |
| `sync` | Sync / SyncAsynchronous | Sync a placeholder file |
| `stream` | Stream / StreamRemote | Stream file contents |
| `refresh` | Refresh | Refresh folder listing |
| `unsync` | Unsync / ForceUnsync | Convert to placeholder |
| `syncstate` | SyncState | Get sync state of path |
| `status` | Status | Overall status |
| `status --mounts` | MountsStatus | List storage mounts |
| `status --backups` | BackupsStatus | List backup jobs |
| `status --sync_requests` | SyncRequestsStatus | Pending sync requests |
| `status --uploads` | UploadsStatus | Active uploads |
| `status --downloads` | DownloadsStatus | Active downloads |
| `status --background` | BackgroundStatus | Background sync status |
| `status --trash` | TrashStatus | Trash contents |
| `status --waiting` | WaitingStatus | Waiting items |
| `status --not_allowed` | NotAllowedStatus | Blocked items |
| `mount` | Mount | Mount remote path |
| `unmount` | Unmount | Unmount local path |
| `backup` | Backup | Create backup job |
| `removebackup` | RemoveBackup | Remove backup job |
| `backupnow` | BackupNow | Run backup now |
| `xlthreshold` | XLThreshold | Set large file threshold |
| `autounsyncthreshold` | AutoUnsyncThreshold | Set auto-unsync threshold |
| `autotrashshreshold` | AutoTrashThreshold | Set auto-trash threshold |
| `placeholderthreshold` | PlaceholderThreshold | Set placeholder threshold |
| `foldersyncrule` | FolderSyncRule | Per-folder sync rule |
| `encpassphrase` | EncPassphrase | Set encryption passphrase |
| `emptytrash` | EmptyTrash | Empty odrive trash |
| `restoretrash` | RestoreTrash | Restore trashed items |
| `shutdown` | Shutdown | Shutdown agent |
| `sharelink` | ShareLinkSimple | Get share link |
| `diagnostics` | Diagnostics | Upload diagnostics |

### Message Format

Commands are sent as JSON over TCP with newline delimiter:
```json
{"command": "sync", "path": "/path/to/file.cloud"}\n
```

Responses use synchronous request-response with status/message types.

## Info.plist Comparison

### Standalone Agent (`odriveagent.app`)
```xml
CFBundleIdentifier: odriveagent
CFBundleShortVersionString: 0.0.0
CFBundleExecutable: MacOS/odriveagent
CFBundleName: odriveagent
LSBackgroundOnly: 1
```

Minimal plist -- no document types, no extensions, no UI elements.

### PKG Inner App (`odriveapp.app`)
```xml
CFBundleIdentifier: com.oxygen.odriveapp
CFBundleShortVersionString: 1.0
CFBundleExecutable: odrive
LSUIElement: 1
NSHighResolutionCapable: true
NSPrincipalClass: NSApplication
NSMainNibFile: MainMenu
```

Full app with 15 document types, Retina support, and MainMenu NIB.

## Linked Libraries Comparison

| Library | Agent | PKG Launcher | PKG Inner App | FinderSync |
|---------|-------|-------------|---------------|------------|
| Carbon.framework | Yes | -- | -- | -- |
| ApplicationServices | Yes | -- | -- | -- |
| libSystem.B.dylib | Yes | Yes | Yes | Yes |
| libz.1.dylib | Yes | -- | -- | -- |
| libgcc_s.1.dylib | Yes | -- | -- | -- |
| AppKit.framework | -- | -- | Yes | Yes |
| Foundation.framework | -- | -- | Yes | Yes |
| CoreFoundation.framework | -- | -- | Yes | Yes |
| libobjc.A.dylib | -- | -- | Yes | Yes |
| UserNotifications.framework | -- | -- | Yes (weak) | -- |
| Python.framework 2.7 | -- | -- | Yes | -- |
| FinderSync.framework | -- | -- | -- | Yes |
| Swift runtime (15 dylibs) | -- | -- | -- | Yes |

## Key Findings

1. **Identical module map**: The standalone agent and Linux agent are the same codebase, just packaged differently (PyInstaller vs tar.gz of .so files).

2. **No macOS-native features in standalone agent**: Despite running on macOS, the agent uses no AppKit, Foundation, or Cocoa APIs. It relies on Carbon only for Python 2.7 compatibility.

3. **CLI client protocol is fully documented**: The `odrive.py` script (1680 lines) is a complete, readable, unobfuscated Python CLI client that documents the entire TCP JSON protocol. This is the most valuable reference for reimplementation.

4. **Port discovery**: Both agent and desktop write their protocol server port to a JSON file (`.oreg`) under their respective home directories.

5. **Dual-target IPC**: The CLI tries the agent port first, then falls back to the desktop port. They share the same protocol.

6. **FinderSync Extension uses CFMessagePort**: The Finder extension communicates with the main app via Mach IPC (CFMessagePort named "odriveUI"), not TCP. It sends commands and receives badge state updates.
