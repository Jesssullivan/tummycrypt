//! tcfs-cloudfilter: Windows Cloud Files API (CFAPI) provider
//!
//! Provides native Windows Explorer integration for tcfs:
//! - Sync root registration (Files show in Explorer with cloud status icons)
//! - Placeholder creation (dehydrated .tc stubs as CFAPI placeholders)
//! - On-demand hydration callbacks (fetch from SeaweedFS when file is opened)
//! - Dehydration (convert back to placeholder, reclaim disk space)
//!
//! This crate maps tcfs concepts to Windows CFAPI:
//!   .tc stub file    → CFAPI placeholder (cloud-only, dehydrated)
//!   hydrated file    → CFAPI in-sync (locally available)
//!   unsync           → CFAPI dehydrate
//!   push             → CFAPI upload
//!
//! ## Architecture
//!
//! ```text
//! Windows Explorer
//!       │
//!       ├── CloudFiles minifilter driver (cflt.sys)
//!       │         │
//!       │         ├── CfConnectSyncRoot()    → registers tcfs as provider
//!       │         ├── CF_CALLBACK_TYPE_FETCH_DATA → hydration request
//!       │         └── CF_CALLBACK_TYPE_CANCEL_FETCH_DATA → cancel
//!       │
//!       └── tcfs-cloudfilter (this crate)
//!                 │
//!                 ├── SyncRootProvider   → manages sync root lifecycle
//!                 ├── PlaceholderManager → create/update placeholders
//!                 └── HydrationHandler   → fetch chunks from SeaweedFS
//! ```
//!
//! ## Platform
//!
//! This crate only compiles on Windows 10 1809+ (build 17763).
//! On non-Windows platforms, it exports nothing.

// ── Platform-independent types (available on all platforms for type checking) ──

/// Configuration for the Cloud Files sync root.
#[derive(Debug, Clone)]
pub struct SyncRootConfig {
    /// Display name shown in Explorer navigation pane (e.g. "TummyCrypt")
    pub display_name: String,
    /// Local path to register as sync root (e.g. `C:\Users\jess\tcfs`)
    pub root_path: std::path::PathBuf,
    /// Provider name (e.g. "tummycrypt")
    pub provider_name: String,
    /// Provider version string
    pub provider_version: String,
    /// SeaweedFS S3 endpoint
    pub s3_endpoint: String,
    /// S3 bucket
    pub s3_bucket: String,
    /// Remote prefix within the bucket
    pub remote_prefix: String,
    /// Hydration policy
    pub hydration_policy: HydrationPolicy,
    /// Population policy
    pub population_policy: PopulationPolicy,
}

/// Controls when files are hydrated (downloaded).
#[derive(Debug, Clone, Copy, Default)]
pub enum HydrationPolicy {
    /// Files are hydrated only when explicitly opened by the user or an app
    #[default]
    Full,
    /// Files are progressively hydrated (streaming)
    Progressive,
    /// Files are always kept fully hydrated locally
    AlwaysLocal,
}

/// Controls how placeholders are populated in the sync root.
#[derive(Debug, Clone, Copy, Default)]
pub enum PopulationPolicy {
    /// Populate the full namespace on sync root connect
    #[default]
    Full,
    /// Populate lazily as directories are enumerated
    Lazy,
}

/// Placeholder file metadata for CFAPI registration.
#[derive(Debug, Clone)]
pub struct PlaceholderInfo {
    /// Relative path within the sync root
    pub relative_path: std::path::PathBuf,
    /// File size in bytes (shown in Explorer even when dehydrated)
    pub file_size: u64,
    /// Last modified timestamp
    pub modified: std::time::SystemTime,
    /// BLAKE3 hash of the file content (stored as file identity)
    pub content_hash: String,
    /// Manifest path for hydration
    pub manifest_path: String,
    /// Whether this is a directory placeholder
    pub is_directory: bool,
}

// ── Windows implementation ──────────────────────────────────────────────────

#[cfg(target_os = "windows")]
pub mod provider;

#[cfg(target_os = "windows")]
pub mod placeholder;

#[cfg(target_os = "windows")]
pub mod hydration;

// ── Non-Windows stub ────────────────────────────────────────────────────────

#[cfg(not(target_os = "windows"))]
mod stub {
    use super::*;

    /// Register a sync root with Windows Cloud Files API.
    /// On non-Windows platforms, this is a no-op that returns an error.
    pub async fn register_sync_root(_config: &SyncRootConfig) -> anyhow::Result<()> {
        anyhow::bail!("Cloud Files API is only available on Windows 10 1809+")
    }

    /// Unregister the sync root.
    pub async fn unregister_sync_root(_root_path: &std::path::Path) -> anyhow::Result<()> {
        anyhow::bail!("Cloud Files API is only available on Windows 10 1809+")
    }
}

#[cfg(not(target_os = "windows"))]
pub use stub::*;
