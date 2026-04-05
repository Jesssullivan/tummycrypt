# Windows odrive Client -- .NET Assembly Decompilation

Date: 2026-04-04
Tool: ILSpy 9.1 (`ilspycmd`) via Nix
Source: `odrivesync.7513.exe` (68MB WiX Burn bootstrapper, build 7513)

## Key Finding: Architecture Correction

The Windows desktop client is **not** a .NET application. It is a **Python 2.7 frozen
application** (using `python27.dll`), identical in architecture to the Linux client.
The .NET components are limited to two small helper executables:

| Assembly | Size | Framework | Role |
|---|---|---|---|
| `odrive.exe` | 166KB decompiled | .NET 4.6 | WinForms system tray UI |
| `OdriveOpen.exe` | ~20KB decompiled | .NET 4.0 | File extension / URL protocol handler |

The main sync engine is `odriveapp.exe` (native PE32, Python 2.7 frozen with
py2exe or similar), accompanied by `app.tar.gz` (23MB of Python bytecode),
`common.tar.gz` (39KB shared utilities), and `misc.tar.gz` (34MB dependencies).

---

## Extraction Path

```
odrivesync.7513.exe                    (WiX Burn bootstrapper, native PE32)
  |-- [WiX UX cab]                     (bootstrapper UI resources)
  |     |-- wixstdba.dll               (WiX Standard BA)
  |     |-- thm.xml / thm.wxl          (theme + localization)
  |     |-- logo.png                   (64x64 icon)
  |     |-- license.rtf                (EULA)
  |     +-- BootstrapperApplicationData.xml
  |
  +-- [WixAttachedContainer]           (67MB cab, attached payload)
        |-- a0: .NET Framework 4.6 Setup (1.5MB, prerequisite installer)
        |-- a1: odrive.x86.msi         (1.3MB, 32-bit MSI)
        |-- a2: odrive.x64.msi         (1.3MB, 64-bit MSI)
        |-- a3: odrive.cab             (65MB, shared between x86/x64)
        +-- a4: odrive.cab             (same file, duplicate reference)
```

The MSIs are structurally identical (same cab hash `A92071E1AD03CEC162A0C3F8A11FEA2720D1C3FB`)
except for architecture template (`Intel` vs `x64`).

### WiX Bundle Metadata

- **Bundle ID**: `{b4682a69-b335-4514-87f9-d717f6a9fd51}`
- **Upgrade Code**: `{D74FBC41-F53D-4A05-93D4-BB5EBA90D383}`
- **x86 Product Code**: `{59A33B45-D172-4372-BEDD-E96D9E7EDC33}`
- **x64 Product Code**: `{CFBCAAE9-4262-4F5E-8A01-3E8A5A5294B9}`
- **Upgrade Code (MSI)**: `{7E242AB3-0F67-4127-9659-CFB7CD432E43}`
- **Publisher**: Oxygen Cloud, Inc.
- **WiX Toolset**: 3.10.3.3007
- **Minimum OS**: Windows 7 (VersionNT >= v6.1)

---

## Cab Contents (40 files, 75MB uncompressed)

### Native Executables
- `odriveapp.exe` -- Main sync engine (Python 2.7 frozen, PE32 console)
- `filOdrive.exe` -- Installer helper (native PE32)
- `OdriveExplorerHelper.exe` -- Explorer integration helper (native PE32 console)

### .NET Assemblies (ILSpy decompilable)
- `odrive.exe` -- System tray UI (.NET 4.6, WinForms)
- `OdriveOpen.exe` -- File/URL handler (.NET 4.0)

### Shell Extensions (native C++ COM DLLs)
- `ActiveOverlay.dll` / `x64/ActiveOverlay.dll` -- Synced file icon overlay
- `SyncedOverlay.dll` / `x64/SyncedOverlay.dll` -- Synced folder icon overlay
- `LockedOverlay.dll` / `x64/LockedOverlay.dll` -- Locked file icon overlay
- `ContextMenu.dll` / `x64/ContextMenu.dll` -- Right-click context menu handler

### Python Runtime
- `python27.dll` / `x64/python27.dll` -- Python 2.7 interpreter
- `pythoncom27.dll` / `x64/pythoncom27.dll` -- COM automation
- `pywintypes27.dll` / `x64/pywintypes27.dll` -- Win32 types

### Python Extension Modules (.pyd)
Crypto: `_AES`, `_ARC4`, `_Blowfish`, `_DES`, `_DES3`, `_SHA256`, `strxor`, `_counter`, `winrandom`
Networking: `_socket`, `_ssl`, `select`
Win32: `win32api`, `win32file`, `win32pipe`, `win32event`, `win32gui`, `win32process`,
       `win32trace`, `win32ui`, `win32wnet`, `win32cred`, `win32security`, `win32com.shell.shell`,
       `winxpgui`, `_win32sysloader`
Other: `_ctypes`, `_multiprocessing`, `_elementtree`, `_hashlib`, `pyexpat`, `bz2`,
       `unicodedata`, `apsw` (SQLite), `psutil._psutil_windows`, `sip` (SIP/Qt bindings),
       `cryptography.hazmat.bindings._openssl`, `cryptography.hazmat.bindings._constant_time`,
       `_bsddb`

### Application Bundles
- `app.tar.gz` (23MB) -- Main Python application code (version 7513)
- `common.tar.gz` (39KB) -- Shared utilities (OdriveOpen.exe, uninstall script, version)
- `misc.tar.gz` (34MB) -- Dependencies/libraries (version 18)

### App Manifest (`app.manifest`)
```yaml
launchPath: bin/7513/odriveapp.exe
installer:
  name: odrive.exe
  version: 7513
bundles:
  - name: app.tar.gz
    version: 7513
    destination: ''
    miscExtractionPoint: bin/7513/
  - name: common.tar.gz
    version: 7513
    destination: common/bin
  - name: misc.tar.gz
    version: 18
    destination: common/misc
    noExtract: true
min_installer_version: 1
```

---

## Decompilation: odrive.exe (Tray UI)

**Assembly**: `odrive.exe`
**Framework**: .NET Framework 4.6
**Namespace**: `odrive`
**GUID**: `14673ecf-888c-40b4-ab38-c573ac322442`
**Copyright**: 2016, Oxygen Cloud Inc.

### Classes

| Class | Description |
|---|---|
| `odrive.Program` | Entry point, launches `odriveUIApplicationContext` |
| `odrive.odriveUIApplicationContext` | WinForms ApplicationContext; sets up tray icon + IPC |
| `odrive.odriveNotifyIcon` | System tray icon, context menu, status polling |
| `odrive.odriveNotifyIconContextMenuStrip` | Custom context menu strip with close delegate |
| `odrive.UIController` | Controller bridge between AppService and NotifyIcon |
| `odrive.AppService` | TCP client for sending commands to `odriveapp` (Python engine) |
| `odrive.UIServer` | TCP server for receiving UI render commands from `odriveapp` |
| `odrive.AutoSizingLabel` | Custom Label control that auto-sizes height |
| `odrive.Dialogs.AlertDialog` | Generic alert dialog (WinForms Form) |
| `odrive.Commands` | Enum of all IPC command codes |

### Commands Enum (complete)

```csharp
public enum Commands
{
    CLICKED = 0,
    GOOGLE_OPEN = 24,
    ONEDRIVE_OPEN = 26,
    LOGIN = 2000,
    AUTH = 2001,
    FORCE_UNSYNC = 2002,
    EMPTY_TRASH = 2003,
    SYNC_DELETE = 2004,
    RESTORE_DELETE = 2005,
    DISCARD_SYNC_ERROR = 2006,
    SEND_DIAGNOSTICS = 2007,
    UPDATE_AND_RESTART = 2008,
    // 2009 unused
    ADD_MOUNT = 2010,
    GET_STARTED = 2011,
    GET_SYSTEM_STATUS_ITEMS = 2012,
    GET_SYSTEM_STATUS_ICON_IDENTIFIER = 2013,
    GET_DEV_SYSTEM_STATUS_ITEMS = 2014,
    ADD_LINK = 2015,
    QUIT = 2016,
    REPAIR_MOUNT = 2017,
    OPEN_FOLDER = 2018,
    MOVE_ODRIVE_FOLDER = 2019,
    CANCEL_SYNC = 2020,
    REVEAL_IN_FILE_BROWSER = 2021,
    RENDER_NOT_ALLOWED_ITEM_DIALOG = 2022,
    STOP_SYNC = 2023,
    START_SYNC = 2024,
    // 2025 unused
    SET_PLACEHOLDER_THRESHOLD = 2026,
    RENDER_SYNC_DELETE_DIALOG = 2027,
    LOGOUT = 2028,
    RENDER_SEND_DIAGNOSTICS_DIALOG = 2029,
    START_APP_IGNORING_OS_VERSION_SUPPORT = 2030,
    SET_ODRIVE_FOLDER_PATH = 2031,
    RENDER_EMPTY_TRASH_DIALOG = 2032,
    CANCEL_EXPAND_REQUEST = 2033,
    CANCEL_SYNC_REQUEST = 2034,
    RENDER_CANCEL_SYNC_DIALOG = 2035,
    RENDER_CANCEL_EXPAND_DIALOG = 2036,
    RENDER_SETUP_ODRIVE_UI = 2037,
    SYNC_FOLDER_WITH_PARAMETERS = 2038,
    UPDATE_BG_SYNC_PREFERENCES = 2039,
    RENDER_BG_SYNC_PREFERENCES = 2040,
    // 2041-2042 unused
    MANAGE_LINKS = 2043,
    RENDER_CANCEL_BACKGROUND_SYNC_DIALOG = 2044,
    CANCEL_BACKGROUND_SYNC_OPERATION = 2045,
    // 2046-2047 unused
    RENDER_CANCEL_NOT_ALLOWED_ITEM_DIALOG = 2048,
    CANCEL_NOT_ALLOWED_ITEM = 2049,
    SET_ENCRYPTOR_PASSWORD = 2050,
    TEST_ENCRYPTOR_PASSWORD = 2051,
    SET_PRO_SYNC_FOLDER_PATH = 2052,
    RENDER_ODRIVE_FOLDER_CHOOSER = 2053,
    RENDER_PRO_SYNC_FOLDER_CHOOSER = 2054,
    OPEN_ODRIVE_FOLDER = 2055,
    OPEN_PRO_SYNC_FOLDER = 2056,
    // 2057-2058 unused
    REMOVE_PRO_SYNC_FOLDER = 2059,
    RENDER_DEAUTHORIZE_USER_DIALOG = 2060,
    DEAUTHORIZE_USER = 2061,
    SET_AUTO_UNSYNC_THRESHOLD = 2062,
    SET_DOWNLOAD_THROTTLING_THRESHOLD = 2063,
    SET_UPLOAD_THROTTLING_THRESHOLD = 2064,
    SET_AUTO_TRASH_THRESHOLD = 2065,
    PURCHASE_PREMIUM = 2066,
    REFRESH_POLICY = 2067,
    FORUM_ANNOUNCEMENT = 2068,
    SELECT_BACKUP_DESTINATION = 2069,
    CONFIRM_BACKUP_DESTINATION = 2070,
    RENDER_BACKUP_INFO_DIALOG = 2071,
    PAUSE_BACKUPS = 2072,
    BACKUP_NOW = 2073,
    OPEN_BACKUP_ACTIVITY_LOG = 2074,
    FORGET_BACKUP_JOB = 2075,
    TEST_ENCRYPTION_PASSWORD = 2076,
    ENABLE_ENCRYPTION_WITH_PASSWORD = 2077,
    SET_XL_FILE_THRESHOLD = 2078,
    ADD_BACKUP = 2079,
    ADD_BACKUP_WITH_PATH = 2080,
    OPEN_USAGE_GUIDE = 2081,
    SET_UI_SERVER_PORT = 2082,
    DISABLE_REMOTE_SCAN = 2083,
    ENABLE_REMOTE_SCAN = 2084,
    RENDER_AUTO_UNSYNC_DIALOG = 2085,
    RENDER_AUTO_TRASH_DIALOG = 2086,
    RESTORE_TRASH = 2087,
    RENDER_RESTORE_TRASH_DIALOG = 2088,
    OPEN_SYNC_ACTIVITY_LOG = 2089,
    RENDER_WAITING_ITEM_DIALOG = 2090,
    CREATE_DETAILED_ODRIVE_STATUS = 2091,
    CHAT_WITH_AGENT = 2092
}
```

### IPC Architecture

The tray UI uses **bidirectional TCP IPC** on `127.0.0.1`:

1. **Port discovery**: Reads `%USERPROFILE%\.odrive\.oreg` (JSON file) to find the
   Python engine's UI server port. The `.oreg` structure is:
   ```json
   {
     "odrive-prod": { "ui": <port>, "nonpersistent": <port> },
     "odrive-dev":  { "ui": <port>, "nonpersistent": <port> },
     "current":     { "nonpersistent": <port> }
   }
   ```

2. **Tray -> Engine (AppService)**: Sends JSON commands over TCP to the Python engine's
   UI port. Format: `{"command": <int>, "parameters": {...}}\n`
   Response: JSON dict read as single line.

3. **Engine -> Tray (UIServer)**: The tray opens its own TCP server on a random
   ephemeral port (49152-65535) and registers it with the engine via
   `SET_UI_SERVER_PORT` (2082). Engine sends JSON commands to render dialogs.

4. **Product resolution**: Determined from executable name:
   - `odrivedev.exe` or `Debug/` dir -> `odrive-dev`
   - `odrivebeta.exe` -> `odrive-beta`
   - Otherwise -> `odrive-prod`

### UIServer Service Commands (Engine -> Tray)

These string-based commands are sent from the Python engine to trigger UI rendering:

```
renderSignInDialog
renderAuthDialog
renderAlertDialog
renderUnsyncDirtyDialog
renderEmptyTrashDialog
renderRestoreTrashDialog
renderAutoUnsyncDialog
renderSyncDeleteDialog
renderCancelSyncRequestDialog
renderCancelExpandRequestDialog
renderSyncErrorDialog
renderSendDiagnosticsDialog
renderMajorUpdateDialog
renderWelcomeDialog
renderOSNotSupportedDialog
renderOdriveFolderChooser
renderProSyncFolderChooser
renderSyncDialog
renderBGSyncPreferences
renderCancelBackgroundSyncDialog
renderCancelNotAllowedItemDialog
renderEncryptorInitializePasswordDialog
renderEncryptorEnterPasswordDialog
renderEncryptionInitializePasswordDialog
renderEncryptionEnterPasswordDialog
renderOdriveFolderMissingDialog
renderProSyncFolderMissingDialog
renderOdriveFolderInvalidDialog
renderProSyncFolderInvalidDialog
renderDeauthorizeUserDialog
deliverUserNotification
renderPremiumRequiredDialog
renderCreateBackupDialog
renderConfirmBackupSettingsDialog
renderBackupInfoDialog
renderBackupFolderChooser
```

Note: Only `deliverUserNotification` is actually dispatched in the decompiled code.
The remaining commands have string constants defined but the `DispatchCommand()` method
only handles notifications. This suggests the UI server was either planned for expansion
or the dialog rendering is handled elsewhere (possibly the Python engine renders dialogs
directly using the Win32 API or a web view).

### Status Polling

The tray icon polls `GET_SYSTEM_STATUS_ITEMS` (2012) every 2 seconds. Response fields:

```
isProd, isActivated, hasSession, syncEnabled, remoteScanEnabled,
majorUpdateAvailable, validPremiumSubscription, timeTillNextBackup,
lastBackupTime, authorizedEmail, authorizedAccountSourceType,
placeholderThreshold, autoUnsyncThreshold, xlFileThreshold,
autoTrashThreshold, downloadThrottlingThreshold, uploadThrottlingThreshold,
productVersion, odriveFolder {name, path, status},
proSyncFolders[], syncRequests[], expandRequests[],
refreshChildOperations[], uploads[], downloads[],
trashItems[], waitingItems[], notAllowedItems[],
backupJobs[], oxygenMcpChatMenuEnabled
```

### Tray Icon States

| Identifier | Icon | Condition |
|---|---|---|
| `signedIn` | `odrivewhite` (Win10) / `odriveblack` | Authenticated, idle |
| `syncing` | 17-frame spinner animation (125ms/frame) | Active sync |
| `backingUp` | Same spinner animation | Active backup |
| `signedOut` | `odrivegrey` | Not authenticated |

### Configuration Thresholds

**Placeholder (auto-download) threshold**:
`neverDownload`, `small` (10MB), `medium` (100MB), `large` (500MB), `alwaysDownload`

**Auto-unsync threshold**:
`never`, `minute`, `day`, `week`, `month`, `custom`

**Auto-trash threshold**:
`never`, `immediately`, `fifteenMinutes`, `hour`, `day`, `custom`

**Download/upload throttling**:
`unlimited`, `normal`, `limited`

**XL file threshold**:
`never`, `extraSmall` (1MB), `small` (100MB), `medium` (500MB), `large` (1GB), `extraLarge` (2GB)

---

## Decompilation: OdriveOpen.exe (File Handler)

**Assembly**: `OdriveOpen.exe` (originally named `ODriveOpen`)
**Framework**: .NET Framework 4.0
**Namespace**: `OxygenOpen`
**GUID**: `571e1e60-3fa7-4988-a446-a5dfafb4339e`
**Copyright**: 2013

### Purpose

File association handler invoked by Windows Explorer when users double-click
odrive placeholder files (`.cloud`, `.cloudf`, `.lockd`, etc.) or `odrive://` URLs.

### File Extension Registry

**Production extensions**:
- `.cloud` -- Cloud file placeholder
- `.cloudf` -- Cloud folder placeholder
- `.gdocx` -- Google Doc
- `.gsheetx` -- Google Sheet
- `.gformx` -- Google Form
- `.gslidesx` -- Google Slides
- `.gdrawx` -- Google Drawing
- `.gmapx` -- Google Map
- `.onotex` -- OneDrive Notebook
- `.lockd` -- Locked file
- `.cloudl` -- Locked cloud file
- `.cloudfl` -- Locked cloud folder
- `.lockdr` -- Encrypted-name locked file
- `.cloudlr` -- Encrypted-name locked cloud file
- `.cloudflr` -- Encrypted-name locked cloud folder

Each extension also has `-dev` and `-beta` variants (e.g., `.cloud-dev`, `.cloud-beta`).

### IPC Protocol (OdriveOpen -> Python engine)

Uses raw TCP sockets to `127.0.0.1:<nonpersistent_port>`. Port is read from
`%USERPROFILE%\.odrive\.oreg` under the key `[appId]["nonpersistent"]`.

**Message format**: `<command_id>:::<payload>\n`
**Response**: 1024-byte buffer read.

| Command ID | Trigger | Payload |
|---|---|---|
| `0` | `.cloud`, `.cloudf`, `.lockd`, `.cloudl`, etc. | JSON: `{"paths":["..."],"idPath":"...","trackingFilePath":"..."}` |
| `24` | `.gdocx`, `.gsheetx`, `.gslidesx`, etc. | JSON: same structure |
| `26` | `.onotex` | JSON: same structure |
| `10` | Cloud invite file (non-folder) | Raw invite string from file |
| `11` | Cloud invite file (folder) | Raw invite string from file |
| `12` | `odrive://` URL protocol | Raw URL string |

### Tracking File Resolution

OdriveOpen walks up the directory tree from the clicked file, looking for `.odrive`
marker files (JSON containing `{"appId": "...", "idPath": "..."}`). This determines
which product instance (odrive-prod, odrive-dev, etc.) should handle the file.

### App Launch Logic

If no running process matches the product, OdriveOpen reads the installation path
from the registry and launches the appropriate executable:

| Product | Process name | Registry key | Launcher |
|---|---|---|---|
| odrive | `odriveapp` | `HKLM\SOFTWARE\odrive` | `odrive.exe` |
| Oxygen Enterprise | `oxygenapp` | `HKLM\SOFTWARE\Oxygen Enterprise` | `Oxygen Enterprise.exe` |
| testmule | `testmuleapp` | `HKLM\SOFTWARE\testmule` | `testmule.exe` |

Product variant is derived from the appId: `odrive-prod`, `odrive-dev`, `odrive-beta`.
Registry key includes variant suffix (e.g., `SOFTWARE\odrive dev`).

### Native Interop

OdriveOpen uses P/Invoke for process inspection:
```csharp
[DllImport("kernel32.dll")]
QueryFullProcessImageName(IntPtr hprocess, int dwFlags, StringBuilder lpExeName, out int size);

[DllImport("kernel32.dll")]
IntPtr OpenProcess(ProcessAccessFlags dwDesiredAccess, bool bInheritHandle, int dwProcessId);

[DllImport("kernel32.dll")]
int GetShortPathName(string path, StringBuilder shortPath, int shortPathLength);
```

---

## Uninstall Script (CU_Uninstall.ps1)

PowerShell script for per-user cleanup during uninstallation:

1. Removes HKCU registry entries for file associations and shell extensions
2. Removes Win10 SyncRootManager entries from HKLM
3. Kills `odriveapp` and `odrive` processes
4. Restarts Explorer (to release shell extension DLLs)
5. Removes odrive Explorer shortcut from Favorites/Links
6. Renames `$HOME\odrive` to `$HOME\odrive_backup_<timestamp>`
7. Deletes `$HOME\.odrive` configuration directory

### Shell Extension CLSIDs

| CLSID | Purpose |
|---|---|
| `{102986AF-0728-447D-83B4-C3CD4F70F273}` | Explorer namespace extension (virtual folder) |
| `{4585263E-BEF5-4A39-A2E8-8F69E0054F0C}` | AppId registration |
| `{679ADC87-66BB-43BF-9DC3-3DE2E4A32B8C}` | AppId registration |
| `{E07BCA71-E88B-4A5E-BA46-69A52D6B9B20}` | AppId registration |
| `{35B08E96-DA1F-4321-BF80-D6B53C20F3CF}` | AppId registration |

---

## Cross-Platform Architecture Summary

The Windows client has the **same architecture** as the Linux and macOS clients:

```
                    +-------------------+
                    | Platform UI Layer |
                    | (odrive.exe .NET) |
                    |  System Tray Icon |
                    +--------+----------+
                             |
                    TCP IPC (JSON over localhost)
                    Port from ~/.odrive/.oreg
                             |
                    +--------v----------+
                    |    odriveapp.exe   |
                    |  (Python 2.7 core)|
                    |  Sync Engine      |
                    +--------+----------+
                             |
                    TCP IPC (command:::payload)
                    "nonpersistent" port
                             |
                    +--------v----------+
                    |  OdriveOpen.exe   |
                    | (.NET file handler)|
                    | Shell extensions  |
                    +-------------------+
```

All platforms share the same Python sync engine; only the UI layer differs:
- **Windows**: .NET WinForms tray icon + native C++ shell extensions
- **macOS**: Objective-C/Swift menu bar app + Finder extensions
- **Linux**: Python GTK tray icon (same process as engine)

---

## ILSpy Methodology Notes

- Tool: `ilspycmd 9.1.0` from Nix (`/Users/jess/.nix-profile/bin/ilspycmd`)
- Extraction: `p7zip 17.06` via `nix-shell -p p7zip` for WiX bootstrapper unpacking
- Three-stage extraction: PE compound -> WiX attached container cab -> odrive.cab -> tarballs
- The main `odrivesync.7513.exe` and `odriveapp.exe` are native PE32 (not .NET),
  confirmed by ILSpy error: "PE file does not contain any managed metadata"
- Only `odrive.exe` and `OdriveOpen.exe` are .NET assemblies (Mono/.Net flag in PE header)
- Full decompilation succeeded for both assemblies with no errors
- Project-mode output generated for `odrive.exe` (8 source files + resources)
