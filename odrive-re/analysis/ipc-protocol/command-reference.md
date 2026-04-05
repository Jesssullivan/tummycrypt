# odrive IPC Command Reference

Every command is sent as a JSON object over TCP to `127.0.0.1:<port>`:

```json
{"command": "<name>", "parameters": {<params>}}
```

Terminated with `\n`. Responses (when applicable) are newline-delimited JSON:

```json
{"messageType": "Status"|"Error", "message": <string_or_object>}
```

---

## authenticate

Authenticate the agent with an auth key obtained from odrive.com.

**Type:** Synchronous (reads response)

```json
{
  "command": "authenticate",
  "parameters": {
    "authKey": "abc123-your-auth-key"
  }
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `authKey` | string | yes | Auth key from odrive.com |

**Response:** Status message on success, Error on failure.

---

## deauthorize

Unlink the current user and exit the agent.

**Type:** Fire-and-forget (no response read)

```json
{
  "command": "deauthorize",
  "parameters": {}
}
```

No parameters.

---

## status

Get comprehensive agent status including session info, queue counts, and config.

**Type:** Synchronous (reads response)

```json
{
  "command": "status",
  "parameters": {}
}
```

No parameters.

**Response:** Single Status message with a JSON object containing:

| Field | Type | Description |
|-------|------|-------------|
| `isActivated` | bool | Whether agent is activated |
| `hasSession` | bool | Whether user session exists |
| `authorizedEmail` | string | Authenticated user email |
| `authorizedAccountSourceType` | string | Account type (e.g. "odrive") |
| `syncEnabled` | bool | Whether sync is enabled |
| `productVersion` | string | Agent version |
| `placeholderThreshold` | string | Auto-download threshold |
| `autoUnsyncThreshold` | string | Auto-unsync time threshold |
| `downloadThrottlingThreshold` | string | Download bandwidth limit |
| `uploadThrottlingThreshold` | string | Upload bandwidth limit |
| `autoTrashThreshold` | string | Auto-trash interval |
| `xlFileThreshold` | string | XL file split threshold |
| `odriveFolder` | object | Default odrive folder `{path, status}` |
| `proSyncFolders` | array | Pro mount folders `[{path, status}]` |
| `backupJobs` | array | Backup jobs `[{jobId, localPath, remotePath, status}]` |
| `expandRequests` | array | Pending folder expansions `[{path, firstPathItem, percentComplete}]` |
| `syncRequests` | array | Pending sync requests `[{path, firstPathItem, percentComplete}]` |
| `refreshChildOperations` | array | Background refresh ops `[{name, percentComplete}]` |
| `uploads` | array | Active uploads `[{name, path, percentComplete}]` |
| `downloads` | array | Active downloads `[{name, path, percentComplete}]` |
| `trashItems` | array | Trashed items `[{name, folderPath}]` |
| `waitingItems` | array | Waiting items `[{name, folderPath, explanation}]` |
| `notAllowedItems` | array | Blocked items `[{name, folderPath, explanation}]` |

---

## syncstate

Query the sync state of a specific file or folder.

**Type:** Synchronous (reads response)

```json
{
  "command": "syncstate",
  "parameters": {
    "path": "/absolute/path/to/file_or_folder"
  }
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `path` | string | yes | Absolute path to query |

**Response:** Status message containing a JSON string that when parsed yields:

```json
{
  "syncState": "Synced",
  "childSyncStates": {
    "child_name": "Synced",
    "other_child": "Active"
  }
}
```

Known sync states: `Synced`, `Locked`, `Active`, and potentially others.

---

## sync

Sync (download/expand) a placeholder file or folder.

**Type:** Synchronous (reads response with progress)

```json
{
  "command": "sync",
  "parameters": {
    "placeholderPath": "/absolute/path/to/file.cloud"
  }
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `placeholderPath` | string | yes | Absolute path to `.cloud` or `.cloudf` placeholder |

**Response:** Multiple Status messages with progress text (overwritten in-place
on TTY), followed by connection close on completion. Error messages on failure.

**Async variant:** Same wire command, but the CLI can choose to not read the
response (used internally by `--nowait` recursive sync).

---

## refresh

Refresh a folder's contents from remote.

**Type:** Synchronous (reads response)

```json
{
  "command": "refresh",
  "parameters": {
    "folderPath": "/absolute/path/to/folder"
  }
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `folderPath` | string | yes | Absolute path to synced folder |

**Response:** Status message with JSON string containing `syncState` and
`childSyncStates` (same format as `syncstate`).

---

## unsync

Convert a synced file/folder back to a placeholder.

**Type:** Synchronous (reads response)

```json
{
  "command": "unsync",
  "parameters": {
    "path": "/absolute/path/to/file_or_folder"
  }
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `path` | string | yes | Absolute path to unsync |

**Response:** Status or Error message.

---

## forceunsync

Force-unsync a file/folder, permanently destroying any unuploaded local changes.

**Type:** Synchronous (reads response)

```json
{
  "command": "forceunsync",
  "parameters": {
    "path": "/absolute/path/to/file_or_folder"
  }
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `path` | string | yes | Absolute path to force-unsync |

**Response:** Status or Error message.

---

## mount

Mount a remote odrive path to a local directory.

**Type:** Synchronous (reads response)

```json
{
  "command": "mount",
  "parameters": {
    "localPath": "/absolute/local/path",
    "remotePath": "/Google Drive/Pictures"
  }
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `localPath` | string | yes | Absolute local directory path |
| `remotePath` | string | yes | Remote path (use `/` for odrive root) |

**Response:** Status or Error message.

---

## unmount

Remove an existing mount.

**Type:** Synchronous (reads response)

```json
{
  "command": "unmount",
  "parameters": {
    "localPath": "/absolute/local/path"
  }
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `localPath` | string | yes | Absolute path of the mount to remove |

**Response:** Status or Error message.

---

## backup

Create a backup job from a local folder to a remote path.

**Type:** Synchronous (reads response)

```json
{
  "command": "backup",
  "parameters": {
    "localPath": "/absolute/local/path",
    "remotePath": "/Amazon Cloud Drive/Backup"
  }
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `localPath` | string | yes | Absolute local directory to back up |
| `remotePath` | string | yes | Remote destination path |

**Response:** Status or Error message.

---

## removebackup

Remove an existing backup job.

**Type:** Synchronous (reads response)

```json
{
  "command": "removebackup",
  "parameters": {
    "backupId": "job-id-string"
  }
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `backupId` | string | yes | Job ID from `status --backups` |

**Response:** Status or Error message.

---

## backupnow

Trigger all configured backup jobs to run immediately.

**Type:** Synchronous (reads response)

```json
{
  "command": "backupnow",
  "parameters": {}
}
```

No parameters.

**Response:** Status or Error message.

---

## stream

Stream the content of a local placeholder file to stdout as raw bytes.

**Type:** Binary stream (no JSON response framing)

```json
{
  "command": "stream",
  "parameters": {
    "path": "/absolute/path/to/file.cloud"
  }
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `path` | string | yes | Absolute path to placeholder file |

**Response:** Raw binary data (256 KB chunks). Connection close = EOF.

---

## streamremote

Stream the content of a remote file by its remote path.

**Type:** Binary stream (no JSON response framing)

```json
{
  "command": "streamremote",
  "parameters": {
    "path": "/Dropbox/movie.mp4"
  }
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `path` | string | yes | Remote path (e.g. `/Dropbox/movie.mp4`) |

**Response:** Raw binary data (256 KB chunks). Connection close = EOF.

---

## diagnostics

Generate a diagnostics report.

**Type:** Synchronous (reads response)

```json
{
  "command": "diagnostics",
  "parameters": {}
}
```

No parameters.

**Response:** Status message(s) with diagnostic information.

---

## emptytrash

Empty the odrive trash, permanently deleting trashed items from remote.

**Type:** Synchronous (reads response)

```json
{
  "command": "emptytrash",
  "parameters": {}
}
```

No parameters.

**Response:** Status or Error message.

---

## restoretrash

Restore all items in the odrive trash.

**Type:** Synchronous (reads response)

```json
{
  "command": "restoretrash",
  "parameters": {}
}
```

No parameters.

**Response:** Status or Error message.

---

## shutdown

Shut down the odrive agent.

**Type:** Fire-and-forget (no response read)

```json
{
  "command": "shutdown",
  "parameters": {}
}
```

No parameters.

---

## xlthreshold

Set the XL file split threshold for large file uploads.

**Type:** Fire-and-forget (no response read)

```json
{
  "command": "xlthreshold",
  "parameters": {
    "threshold": "medium"
  }
}
```

| Parameter | Type | Required | Values |
|-----------|------|----------|--------|
| `threshold` | string | yes | `never`, `small` (100MB), `medium` (500MB), `large` (1GB), `xlarge` (2GB) |

---

## autounsyncthreshold

Set the auto-unsync interval for unmodified files.

**Type:** Fire-and-forget (no response read)

```json
{
  "command": "autounsyncthreshold",
  "parameters": {
    "threshold": "week"
  }
}
```

| Parameter | Type | Required | Values |
|-----------|------|----------|--------|
| `threshold` | string | yes | `never`, `day`, `week`, `month` |

---

## autotrashthreshold

Set the automatic trash emptying interval.

**Type:** Fire-and-forget (no response read)

```json
{
  "command": "autotrashthreshold",
  "parameters": {
    "threshold": "hour"
  }
}
```

| Parameter | Type | Required | Values |
|-----------|------|----------|--------|
| `threshold` | string | yes | `never`, `immediately`, `fifteen`, `hour`, `day` |

---

## placeholderthreshold

Set the automatic file download threshold for syncing/expanding folders.

**Type:** Fire-and-forget (no response read)

```json
{
  "command": "placeholderthreshold",
  "parameters": {
    "threshold": "medium"
  }
}
```

| Parameter | Type | Required | Values |
|-----------|------|----------|--------|
| `threshold` | string | yes | `never`, `small` (10MB), `medium` (100MB), `large` (500MB), `always` |

---

## foldersyncrule

Set a per-folder rule for automatic content syncing.

**Type:** Fire-and-forget (no response read)

```json
{
  "command": "foldersyncrule",
  "parameters": {
    "path": "/absolute/folder/path",
    "threshold": "100",
    "expandsubfolders": true
  }
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `path` | string | yes | Absolute path to the folder |
| `threshold` | string | yes | Size in MB (base 10); `"0"` = nothing, `"inf"` = unlimited |
| `expandsubfolders` | bool | no | Apply rule to all subfolders (default: false) |

**Note:** Client validates that the path exists and is a directory before sending.

---

## encpassphrase

Set or initialize a passphrase for an Encryptor folder.

**Type:** Synchronous (reads response)

```json
{
  "command": "encpassphrase",
  "parameters": {
    "passphrase": "my-secret-passphrase",
    "id": "encryptor-folder-id",
    "initialize": false
  }
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `passphrase` | string | yes | The encryption passphrase |
| `id` | string | yes | Encryptor folder identifier |
| `initialize` | bool | no | `true` to create new; `false` to unlock existing (default: false) |

**Response:** Status or Error message.

---

## Quick Reference Table

| Command | Type | Parameters | CLI Flags |
|---------|------|------------|-----------|
| `authenticate` | sync | `authKey` | `authenticate <key>` |
| `deauthorize` | fire-forget | none | `deauthorize` |
| `status` | sync | none | `status [--mounts\|--backups\|--sync_requests\|--uploads\|--downloads\|--background\|--trash\|--waiting\|--not_allowed]` |
| `syncstate` | sync | `path` | `syncstate <path> [--textonly]` |
| `sync` | sync | `placeholderPath` | `sync <path> [--recursive] [--nodownload] [--nowait]` |
| `refresh` | sync | `folderPath` | `refresh <path>` |
| `unsync` | sync | `path` | `unsync <path>` |
| `forceunsync` | sync | `path` | `unsync <path> --force` |
| `mount` | sync | `localPath`, `remotePath` | `mount <local> <remote>` |
| `unmount` | sync | `localPath` | `unmount <local>` |
| `backup` | sync | `localPath`, `remotePath` | `backup <local> <remote>` |
| `removebackup` | sync | `backupId` | `removebackup <id>` |
| `backupnow` | sync | none | `backupnow` |
| `stream` | binary | `path` | `stream <path>` |
| `streamremote` | binary | `path` | `stream <path> --remote` |
| `diagnostics` | sync | none | `diagnostics` |
| `emptytrash` | sync | none | `emptytrash` |
| `restoretrash` | sync | none | `restoretrash` |
| `shutdown` | fire-forget | none | `shutdown` |
| `xlthreshold` | fire-forget | `threshold` | `xlthreshold <value>` |
| `autounsyncthreshold` | fire-forget | `threshold` | `autounsyncthreshold <value>` |
| `autotrashthreshold` | fire-forget | `threshold` | `autotrashthreshold <value>` |
| `placeholderthreshold` | fire-forget | `threshold` | `placeholderthreshold <value>` |
| `foldersyncrule` | fire-forget | `path`, `threshold`, `expandsubfolders` | `foldersyncrule <path> <threshold> [--expandsubfolders]` |
| `encpassphrase` | sync | `passphrase`, `id`, `initialize` | `encpassphrase <passphrase> <id> [--initialize]` |

Types: **sync** = sends request + reads JSON responses. **fire-forget** = sends request + closes. **binary** = sends JSON request + reads raw byte stream.
