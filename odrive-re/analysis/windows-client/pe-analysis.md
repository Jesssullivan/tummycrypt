# odrive Windows Client PE Analysis

**Binary**: `odrivesync.7513.exe`
**Size**: 67,822,896 bytes (64.7 MB)
**Type**: PE32 executable (GUI) Intel 80386, for MS Windows, 7 sections
**Version**: 1.0.7513
**Publisher**: Oxygen Cloud, Inc.
**Build tool**: WiX Toolset v3.10.3.3007
**Signing**: Sectigo Public Code Signing CA R36 + DigiCert timestamping
**Date**: 2026-03-27 (MSI creation dates)
**Min OS**: Windows 7 (VersionNT >= v6.1)
**CI**: Hudson/Jenkins (`C:\hudson\workspace\Odrive_Win_Master_Build\`)

---

## 1. Installer Structure (WiX Burn Bootstrapper)

The outer EXE is a **WiX Burn bootstrapper** -- not the application itself. It chains
multiple packages together.

### PE Sections

| Section   | VA         | Raw Size |
|-----------|------------|----------|
| .text     | 0x00001000 | 302 KB   |
| .rdata    | 0x0004b000 | 123 KB   |
| .data     | 0x0006a000 | 3 KB     |
| .wixburn  | 0x0006c000 | 0.5 KB   |
| .tls      | 0x0006d000 | 0.5 KB   |
| .rsrc     | 0x0006e000 | 170 KB   |
| .reloc    | 0x00099000 | 16 KB    |

### Embedded Containers

**UX Cabinet** (108 KB at offset 0x98200) -- bootstrapper UI:

| File | Content                           | Size   |
|------|-----------------------------------|--------|
| u0   | wixstdba.dll (Burn BA DLL)        | 176 KB |
| u1   | thm.xml (UI theme: 600x450 win)   | 5 KB   |
| u2   | thm.wxl (UI localization: en-us)  | 4 KB   |
| u3   | logo.png (64x64 icon)             | 2 KB   |
| u4   | license.rtf                       | 24 KB  |
| u5   | BootstrapperApplicationData.xml   | 6 KB   |

**Payload Cabinet** (64.0 MB at offset 0xb54f8) -- main content:

| File | Content                              | Size     | Date       |
|------|--------------------------------------|----------|------------|
| a0   | .NET Framework 4.6 setup stub        | 1.4 MB   | 2021-09-24 |
| a1   | odrive.x86.msi (32-bit installer)    | 1.3 MB   | 2026-03-27 |
| a2   | odrive.x64.msi (64-bit installer)    | 1.3 MB   | 2026-03-27 |
| a3   | odrive.cab (app files, for x86 MSI)  | 62.2 MB  | 2026-03-27 |
| a4   | odrive.cab (app files, for x64 MSI)  | 62.2 MB  | 2026-03-27 |

Note: a3 and a4 are identical (same SHA1 hash `A92071E1AD03CEC162A0C3F8A11FEA2720D1C3FB`).

### Install Chain

1. **Net46** (ExePackage) -- installs .NET 4.6 if not already present (detect: registry `Net46Installed >= 393295`)
2. **Odrive_x86.exe** (MsiPackage) -- installs on 32-bit Windows (condition: `NOT VersionNT64`)
3. **Odrive_x64.exe** (MsiPackage) -- installs on 64-bit Windows (condition: `VersionNT64 AND NOT ARM64`)

### Registry Searches (installer)

- `HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment\PROCESSOR_ARCHITECTURE`
- `HKLM\SOFTWARE\Microsoft\Net Framework Setup\NDP\v4\Full\Release` (check .NET 4.6+)

### Bundle Registration

- **BundleId**: `{b4682a69-b335-4514-87f9-d717f6a9fd51}`
- **UpgradeCode**: `{D74FBC41-F53D-4A05-93D4-BB5EBA90D383}`
- **x86 ProductCode**: `{59A33B45-D172-4372-BEDD-E96D9E7EDC33}`
- **x64 ProductCode**: `{CFBCAAE9-4262-4F5E-8A01-3E8A5A5294B9}`
- **UpgradeCode** (MSI): `{7E242AB3-0F67-4127-9659-CFB7CD432E43}`

---

## 2. Application Architecture

The application is a **multi-process** design:

```
                                     +--------------------------+
                                     |    odrive.exe (.NET)     |
                                     |  WinForms system tray    |
                                     |  NotifyIcon + UI Server  |
                                     +-----+------+-------------+
                                           |      ^
                                     TCP   |      | TCP
                                     cmds  |      | render cmds
                                           v      |
+-------------------+          +-----------+------+-----------+
|  OdriveOpen.exe   |          |       odriveapp.exe          |
|  (.NET, file      +---TCP--->|  PyInstaller (Python 2.7)    |
|   handler/opener) |          |  Core sync engine            |
+-------------------+          +----------+-------------------+
                                          |
+-------------------+          +----------+-------------------+
| Shell Extensions  |<--pipe-->| OdriveExplorerHelper.exe     |
| (COM DLLs, 4x)   |          | (native C++, COM registrar)  |
+-------------------+          +------------------------------+
```

### Process Inventory

| Executable                 | Type                | Role                                      |
|----------------------------|---------------------|-------------------------------------------|
| `odriveapp.exe`            | PyInstaller PE32    | Core sync engine (Python 2.7, 4.9 MB)    |
| `odrive.exe`               | .NET WinForms 4.6   | System tray UI, notification icon          |
| `OdriveOpen.exe`           | .NET 4.0            | File association handler (.cloud/.cloudf)  |
| `OdriveExplorerHelper.exe` | Native C++ (MSVC)   | Shell extension COM registration           |

### Shell Extension DLLs (COM, Python-backed via pythoncom27)

Each DLL embeds a `.pyo` file and loads `PYTHON27.DLL` + `pythoncom27.dll` at runtime.

| DLL                 | CLSID (from uninstall script)                  | Purpose              |
|---------------------|------------------------------------------------|----------------------|
| `SyncedOverlay.dll` | `{4585263E-BEF5-4A39-A2E8-8F69E0054F0C}`       | Synced icon overlay  |
| `ActiveOverlay.dll` | `{679ADC87-66BB-43BF-9DC3-3DE2E4A32B8C}`       | Syncing icon overlay |
| `LockedOverlay.dll` | `{E07BCA71-E88B-4A5E-BA46-69A52D6B9B20}`       | Locked icon overlay  |
| `ContextMenu.dll`   | `{35B08E96-DA1F-4321-BF80-D6B53C20F3CF}`       | Right-click context  |

All four have both x86 and x64 variants (`bin/7513/x64/` directory).

### Explorer Namespace Extension

- **CLSID**: `{102986AF-0728-447D-83B4-C3CD4F70F273}`
- Registers under `HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Desktop\NameSpace`
- Appears as an "odrive" virtual folder in Explorer navigation pane
- Also registered in `HideDesktopIcons\NewStartPanel`

---

## 3. IPC Protocol

### tray <-> sync engine (TCP on 127.0.0.1)

Port discovery: `odrive.exe` reads `%USERPROFILE%\.odrive\.oreg` (JSON) to find `odriveapp`'s
UI server port. The `.oreg` file is keyed by product name (`odrive-prod`, `odrive-dev`, `odrive-beta`).

Protocol: JSON-over-TCP, newline-delimited. Each message is:

```json
{"command": <int_enum>, "parameters": {...}}
```

Response is a single JSON line.

### Full Command Enum (from .NET decompilation)

```csharp
enum Commands {
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
    MANAGE_LINKS = 2043,
    RENDER_CANCEL_BACKGROUND_SYNC_DIALOG = 2044,
    CANCEL_BACKGROUND_SYNC_OPERATION = 2045,
    RENDER_CANCEL_NOT_ALLOWED_ITEM_DIALOG = 2048,
    CANCEL_NOT_ALLOWED_ITEM = 2049,
    SET_ENCRYPTOR_PASSWORD = 2050,
    TEST_ENCRYPTOR_PASSWORD = 2051,
    SET_PRO_SYNC_FOLDER_PATH = 2052,
    RENDER_ODRIVE_FOLDER_CHOOSER = 2053,
    RENDER_PRO_SYNC_FOLDER_CHOOSER = 2054,
    OPEN_ODRIVE_FOLDER = 2055,
    OPEN_PRO_SYNC_FOLDER = 2056,
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
    CHAT_WITH_AGENT = 2092,
}
```

### UI Render Commands (UIServer dispatch from odriveapp -> odrive.exe)

The sync engine (`odriveapp.exe`) sends render commands to the tray app's UIServer:

- `renderSignInDialog`, `renderAuthDialog`
- `renderAlertDialog`, `renderWelcomeDialog`
- `renderSyncDialog`, `renderSyncDeleteDialog`
- `renderUnsyncDirtyDialog`, `renderSyncErrorDialog`
- `renderEmptyTrashDialog`, `renderRestoreTrashDialog`
- `renderAutoUnsyncDialog`, `renderCancelSyncRequestDialog`
- `renderCancelExpandRequestDialog`, `renderCancelBackgroundSyncDialog`
- `renderCancelNotAllowedItemDialog`
- `renderSendDiagnosticsDialog`, `renderMajorUpdateDialog`
- `renderOSNotSupportedDialog`
- `renderOdriveFolderChooser`, `renderProSyncFolderChooser`
- `renderBGSyncPreferences`
- `renderEncryptorInitializePasswordDialog`, `renderEncryptorEnterPasswordDialog`
- `renderEncryptionInitializePasswordDialog`, `renderEncryptionEnterPasswordDialog`
- `renderOdriveFolderMissingDialog`, `renderProSyncFolderMissingDialog`
- `renderOdriveFolderInvalidDialog`, `renderProSyncFolderInvalidDialog`
- `renderDeauthorizeUserDialog`, `renderPremiumRequiredDialog`
- `renderCreateBackupDialog`, `renderConfirmBackupSettingsDialog`
- `renderBackupInfoDialog`, `renderBackupFolderChooser`
- `deliverUserNotification`

---

## 4. Python Runtime (odriveapp.exe)

- **Python version**: 2.7 (python27.dll)
- **Packaging**: PyInstaller (cookie magic `MEI\x0c` found)
- **Architecture**: PE32 (console mode, x86)
- **Size**: 4.9 MB (standalone, references external python27.dll and .pyd files)
- **DPI awareness**: Explicitly set to "unaware" (app.manifest)

### Python Extension Modules (.pyd files)

| Module                                      | Purpose                      |
|---------------------------------------------|------------------------------|
| `_ctypes.pyd`                               | C FFI                        |
| `_multiprocessing.pyd`                       | Multiprocessing              |
| `_socket.pyd`                                | Socket support               |
| `_ssl.pyd`                                   | SSL/TLS                      |
| `_hashlib.pyd`                               | Hashing                      |
| `_elementtree.pyd`                           | XML parsing                  |
| `_bsddb.pyd`                                 | Berkeley DB                  |
| `_win32sysloader.pyd`                        | Win32 system loader          |
| `apsw.pyd`                                   | SQLite (Another Python SQLite Wrapper) |
| `bz2.pyd`                                    | bzip2 compression            |
| `pyexpat.pyd`                                | XML (expat)                  |
| `select.pyd`                                 | I/O multiplexing             |
| `sip.pyd`                                    | SIP (PyQt5 binding)          |
| `unicodedata.pyd`                            | Unicode DB                   |
| `psutil._psutil_windows.pyd`                 | Process/system utilities     |
| `win32api.pyd`                               | Win32 API                    |
| `win32file.pyd`                              | Win32 file operations        |
| `win32pipe.pyd`                              | Named pipes                  |
| `win32trace.pyd`                             | Win32 tracing                |
| `win32ui.pyd`                                | Win32 UI (MFC)               |
| `win32wnet.pyd`                              | Win32 networking             |
| `win32com.shell.shell.pyd`                   | Shell COM                    |
| `win32cred.pyd`                              | Windows Credential Manager   |
| `win32event.pyd`                             | Win32 events                 |
| `win32evtlog.pyd`                            | Windows Event Log            |
| `win32gui.pyd`                               | Win32 GUI                    |
| `win32process.pyd`                           | Win32 process mgmt           |
| `win32security.pyd`                          | Win32 security               |
| `winxpgui.pyd`                               | Win32 XP GUI extensions      |
| `Crypto.Cipher._AES.pyd`                     | PyCrypto AES                 |
| `Crypto.Cipher._ARC4.pyd`                    | PyCrypto RC4                 |
| `Crypto.Cipher._Blowfish.pyd`                | PyCrypto Blowfish            |
| `Crypto.Cipher._DES.pyd`                     | PyCrypto DES                 |
| `Crypto.Cipher._DES3.pyd`                    | PyCrypto 3DES                |
| `Crypto.Hash._SHA256.pyd`                    | PyCrypto SHA256              |
| `Crypto.Random.OSRNG.winrandom.pyd`          | PyCrypto Windows RNG         |
| `Crypto.Util.strxor.pyd`                     | PyCrypto XOR utility         |
| `Crypto.Util._counter.pyd`                   | PyCrypto counter mode        |
| `cryptography.hazmat.bindings._constant_time.pyd` | pyca/cryptography       |
| `cryptography.hazmat.bindings._openssl.pyd`  | pyca/cryptography OpenSSL    |

### UI Framework (misc.tar.gz, 34 MB)

The UI is rendered via **PyQt5** with **QtQuick/QML**:

- `PyQt5.QtCore.pyd`, `PyQt5.QtGui.pyd`, `PyQt5.QtWidgets.pyd`
- `PyQt5.QtWebKit.pyd`, `PyQt5.QtWebKitWidgets.pyd` (embedded browser)
- `PyQt5.QtQml.pyd`, `PyQt5.QtQuick.pyd` (QML UI)
- Qt5 libraries: `Qt5Core.dll`, `Qt5Gui.dll`, `Qt5Widgets.dll`, `Qt5WebKit.dll`, etc.
- ICU libraries: `icudt52.dll`, `icuin52.dll`, `icuuc52.dll`
- OpenSSL: `libeay32.dll`, `libssl32.dll`
- Full QML controls: `qml/QtQuick/Controls/`, `qml/QtQuick/Dialogs/`

### HTML Views (embedded in assets/common/views/)

The app uses **QtWebKit** to render HTML-based UI dialogs:

| View                               | Purpose                          |
|------------------------------------|----------------------------------|
| `welcomeViewWin.html`              | Windows onboarding               |
| `welcomeViewMac.html`              | macOS onboarding (shared asset)  |
| `syncView.html`                    | Sync dialog (file download UI)   |
| `alertView.html`                   | Alert/confirmation dialogs       |
| `addExternalMount.html`            | Add external mount point         |
| `enterEncryptionPasswordView.html` | Encryption passphrase entry      |
| `enterEncryptorPasswordView.html`  | Encryptor passphrase entry       |
| `initializeEncryptionPasswordView.html` | Create encryption passphrase |
| `initializeEncryptorPasswordView.html`  | Create encryptor passphrase  |

HTML views use jQuery 2.0.3, Bootstrap 3.0.2, and Underscore.js, communicating with
the Python backend via `WindowController.async_call()` and `WindowController.sync_call()`.

---

## 5. File Associations & Custom Extensions

From OdriveOpen.exe (.NET decompilation), the application registers handlers for:

### Production Extensions

| Extension   | Purpose                           |
|-------------|-----------------------------------|
| `.cloud`    | Cloud file placeholder            |
| `.cloudf`   | Cloud folder placeholder          |
| `.gdocx`    | Google Docs placeholder           |
| `.gsheetx`  | Google Sheets placeholder         |
| `.gformx`   | Google Forms placeholder          |
| `.gslidesx` | Google Slides placeholder         |
| `.gdrawx`   | Google Drawings placeholder       |
| `.gmapx`    | Google Maps placeholder           |
| `.onotex`   | OneNote notebook placeholder      |
| `.lockd`    | Locked file (encrypted)           |
| `.cloudl`   | Locked cloud file                 |
| `.cloudfl`  | Locked cloud folder               |
| `.lockdr`   | Encrypted-name locked file        |
| `.cloudlr`  | Encrypted-name locked cloud file  |
| `.cloudflr` | Encrypted-name locked cloud folder|
| `.space`    | Space (mount point) file          |

All extensions also have `-dev` and `-beta` variants.

---

## 6. System Tray Menu Structure

From the .NET decompilation, the tray context menu builds dynamically:

1. Chat with agent (if available)
2. ---separator---
3. Syncing status submenu
4. Backup status submenu
5. ---separator---
6. Waiting items submenu
7. Not-allowed items submenu
8. Trash submenu
9. ---separator---
10. Major update notification (if available)
11. Open odrive folder
12. Manage links
13. Usage guide
14. Forum announcements
15. ---separator---
16. Premium/upgrade account
17. Refresh subscriptions
18. Move odrive folder
19. Placeholder threshold submenu
20. Auto unsync threshold submenu
21. Throttling threshold submenu
22. XL file threshold submenu
23. Auto trash threshold submenu
24. Pro sync submenu
25. Backup jobs submenu
26. Authorized user submenu / Sign in
27. ---separator---
28. Product version
29. Detailed odrive status
30. Send diagnostics
31. Exit
32. ---separator--- (dev only)
33. App state (dev only)
34. BG sync prefs (dev only)

Tray refreshes on a timer (`TRAY_REFRESH_INTERVAL`).

---

## 7. Installation Layout

### app.manifest (YAML)

```yaml
launchPath: bin/7513/odriveapp.exe
bundles:
  - name: app.tar.gz       # main application (23 MB)
    destination: ''
    miscExtractionPoint: bin/7513/
  - name: common.tar.gz    # shared .NET helpers (39 KB)
    destination: common/bin
miscBundle:
  name: misc.tar.gz         # PyQt5/Qt5 runtime (34 MB)
  destination: common/misc
  noExtract: true            # extracted on demand
```

### Directory Structure (installed)

```
%ProgramFiles%\odrive\ (or %ProgramFiles(x86)%)
  bin/7513/
    odriveapp.exe           # PyInstaller main app
    odrive.exe              # .NET tray UI
    OdriveExplorerHelper.exe # native COM registrar
    python27.dll            # Python 2.7 runtime
    pythoncom27.dll         # Python COM support
    pywintypes27.dll        # Python Win32 types
    *.pyd                   # Python extension modules
    SyncedOverlay.dll       # x86 overlay handler
    ActiveOverlay.dll       # x86 overlay handler
    LockedOverlay.dll       # x86 overlay handler
    ContextMenu.dll         # x86 context menu handler
    x64/                    # x64 builds of all shell extensions
      SyncedOverlay.dll
      ActiveOverlay.dll
      LockedOverlay.dll
      ContextMenu.dll
      python27.dll
      pythoncom27.dll
      ...
    cli/
      odrive.py             # CLI script (cross-platform)
    assets/
      *.ico                 # File type icons
      *.png                 # Tray icons (black/white/pink variants)
      common/
        cacert.pem           # CA certificates
        odrive.png           # App icon
        views/               # HTML/CSS/JS UI views
    requests/
      cacert.pem             # CA certificates (for requests lib)
    include/
      pyconfig.h             # Python config header
  common/
    bin/
      OdriveOpen.exe          # File handler (.NET)
      CU_Uninstall.ps1        # Per-user cleanup script
    misc/
      misc.tar.gz             # PyQt5/Qt5 (34 MB, lazy extract)
```

### User Data

```
%USERPROFILE%\.odrive\
  .oreg                     # IPC port registry (JSON)
%USERPROFILE%\odrive\       # sync root folder
%USERPROFILE%\Links\odrive.lnk  # Explorer favorites shortcut
```

---

## 8. Uninstall Cleanup (CU_Uninstall.ps1)

The PowerShell script handles per-user cleanup:

1. Remove HKCU registry keys: `Software\odrive*`, `Software\Classes\odrive.*`
2. Remove file association keys: `.cloud*`, `.lockd*`, `.gdocx`, `.space`, etc.
3. Remove COM CLSIDs for overlay handlers and context menu
4. Remove Explorer namespace extension
5. Remove SyncRootManager entries (Windows 10 Cloud Files API)
6. Kill `odriveapp` and `odrive` processes
7. Restart Explorer (to unload shell extensions)
8. Remove Explorer shortcut
9. Rename `%USERPROFILE%\odrive` to `odrive_backup_<timestamp>`
10. Remove `%USERPROFILE%\.odrive`

---

## 9. Windows-Specific Features (vs. Linux Agent)

| Feature                        | Windows                                      | Linux              |
|--------------------------------|----------------------------------------------|--------------------|
| **UI framework**               | .NET WinForms tray + PyQt5/QtWebKit dialogs  | None (headless)    |
| **File overlays**              | COM shell extensions (4 overlay DLLs)        | None               |
| **Context menu**               | COM shell extension (ContextMenu.dll)        | None               |
| **Explorer namespace**         | Virtual folder in Explorer nav pane          | None               |
| **File handler**               | OdriveOpen.exe (.NET) handles .cloud clicks  | None               |
| **System tray**                | WinForms NotifyIcon with rich context menu   | None               |
| **Credential storage**         | win32cred.pyd (Windows Credential Manager)   | N/A                |
| **FS events**                  | win32file.pyd (ReadDirectoryChangesW)        | inotify            |
| **Process management**         | win32process.pyd                              | os/signal          |
| **Event logging**              | win32evtlog.pyd (Windows Event Log)          | syslog             |
| **Auto-start**                 | Registry RunOnce / startup                   | N/A                |
| **Windows 10 sync root**       | SyncRootManager (Cloud Files API)            | N/A                |
| **Installer**                  | WiX Burn bootstrapper + MSI                  | tar.gz             |
| **Update mechanism**           | MSI upgrade via WiX                          | In-app download    |

---

## 10. Key Findings for tummycrypt

1. **Same core engine**: The Python 2.7 sync engine is shared across platforms. The Linux
   agent and Windows desktop use the same `src.*` module hierarchy, confirming a single
   codebase with platform-specific adapters.

2. **IPC protocol is simple**: JSON over TCP on localhost, command enum is numeric.
   The full command list (93 commands) reveals all user-facing operations. This is the
   same protocol the CLI (`odrive.py`) uses.

3. **Shell extensions are Python-COM bridges**: The overlay/context menu DLLs are
   lightweight C++ wrappers that load Python 2.7 and execute .pyo bytecode. They
   communicate with the running `odriveapp.exe` process.

4. **QtWebKit for rich dialogs**: Sync configuration, encryption password entry, and
   backup management use HTML/JS rendered in QtWebKit. This explains the large misc.tar.gz.

5. **Multiple product variants**: dev, beta, and prod -- each with separate file extensions
   and registry keys. The product variant is determined by the executable filename.

6. **Windows Cloud Files API**: The uninstall script references `SyncRootManager`,
   indicating at least partial integration with the Windows 10 Cloud Files API
   (placeholder files managed by the OS).
