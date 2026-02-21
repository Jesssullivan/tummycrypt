//! Placeholder management: create, update, and dehydrate CFAPI placeholders.
//!
//! CFAPI placeholders are sparse NTFS files with reparse points that:
//! - Show real file sizes in Explorer (even when dehydrated/cloud-only)
//! - Display cloud status icons (cloud, checkmark, pin)
//! - Trigger hydration callbacks when opened
//!
//! Mapping to tcfs concepts:
//!   PlaceholderInfo → .tc stub metadata (size, hash, manifest path)
//!   create_placeholder() → equivalent to creating a .tc stub file
//!   dehydrate() → equivalent to `tcfs unsync` (convert back to stub)
//!   convert_to_placeholder() → mark an existing file as synced + dehydratable

#![cfg(target_os = "windows")]

use anyhow::{Context, Result};
use std::path::Path;
use tracing::{debug, info};

use crate::PlaceholderInfo;

/// Create a new placeholder file in the sync root.
///
/// The file appears in Explorer with the configured size but occupies
/// minimal disk space (cloud-only state). When a user opens it,
/// the CFAPI minifilter triggers a FETCH_DATA callback.
///
/// # Arguments
/// - `sync_root` — path to the registered sync root directory
/// - `info` — placeholder metadata (path, size, hash, manifest)
pub async fn create_placeholder(sync_root: &Path, info: &PlaceholderInfo) -> Result<()> {
    let full_path = sync_root.join(&info.relative_path);

    debug!(
        path = %full_path.display(),
        size = info.file_size,
        hash = %info.content_hash,
        "creating CFAPI placeholder"
    );

    // Ensure parent directory exists
    if let Some(parent) = full_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("creating parent dir: {}", parent.display()))?;
    }

    // TODO: Phase 6c implementation
    // 1. Build CF_PLACEHOLDER_CREATE_INFO struct:
    //    - FileIdentity = content_hash bytes (used in FETCH_DATA callback)
    //    - FsMetadata.FileSize = info.file_size
    //    - FsMetadata.BasicInfo.LastWriteTime = info.modified
    //    - Flags = CF_PLACEHOLDER_CREATE_FLAG_MARK_IN_SYNC
    // 2. Call CfCreatePlaceholders() with the parent directory

    Ok(())
}

/// Create placeholder files for an entire directory tree.
///
/// Scans the remote index and creates a placeholder for each entry.
pub async fn populate_root(
    sync_root: &Path,
    op: &opendal::Operator,
    remote_prefix: &str,
) -> Result<usize> {
    let index_prefix = format!("{}/index/", remote_prefix.trim_end_matches('/'));

    info!(
        root = %sync_root.display(),
        prefix = %index_prefix,
        "populating sync root with placeholders"
    );

    let entries = op
        .list(&index_prefix)
        .await
        .context("listing remote index")?;

    let mut count = 0;
    for entry in entries {
        let rel_path = entry
            .name()
            .strip_prefix(&index_prefix)
            .unwrap_or(entry.name());

        if rel_path.is_empty() || rel_path.ends_with('/') {
            continue; // skip directory markers
        }

        // Read index entry to get size and hash
        let data = match op.read(entry.name()).await {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(path = %entry.name(), "skipping unreadable index entry: {e}");
                continue;
            }
        };

        let text = String::from_utf8_lossy(&data.to_bytes());
        // Parse index entry: "size hash" format
        // TODO: share IndexEntry type from tcfs-fuse into tcfs-core
        let parts: Vec<&str> = text.trim().splitn(2, ' ').collect();
        if parts.len() == 2 {
            if let Ok(size) = parts[0].parse::<u64>() {
                let hash = parts[1].to_string();
                let info = PlaceholderInfo {
                    relative_path: std::path::PathBuf::from(rel_path),
                    file_size: size,
                    modified: std::time::SystemTime::now(),
                    content_hash: hash.clone(),
                    manifest_path: format!(
                        "{}/manifests/{}",
                        remote_prefix.trim_end_matches('/'),
                        hash
                    ),
                    is_directory: false,
                };

                create_placeholder(sync_root, &info).await?;
                count += 1;
            }
        }
    }

    info!(root = %sync_root.display(), count, "populated placeholders");
    Ok(count)
}

/// Dehydrate a file — convert it from locally-available back to cloud-only.
///
/// Equivalent to `tcfs unsync`: the file's content is removed from disk
/// but the placeholder remains, showing the original size in Explorer.
/// Opening the file again triggers re-hydration.
pub async fn dehydrate(file_path: &Path) -> Result<()> {
    info!(path = %file_path.display(), "dehydrating to placeholder");

    // TODO: Phase 6c implementation
    // 1. Open file handle with CF_OPEN_FILE_FLAG_NONE
    // 2. Call CfDehydratePlaceholder() with offset=0, length=file_size
    // 3. Close handle
    //
    // After dehydration, the file appears as "cloud-only" (cloud icon)
    // in Explorer and occupies minimal disk space.

    Ok(())
}

/// Convert an existing local file into a synced placeholder.
///
/// Used after `tcfs push`: the file content is already on SeaweedFS,
/// so we mark it as a placeholder that can be dehydrated later.
pub async fn convert_to_placeholder(file_path: &Path, info: &PlaceholderInfo) -> Result<()> {
    debug!(
        path = %file_path.display(),
        hash = %info.content_hash,
        "converting to CFAPI placeholder"
    );

    // TODO: Phase 6c implementation
    // 1. Call CfConvertToPlaceholder() with file identity = content_hash
    // 2. Mark as in-sync: CfSetInSyncState(CF_IN_SYNC_STATE_IN_SYNC)
    //
    // The file keeps its content but gains cloud status:
    // - Green checkmark = locally available + synced
    // - Can be dehydrated later to free space

    Ok(())
}
