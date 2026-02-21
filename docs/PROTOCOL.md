# tcfs Protocol Specification

## Stub File Formats

tcfs uses lightweight stub files to represent remote content locally. Stubs are small metadata files that trigger on-demand hydration when accessed.

### `.tc` File Stub (single file)

A `.tc` stub replaces a single file (e.g., `photo.jpg` becomes `photo.jpg.tc`). The stub is a JSON file containing the metadata needed to reconstruct the original file.

```json
{
  "version": 1,
  "file_id": "<BLAKE3 hash of original file, hex>",
  "original_name": "photo.jpg",
  "original_size": 4194304,
  "mime_type": "image/jpeg",
  "modified_at": "2026-02-20T12:00:00Z",
  "chunk_count": 3,
  "manifest_key": "chunks/manifests/<file_id>",
  "remote_prefix": "tcfs/default"
}
```

**Fields:**

| Field | Type | Description |
|-------|------|-------------|
| `version` | u32 | Stub format version (currently 1) |
| `file_id` | string | BLAKE3 hash of the original file content |
| `original_name` | string | Original filename without `.tc` extension |
| `original_size` | u64 | Original file size in bytes |
| `mime_type` | string | MIME type (optional, for display hints) |
| `modified_at` | string | ISO 8601 timestamp of last modification |
| `chunk_count` | u32 | Number of chunks in the manifest |
| `manifest_key` | string | S3 key path to the chunk manifest |
| `remote_prefix` | string | S3 prefix where chunks are stored |

### `.tcf` Folder Stub (directory placeholder)

A `.tcf` stub represents an unsynced directory. It is a JSON file listing the directory's remote contents without downloading them.

```json
{
  "version": 1,
  "dir_id": "<BLAKE3 hash of directory listing>",
  "original_name": "photos",
  "entry_count": 47,
  "total_size": 1073741824,
  "remote_prefix": "tcfs/default/photos"
}
```

## Chunk Layout (S3/SeaweedFS)

Content-addressed storage using BLAKE3 hashes as keys:

```
{prefix}/
├── chunks/
│   ├── {blake3_hash_1}     # Compressed chunk data
│   ├── {blake3_hash_2}
│   └── ...
└── manifests/
    └── {file_blake3_hash}  # Chunk manifest (JSON)
```

### Chunk Manifest Format

```json
{
  "version": 1,
  "file_hash": "<BLAKE3 hex of original file>",
  "file_size": 4194304,
  "chunk_count": 3,
  "chunks": [
    {
      "hash": "<BLAKE3 hex of chunk data>",
      "offset": 0,
      "length": 1398101,
      "compressed_length": 1205432
    },
    {
      "hash": "<BLAKE3 hex of chunk data>",
      "offset": 1398101,
      "length": 1398101,
      "compressed_length": 1189744
    },
    {
      "hash": "<BLAKE3 hex of chunk data>",
      "offset": 2796202,
      "length": 1398102,
      "compressed_length": 1201003
    }
  ]
}
```

## Chunking Algorithm

tcfs uses **FastCDC** (Fast Content-Defined Chunking) for splitting files into variable-size chunks:

- Minimum chunk size: 2 KiB
- Average chunk size: 8 KiB
- Maximum chunk size: 16 KiB
- Hash: BLAKE3
- Compression: zstd (level 3)

Content-defined chunking ensures that inserting or modifying bytes in a file only affects the chunks near the edit point. Unmodified regions produce identical chunks, enabling efficient delta sync.

## Hydration Flow

When a FUSE-mounted `.tc` stub is opened:

1. FUSE `open()` intercepts the request
2. Manifest is fetched from `{prefix}/manifests/{file_hash}`
3. Chunks are fetched in parallel from `{prefix}/chunks/{chunk_hash}`
4. Each chunk is decompressed (zstd)
5. Chunks are concatenated in order to reconstruct the original file
6. File is served to the calling process via FUSE `read()`

## State Tracking

The sync engine maintains a local state cache (JSON) tracking:

- Per-file: local path, remote key, BLAKE3 hash, size, sync status
- Sync states: `synced`, `modified`, `pending_upload`, `pending_download`, `conflict`

## Wire Protocol (gRPC)

The daemon (`tcfsd`) exposes a gRPC service over a Unix domain socket:

```protobuf
service TcfsService {
  rpc GetStatus(StatusRequest) returns (StatusResponse);
  rpc Push(PushRequest) returns (PushResponse);
  rpc Pull(PullRequest) returns (PullResponse);
  rpc Mount(MountRequest) returns (MountResponse);
  rpc Unmount(UnmountRequest) returns (UnmountResponse);
}
```

See `crates/tcfs-core/proto/tcfs.proto` for the full service definition.
