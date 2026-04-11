//! tcfs-vfs: platform-agnostic virtual filesystem trait and shared infrastructure.
//!
//! This crate provides:
//! - `VirtualFilesystem` trait — async filesystem operations for any backend
//! - `TcfsVfs` — concrete implementation mapping SeaweedFS index to virtual tree
//! - `DiskCache` — LRU disk cache for hydrated file content
//! - `NegativeCache` — TTL-based negative dentry cache
//! - Stub file formats (`.tc`, `.tcf`) and index entry parsing
//! - Content hydration from manifest + chunks
//!
//! ## Backend implementations
//!
//! | Backend | Crate | Protocol |
//! |---------|-------|----------|
//! | NFS | `tcfs-nfs` | NFSv3 RPC -> VirtualFilesystem |
//! | FileProvider | `tcfs-file-provider` | Apple FileProvider -> direct S3 |

pub mod cache;
pub mod driver;
pub mod hydrate;
pub mod negative_cache;
pub mod stub;
pub mod types;
pub mod vfs;

// ── Public re-exports ────────────────────────────────────────────────────────

// Trait
pub use vfs::VirtualFilesystem;

// Concrete implementation
pub use driver::{OnFlushCallback, TcfsVfs, UnsyncResult};

// Types
pub use types::{VfsAttr, VfsDirEntry, VfsFileType, VfsStatFs};

// Disk cache
pub use cache::{cache_key_for_path, CacheStats, DiskCache};

// Negative cache
pub use negative_cache::NegativeCache;

// Stub file types
pub use stub::{is_stub_path, real_to_stub_name, stub_to_real_name, IndexEntry, StubMeta};

// Hydration
pub use hydrate::{fetch_cached, fetch_content};

// Shared master key type for daemon → VFS propagation
pub type SharedMasterKey = std::sync::Arc<tokio::sync::Mutex<Option<tcfs_crypto::MasterKey>>>;
