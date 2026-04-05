# odrive CLI-to-Agent IPC Protocol Specification

Source: `odrive-cleanroom-re/odrive.py` (1632 lines, official odrive CLI client)

---

## 1. Connection Setup

### 1.1 Port Discovery

The CLI discovers the agent's TCP port by reading a **registry file** (`.oreg`):

| Component | Registry Path |
|-----------|---------------|
| Agent (headless) | `~/.odrive-agent/.oreg` |
| Desktop (GUI) | `~/.odrive/.oreg` |

On Windows, `~` is resolved via `SHGetKnownFolderPath(FOLDERID_Profile)` or
`SHGetSpecialFolderPathW(CSIDL_PROFILE)`, not `os.path.expanduser`.

The `.oreg` file is JSON with this structure:

```json
{
  "current": {
    "protocol": <port_number>
  }
}
```

The key `"protocol"` is extracted from `data["current"]["protocol"]`.

### 1.2 Connection Precedence

The CLI tries the **agent port first**, then the **desktop port**. If both are
available, agent wins. If neither is reachable, the CLI exits with:

> "You must have odrive agent or desktop running before using this script."

### 1.3 TCP Socket

- Transport: **TCP over IPv4** (`AF_INET`, `SOCK_STREAM`)
- Host: `127.0.0.1` (loopback only -- no remote access)
- Connect timeout: **100ms** (`sock.settimeout(0.1)`)
  - Rationale: since agent is tried first, a fast timeout prevents 1s+ delay
    when the agent is down and only the desktop is running.
- After connect: timeout reset to `None` (blocking) for command exchange.

### 1.4 Signal Handling

For synchronous commands and streaming, the CLI installs `SIGINT` and `SIGTERM`
handlers that close the socket and call `sys.exit(0)`, ensuring clean shutdown
on Ctrl+C.

---

## 2. Message Format

### 2.1 Request Format (CLI -> Agent)

Every request is a single **newline-delimited JSON** line:

```
<json_object>\n
```

Encoded as UTF-8. The JSON structure is always:

```json
{
  "command": "<command_name>",
  "parameters": {
    "<key>": "<value>",
    ...
  }
}
```

The `parameters` object may be empty (`{}`) for parameterless commands.

### 2.2 Response Format (Agent -> CLI)

Responses are also **newline-delimited JSON** lines, potentially multiple:

```
<json_line_1>\n
<json_line_2>\n
...
```

Each line is:

```json
{
  "messageType": "<type>",
  "message": <value>
}
```

Where `messageType` is one of:

| messageType | Meaning |
|-------------|---------|
| `"Status"` | Informational/progress message |
| `"Error"` | Error message |

The `message` field can be:
- A **string** (for sync progress, error text, etc.)
- A **JSON object** (for status, syncstate, refresh responses)

### 2.3 Response Streaming

The CLI reads in a loop with `sock.recv(1048576)` (1 MB chunks), buffering
until `\n` delimiters are found. Multiple response lines may arrive in a single
TCP segment. The connection is considered closed when `recv()` returns empty
bytes.

### 2.4 Binary Streaming (stream/streamremote only)

The `stream` and `streamremote` commands use a different protocol after the
initial JSON request:

- Request: standard JSON + newline (as above)
- Response: **raw binary data** in 256 KB chunks
- No JSON framing, no message types -- pure byte stream
- Written directly to stdout (binary mode)
- Connection close signals end-of-stream

---

## 3. Command Categories

### 3.1 Fire-and-Forget Commands (OdriveCommand)

These send the JSON request, do **not** read any response, and close the socket.

| Command | Description |
|---------|-------------|
| `deauthorize` | Unlink user and exit agent |
| `shutdown` | Shut down the agent |
| `xlthreshold` | Set XL file split threshold |
| `autounsyncthreshold` | Set auto-unsync time threshold |
| `autotrashthreshold` | Set auto-trash interval |
| `placeholderthreshold` | Set auto-download size threshold |
| `foldersyncrule` | Set per-folder sync rules |

### 3.2 Synchronous Commands (OdriveSynchronousCommand)

These send the request and then **read one or more JSON response lines**,
printing status/error messages to stdout/stderr respectively.

| Command | Description |
|---------|-------------|
| `authenticate` | Submit auth key |
| `diagnostics` | Generate diagnostic report |
| `sync` | Sync a single placeholder |
| `refresh` | Refresh a folder |
| `unsync` | Unsync a file/folder |
| `forceunsync` | Force-unsync (destroys unuploaded changes) |
| `syncstate` | Query sync state of a path |
| `status` | Get full agent status |
| `mount` | Mount remote path to local folder |
| `unmount` | Remove a mount |
| `backup` | Create a backup job |
| `removebackup` | Remove a backup job |
| `backupnow` | Trigger immediate backup |
| `encpassphrase` | Set/initialize Encryptor passphrase |
| `emptytrash` | Empty the odrive trash |
| `restoretrash` | Restore all trashed items |

### 3.3 Streaming Commands

| Command | Description |
|---------|-------------|
| `stream` | Stream a local placeholder file's content |
| `streamremote` | Stream a remote file by path |

### 3.4 Client-Side Composite Commands

| Command | Description |
|---------|-------------|
| `sync --recursive` | Walk directory tree, sync all placeholders |

This is **not** a single agent command. The CLI walks the local filesystem and
issues individual `sync` commands for each `.cloud`/`.cloudf` placeholder found.

---

## 4. Placeholder System

### 4.1 File Extensions

| Extension | Type | Meaning |
|-----------|------|---------|
| `.cloud` | File placeholder | Remote file not yet downloaded |
| `.cloudf` | Folder placeholder | Remote folder not yet expanded |
| `.cloud-dev` | Dev file placeholder | Development variant |
| `.cloudf-dev` | Dev folder placeholder | Development variant |

### 4.2 Placeholder Semantics

- A `.cloud` file represents a remote file. Syncing it downloads the actual
  content and removes the `.cloud` extension.
- A `.cloudf` file represents a remote folder. Syncing it expands it into a
  real directory (the `.cloudf` extension is stripped). The contents of the new
  directory will be populated with further `.cloud`/`.cloudf` placeholders.

### 4.3 Placeholder Threshold

The `placeholderthreshold` command controls automatic downloading:

| Value | Behavior |
|-------|----------|
| `never` | Never auto-download; everything stays as placeholder |
| `small` | Auto-download files <= 10 MB |
| `medium` | Auto-download files <= 100 MB |
| `large` | Auto-download files <= 500 MB |
| `always` | Auto-download everything |

---

## 5. Sync States

### 5.1 Known States

Observed from `SyncState` and `Refresh` command responses:

| State | Color | Meaning |
|-------|-------|---------|
| `Synced` | Cyan | Fully synchronized with remote |
| `Locked` | Cyan | Locked/protected state (treated same as Synced visually) |
| `Active` | Magenta | Currently transferring/processing |
| *(other)* | Default | Any unrecognized state gets no color |

### 5.2 SyncState Response Structure

```json
{
  "messageType": "Status",
  "message": "{\"syncState\": \"Synced\", \"childSyncStates\": {\"file1.txt\": \"Synced\", \"dir/\": \"Active\"}}"
}
```

Note: the `message` field for syncstate/refresh is a **JSON string** that must
be parsed a second time. It contains:
- `syncState`: the state of the queried path itself
- `childSyncStates`: a dict mapping child names to their sync states

---

## 6. Status Response Structure

The `status` command returns a rich JSON object as the `message` field:

```json
{
  "messageType": "Status",
  "message": {
    "isActivated": true,
    "hasSession": true,
    "authorizedEmail": "user@example.com",
    "authorizedAccountSourceType": "odrive",
    "syncEnabled": true,
    "productVersion": "7.x.x",
    "placeholderThreshold": "never",
    "autoUnsyncThreshold": "never",
    "downloadThrottlingThreshold": "unlimited",
    "uploadThrottlingThreshold": "unlimited",
    "autoTrashThreshold": "never",
    "xlFileThreshold": "never",
    "odriveFolder": {
      "path": "/home/user/odrive",
      "status": "Active"
    },
    "proSyncFolders": [
      {"path": "/mnt/data", "status": "Synced"}
    ],
    "backupJobs": [
      {"jobId": "abc123", "localPath": "/backup/src", "remotePath": "/S3/backup", "status": "idle"}
    ],
    "expandRequests": [],
    "syncRequests": [
      {"path": "/path/to/file", "firstPathItem": "file.txt", "percentComplete": 50}
    ],
    "refreshChildOperations": [
      {"name": "folder_name", "percentComplete": 75}
    ],
    "uploads": [
      {"name": "file.txt", "path": "/path/to/file.txt", "percentComplete": 30}
    ],
    "downloads": [
      {"name": "file.txt", "path": "/path/to/file.txt", "percentComplete": 60}
    ],
    "trashItems": [
      {"name": "deleted.txt", "folderPath": "/some/folder"}
    ],
    "waitingItems": [
      {"name": "pending.txt", "folderPath": "/some/folder", "explanation": "Waiting for lock"}
    ],
    "notAllowedItems": [
      {"name": "forbidden.txt", "folderPath": "/some/folder", "explanation": "Permission denied"}
    ]
  }
}
```

The `--mounts`, `--backups`, `--sync_requests`, `--uploads`, `--downloads`,
`--background`, `--trash`, `--waiting`, and `--not_allowed` flags are all
**client-side filters** on this same response. The agent always sends the full
status object; the CLI selects which section to display.

---

## 7. Authentication Flow

1. User obtains an auth key from `https://www.odrive.com`
2. CLI sends: `{"command": "authenticate", "parameters": {"authKey": "<key>"}}`
3. Agent responds with `Status` or `Error` message(s)
4. On success, the agent stores the session internally
5. Deauthorization: `{"command": "deauthorize", "parameters": {}}`
   - Fire-and-forget; no response expected
   - Unlinks the user and exits the agent

---

## 8. Recursive Sync Algorithm

The `sync --recursive` command is implemented entirely client-side:

```
1. If the target path is a placeholder (.cloud/.cloudf/.cloud-dev/.cloudf-dev):
   a. Sync it first (expand folder or download file)
   b. If .cloudf: strip extension to get the new directory path
   c. If .cloud: done (no recursion into files)

2. Walk the directory tree with os.walk():
   a. For each file:
      - If .cloudf/.cloudf-dev: sync it (expand folder)
      - If .cloud/.cloud-dev AND --nodownload is NOT set: sync it (download)
   b. Files are sorted to ensure deterministic traversal order

3. Retry logic:
   a. Track count of remaining unsynced items and the last attempted path
   b. If a pass makes no progress (same count, same last path):
      - Increment retry counter
      - After 5 retries with no progress: exit with error
   c. If progress was made: reset retry counter

4. Async mode (--nowait, hidden/experimental):
   a. Uses SyncAsynchronous (fire-and-forget) instead of Sync
   b. Adds 1s sleep between items to avoid overwhelming the agent
   c. Initial 1s sleep after first sync to let agent process
```

### 8.1 Recursive Sync Modes

| Flag | Effect |
|------|--------|
| `--recursive` | Enable recursive sync |
| `--nodownload` | Only expand folders (.cloudf); skip file downloads (.cloud) |
| `--nowait` | Hidden/experimental; async fire-and-forget sync commands |

---

## 9. Mount and Backup System

### 9.1 Mounts (Pro Feature)

```json
{"command": "mount", "parameters": {"localPath": "/abs/path", "remotePath": "/Google Drive/Pics"}}
{"command": "unmount", "parameters": {"localPath": "/abs/path"}}
```

- `localPath` is always resolved to an absolute path by the CLI
- `remotePath` uses `/` for the odrive root (all linked storage)
- Mount status is reported in the `proSyncFolders` array of the status response
- The legacy `odriveFolder` is a single default mount

### 9.2 Backups

```json
{"command": "backup", "parameters": {"localPath": "/abs/path", "remotePath": "/S3/backup"}}
{"command": "removebackup", "parameters": {"backupId": "<id>"}}
{"command": "backupnow", "parameters": {}}
```

- Backup jobs have a `jobId` assigned by the agent
- `backupnow` triggers all configured backup jobs immediately

---

## 10. Threshold/Configuration Commands

These are fire-and-forget commands that modify agent behavior:

### 10.1 XL File Threshold

Controls file splitting for large uploads:

| Value | Size |
|-------|------|
| `never` | Never split |
| `small` | 100 MB |
| `medium` | 500 MB |
| `large` | 1 GB |
| `xlarge` | 2 GB |

### 10.2 Auto-Unsync Threshold

Automatically unsync files not modified within a time window:

| Value | Period |
|-------|--------|
| `never` | Disabled |
| `day` | Daily |
| `week` | Weekly |
| `month` | Monthly |

### 10.3 Auto-Trash Threshold

Automatically empty the odrive trash:

| Value | Interval |
|-------|----------|
| `never` | Disabled |
| `immediately` | Immediately |
| `fifteen` | Every 15 minutes |
| `hour` | Hourly |
| `day` | Daily |

### 10.4 Folder Sync Rule

Per-folder rule for automatic content downloading:

```json
{
  "command": "foldersyncrule",
  "parameters": {
    "path": "/abs/folder/path",
    "threshold": "100",
    "expandsubfolders": true
  }
}
```

- `threshold`: size in MB (base 10). `"0"` = nothing, `"inf"` = infinite
- `expandsubfolders`: boolean; apply rule to all descendant folders
- Client-side validation checks path exists and is a directory before sending

---

## 11. Encryptor Support

```json
{
  "command": "encpassphrase",
  "parameters": {
    "passphrase": "my secret passphrase",
    "id": "encryptor-id-123",
    "initialize": false
  }
}
```

- `id`: identifies which Encryptor folder
- `initialize`: `true` to create a new passphrase; `false` to unlock existing
- This is a synchronous command; agent responds with Status/Error

---

## 12. Error Handling

### 12.1 Connection Errors

| Condition | Behavior |
|-----------|----------|
| No .oreg file found | Exit with "must have odrive agent or desktop running" |
| Both ports unreachable | Same exit message |
| Socket connect fails | Silently falls through to next port |
| Send fails | Print exception, return False |

### 12.2 Command Errors

- The agent sends `{"messageType": "Error", "message": "..."}` response lines
- Error messages are written to stderr
- Status messages are written to stdout

### 12.3 Recursive Sync Errors

- If `command.execute()` returns False during recursive walk:
  exit immediately with "There was an error sending the command"
- Stalled progress (5 retries with no change): exit with count of remaining items

---

## 13. Platform-Specific Behaviors

### 13.1 Windows

- Unicode console output via `WriteConsoleW` (ctypes)
- Console color via `SetConsoleTextAttribute`
- Path prefix `\\?\` added for long path support
- `os.O_BINARY` mode for stream output
- Screen clear: `cls` command
- Line clear: spaces + `\r`

### 13.2 macOS / Linux

- ANSI escape codes for color (`\033[96m` cyan, `\033[95m` magenta, `\033[91m` red)
- `tput colors` check on Linux for color support
- Screen clear: `clear` command
- Line clear: `\x1b[2K`
- Paths encoded to UTF-8 bytes for filesystem operations

### 13.3 Python 2/3 Compatibility

- `from __future__ import print_function, unicode_literals`
- `codecs.getwriter/getreader` for Python 2 stdout/stdin
- `unicode = str` alias for Python 3
- `sys.stdout.buffer` for binary stream output in Python 3

---

## 14. Wire Protocol Summary

```
CLI                                     Agent
 |                                        |
 |-- TCP connect to 127.0.0.1:<port> --->|
 |                                        |
 |-- {"command":"X","parameters":{...}}\n |
 |                                        |
 |<-- {"messageType":"Status","message":..}\n
 |<-- {"messageType":"Status","message":..}\n
 |<-- {"messageType":"Error","message":..}\n
 |                                        |
 |<-- [connection closed by agent]        |
 |                                        |

For streaming:
 |-- {"command":"stream","parameters":{..}}\n
 |<-- [raw binary data, 256KB chunks]     |
 |<-- [connection closed = EOF]           |
```

Each CLI invocation opens a **new TCP connection**, sends exactly **one
command**, reads all responses, and closes. There is no session multiplexing
or command pipelining.
