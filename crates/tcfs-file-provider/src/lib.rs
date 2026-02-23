//! tcfs-file-provider: UniFFI bridge for iOS/macOS File Provider extensions
//!
//! This crate exposes tcfs storage, chunking, and sync operations
//! as a C-compatible FFI layer via Mozilla UniFFI, enabling Swift
//! and Kotlin consumers to build native file provider extensions.
//!
//! ## Architecture
//!
//! ```text
//! iOS Files App / macOS Finder
//!       │
//!       ├── NSFileProviderExtension (Swift)
//!       │         │
//!       │         └── UniFFI C bridge
//!       │                   │
//!       └── tcfs-file-provider (this crate)
//!                   │
//!                   ├── tcfs-storage  → S3/SeaweedFS access
//!                   ├── tcfs-chunks   → FastCDC + BLAKE3 + zstd
//!                   ├── tcfs-sync     → state cache, vector clocks
//!                   └── tcfs-core     → config, proto types
//! ```
//!
//! ## Status
//!
//! Skeleton — see [RFC 0003](../../docs/rfc/0003-ios-file-provider.md) for
//! implementation roadmap.

// UniFFI scaffolding will be generated here when implementation begins (Phase 7b)
// uniffi::setup_scaffolding!();

/// Provider configuration passed from the Swift layer.
#[derive(Debug, Clone)]
pub struct ProviderConfig {
    pub s3_endpoint: String,
    pub s3_bucket: String,
    pub access_key: String,
    pub secret_key: String,
    pub remote_prefix: String,
    pub encryption_key: Option<String>,
}

/// A file item returned by directory enumeration.
#[derive(Debug, Clone)]
pub struct FileItem {
    pub item_id: String,
    pub filename: String,
    pub file_size: u64,
    pub modified_timestamp: i64,
    pub is_directory: bool,
    pub content_hash: String,
}

/// Sync status reported to the Swift layer.
#[derive(Debug, Clone)]
pub struct SyncStatus {
    pub connected: bool,
    pub files_synced: u64,
    pub files_pending: u64,
    pub last_error: Option<String>,
}

/// Errors returned to the Swift layer via UniFFI.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("storage error: {0}")]
    Storage(String),
    #[error("decryption error: {0}")]
    Decryption(String),
    #[error("network error: {0}")]
    Network(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("permission denied: {0}")]
    PermissionDenied(String),
}
