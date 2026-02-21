use serde::{Deserialize, Serialize};

/// State of a file in the sync system
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileState {
    /// Remote-only stub (.tc file, not downloaded)
    Stub,
    /// Partially downloaded (sparse file in cache)
    Partial,
    /// Fully downloaded and available locally
    Hydrated,
    /// Local change pending upload
    PendingUpload,
    /// Sync in progress
    Syncing,
    /// Conflict requiring resolution
    Conflict,
}

/// Metadata stored in a .tc stub file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StubMeta {
    pub version: String,
    pub chunks: u64,
    pub compressed: bool,
    pub fetched: bool,
    pub oid: String,
    pub origin: String,
    pub size: u64,
}

/// A sync task dispatched to NATS JetStream
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncTask {
    pub task_id: String,
    pub local_path: String,
    pub remote_path: String,
    pub direction: SyncDirection,
    pub priority: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SyncDirection {
    Upload,
    Download,
    Bidirectional,
}

/// A content-addressed chunk reference
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkRef {
    pub index: u64,
    pub blake3: String,
    pub offset: u64,
    pub length: u64,
    pub compressed_length: u64,
}

/// Mount specification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountSpec {
    pub remote: String,
    pub mountpoint: String,
    pub read_only: bool,
    pub options: Vec<String>,
}
