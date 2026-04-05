# macOS PKG Installer Analysis: odrivesync.7666.pkg

## Overview

| Property | Value |
|----------|-------|
| File | `odrivesync.7666.pkg` |
| Size | 50 MB |
| Format | xar archive, SHA-1 checksum |
| Version | 1.0.7666 |
| Identifier | `com.oxygen.odrive.installer-prod.pkg` |
| Min macOS | 10.7 (Distribution check), 10.13 (LSMinimumSystemVersion) |
| Install location | `/Applications` |
| Auth | root |
| Code signing | Developer ID Application: Oxygen Cloud, Inc. (N887K88VYZ) |
| Build SDK | macOS 15.2 (Xcode 16.2) |
| Timestamp | Mar 27, 2026 |

## PKG Structure

```
odrivesync.7666.pkg (xar)
+-- Distribution           -- XML install script, version check
+-- Resources/
|   +-- en.lproj/
|       +-- License.rtf    -- License agreement
+-- app.pkg/
    +-- Bom                -- Bill of Materials (14 files)
    +-- PackageInfo        -- Package metadata
    +-- Payload            -- 50 MB gzip+cpio archive
    +-- Scripts            -- 34 KB gzip+cpio archive
```

## Installer Scripts

### preinstall / preupgrade
- Terminates running `odriveapp` process (SIGTERM, then SIGKILL after 4s)
- Runs as root
- Logs to `/tmp/.odrive-preinstall.log`

### postinstall / postupgrade
- Reads `product.conf` to determine prod vs dev variant
- Registers app with LaunchServices via `lsregister`
- Launches the app via three methods (AppleScript x2, then `open`)
  - Uses Finder AppleScript: `tell application "Finder" to open application file id "com.oxygencloud.odrive"`
- Activates Installer.app to bring it back to foreground
- **postinstall only**: Adds login item via System Events AppleScript
  - `tell application "System Events" to make new login item with properties {path:"/Applications/odrive.app", hidden:false}`
- **postupgrade**: Same but omits the login item creation (already exists)
- Logs to `/tmp/.odrive-install.log`

### product.conf
Contains single word: `prod`

## Three-Layer App Architecture

The PKG installs a **three-layer nesting** structure:

### Layer 1: Outer Shell (`/Applications/odrive.app`)
- Bundle ID: `com.oxygencloud.odrive`
- `LSBackgroundOnly: true` -- invisible, no Dock icon
- Registers `.odrivestart` file type
- Contains a thin Mach-O x86_64 launcher binary (6 MB)
- Links: Carbon, libSystem, libz, libgcc_s, ApplicationServices
- Resources: `app.manifest`, `app.tar.gz` (44 MB), `version`, `entitlements.plist`

### Layer 2: Inner App (`bin/7666/odriveapp.app`)
- Bundle ID: `com.oxygen.odriveapp`
- This is the actual application, extracted from `app.tar.gz`
- `LSUIElement: true` -- status bar app, no Dock icon
- Thin Mach-O x86_64 launcher binary (436 KB) -- Obj-C/Swift
- Links: Python.framework 2.7, Foundation, AppKit, CoreFoundation, UserNotifications
- **Obj-C AppDelegate** with NSStatusItem, NSMenu, NSWindowController
- **UserNotifications** framework for system notifications
- Total size: 114 MB expanded

### Layer 3: FinderSync Extension (`.appex`)
- Lives at `odriveapp.app/Contents/PlugIns/FinderSyncExtension.appex`
- Bundle ID: `com.oxygen.odriveapp` (shared with parent)
- Written in **Swift** (bundles 15 Swift runtime dylibs)
- Links: FinderSync.framework, AppKit, Foundation, XPC
- Extension point: `com.apple.FinderSync`
- Principal class: `FinderSyncExtension.FinderSync`
- Size: 12 MB (mostly Swift runtime libs)
- Badge icons: `synced.icns`, `syncing.icns`, `locked.icns`, `infinity-darkgrey.icns`, `infinity-pink.icns`

## App Manifest (Update System)

```yaml
bundles:
- deleteIfExist:
  - bin/7666
  destination: ''
  name: app.tar.gz
  sha256: 9d9c247e17bca651923f0e23add651526944a86565b6f0ca715d13e44305050d
  url: local
  version: 7666
installer:
  name: odrive.pkg
  sha256: local
  url: local
  version: 7666
launchPath: bin/7666/odriveapp.app
min_installer_version: 1
```

This reveals the auto-update mechanism: the outer shell app reads `app.manifest`, downloads newer `app.tar.gz` bundles, extracts them to `bin/<version>/`, and launches the inner app. Old versions are deleted via `deleteIfExist`.

## Entitlements

```xml
com.apple.security.cs.allow-jit: true
com.apple.security.cs.allow-unsigned-executable-memory: true
com.apple.security.cs.allow-dyld-environment-variables: true
com.apple.security.cs.disable-library-validation: true
```

These permissive entitlements are needed because:
- JIT + unsigned executable memory: Python 2.7 interpreter
- dyld environment variables: custom framework load paths
- disable library validation: loading Python .so extensions without Apple signing

## Python 2.7 Runtime

The inner app bundles a complete Python 2.7 framework:
- `Contents/Frameworks/Python.framework/Versions/2.7/Python`
- `Contents/Frameworks/libssl.1.1.dylib`, `libcrypto.1.1.dylib`
- `Contents/Frameworks/libsqlite3.0.dylib`, `libreadline.8.1.dylib`

### Application Code (391 compiled .so modules)

Source modules are compiled to `.so` (Cython or similar), not `.pyc`:
- **272 unique src modules** in the PKG app
- All source is compiled -- only `__init__.pyo` files remain as bytecode

### macOS-Specific Modules (not in Linux agent)

| Module | Purpose |
|--------|---------|
| `src.common.services.shell.MacShellService` | macOS shell integration |
| `src.file_system_sync9.MacFSEventService` | FSEvents monitoring |
| `src.utility.mac_os_util.MacOSUtil` | macOS utility functions |
| `src.utility.macutil` | Additional macOS utilities |
| `src.utility.KeyChainService` | Keychain Access integration |
| `src.odrive_app.controllers.MacOSBadgeController` | Finder badge management |
| `src.odrive_app.servers.MacOSExtensionServer` | FinderSync Extension server |
| `src.odrive_app.servers.MacOSUIServer` | macOS native UI server |
| `src.odrive_app.servers.ThreadedCFMessagePortServer` | CFMessagePort IPC |
| `src.odrive_app.services.MacOSExtensionService` | FinderSync Extension service |
| `src.odrive_app.services.MacOSUIService` | macOS native UI service |
| `src.odrive_app.services.MacPackageService` | PKG/update management |

## UI Window Controllers (NIBs)

36 compiled NIB files reveal every dialog in the desktop app:

| NIB | Purpose |
|-----|---------|
| MainMenu | Main menu bar / status item menu |
| LoginWindowController | Authentication dialog |
| WelcomeWindowController | First-run welcome |
| SyncWindowController | Sync confirmation dialog |
| SyncDeleteWindowController | Delete during sync dialog |
| SyncErrorWindowController | Sync error display |
| AddMountWindowController | Add storage mount |
| LinkAuthWindowController | OAuth link authentication |
| CreateBackupWindowController | Backup job creation |
| ConfirmBackupSettingsWindowController | Backup settings confirmation |
| BackupInfoWindowController | Backup job info |
| BGSyncPrefsWindowController | Background sync preferences |
| AutoUnsyncWindowController | Auto-unsync threshold settings |
| EmptyTrashWindowController | Trash emptying confirmation |
| RestoreTrashWindowController | Trash restoration |
| EncryptionEnterPasswordWindowController | Encryption password entry |
| EncryptionInitializePasswordWindowController | Encryption password setup |
| EncryptorEnterPasswordWindowController | Encryptor password entry |
| EncryptorInitializePasswordWindowController | Encryptor password setup |
| SendDiagnosticsWindowController | Diagnostic upload |
| PremiumRequiredWindowController | Premium upsell |
| SubscriptionRequiredWindowController | Subscription upsell |
| MajorUpdateWindowController | Major update notification |
| OSNotSupportedWindowController | OS version too old |
| OdriveFolderMissingWindowController | odrive folder missing |
| OdriveFolderInvalidWindowController | odrive folder invalid |
| ProSyncFolderMissingWindowController | Pro sync folder missing |
| ProSyncFolderInvalidWindowController | Pro sync folder invalid |
| CancelSyncRequestWindowController | Cancel sync request |
| CancelExpandRequestWindowController | Cancel expand request |
| CancelBackgroundSyncWindowController | Cancel background sync |
| CancelNotAllowedItemWindowController | Not-allowed item dialog |
| UnsyncDirtyWindowController | Unsync with dirty files |
| DeauthorizeUserWindowController | Deauthorize confirmation |
| NotificationsPromptWindowController | Notification permission prompt |
| AlertWindowController | Generic alert |

## Custom File Types

| Extension | Type | Icon |
|-----------|------|------|
| `.cloud` | CloudFile | cloud placeholder file |
| `.cloudf` | CloudFolder | cloud placeholder folder |
| `.cloudl` / `.cloudlr` | Cloud link | cloud link placeholder |
| `.cloudfl` / `.cloudflr` | Cloud folder link | cloud folder link |
| `.lockd` / `.lockdr` | Locked file | locked item |
| `.gdocx` | Google Doc | Google Docs icon |
| `.gsheetx` | Google Sheet | Google Sheets icon |
| `.gslidesx` | Google Slides | Google Slides icon |
| `.gdrawx` | Google Drawing | Google Drawing icon |
| `.gformx` | Google Form | Google Forms icon |
| `.gmapx` | Google Map | Google Maps icon |
| `.onotex` | OneNote | OneNote icon |
| `.odrivestart` | Config file | (outer shell only) |

## IPC Architecture

The desktop app exposes multiple IPC mechanisms:

1. **TCP Protocol Server** (localhost, dynamic port)
   - Port stored in `~/.odrive/.oreg` (desktop) or `~/.odrive-agent/.oreg` (agent)
   - JSON-over-TCP protocol (same as Linux/Windows agent)
   - Used by `odrive.py` CLI client

2. **CFMessagePort** (Mach IPC)
   - `ThreadedCFMessagePortServer` for fast local IPC
   - Used by FinderSync Extension to communicate with main app
   - Named port: likely `odriveUI` (from Finder extension strings)

3. **FinderSync Extension**
   - Communicates badge state and context menu actions
   - Receives file paths and sync states
   - Shows overlay badges: Synced, Syncing, Active, Not Sync, Locked

## Third-Party Dependencies

| Package | Purpose |
|---------|---------|
| PyCrypto (Crypto.*) | Encryption (AES, DES, etc.) |
| psutil | Process/system monitoring |
| paramiko | SFTP transport |
| ftputil | FTP support |
| backports.ssl_match_hostname | SSL compatibility |

## Comparison: PKG Desktop App vs Standalone Agent

| Aspect | PKG Desktop (v7666) | Standalone Agent (v977) |
|--------|---------------------|------------------------|
| Binary | Obj-C/Swift launcher + Python 2.7 | PyInstaller frozen binary |
| UI | Full native macOS (AppKit, NIBs, NSStatusItem) | Headless / CLI only |
| Finder | FinderSync Extension (.appex) | Carbon/Finder AppleScript |
| Notifications | UserNotifications framework | None |
| Architecture | x86_64 | x86_64 |
| Code signing | Hardened runtime | Hardened runtime |
| Total size | ~114 MB | ~8.4 MB |
| Data dir | `~/.odrive/` | `~/.odrive-agent/` |
| IPC | TCP + CFMessagePort | TCP only |
| Python src modules | 272 compiled .so | 295 (includes __init__) |
| Auto-update | manifest-based bundle system | Unknown |
