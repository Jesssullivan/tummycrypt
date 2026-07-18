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
/// # Implementation
///
/// On Windows, calls `CfCreatePlaceholders()` with:
/// - FileIdentity = content_hash bytes (used in FETCH_DATA callback)
/// - FsMetadata.FileSize = info.file_size
/// - Flags = CF_PLACEHOLDER_CREATE_FLAG_MARK_IN_SYNC
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

    // TODO: Full CfCreatePlaceholders implementation
    //
    // use windows::Win32::Storage::CloudFilters::*;
    //
    // let identity = info.content_hash.as_bytes();
    // let file_name = full_path.file_name().unwrap().to_string_lossy();
    //
    // let placeholder = CF_PLACEHOLDER_CREATE_INFO {
    //     RelativeFileName: HSTRING::from(file_name.as_ref()).as_ptr(),
    //     FsMetadata: CF_FS_METADATA {
    //         FileSize: info.file_size as i64,
    //         BasicInfo: FILE_BASIC_INFO {
    //             LastWriteTime: to_filetime(info.modified),
    //             ..Default::default()
    //         },
    //         ..Default::default()
    //     },
    //     FileIdentity: identity.as_ptr() as _,
    //     FileIdentityLength: identity.len() as u32,
    //     Flags: CF_PLACEHOLDER_CREATE_FLAG_MARK_IN_SYNC,
    //     ..Default::default()
    // };
    //
    // let parent = full_path.parent().unwrap();
    // let parent_str = HSTRING::from(parent.to_string_lossy().as_ref());
    // unsafe { CfCreatePlaceholders(&parent_str, &[placeholder], 1, CF_CREATE_FLAG_NONE, ptr::null_mut())? };

    Ok(())
}

/// Create placeholder files for an entire directory tree.
///
/// Scans the remote index and creates a placeholder for each entry.
/// Uses `entry.path()` for the full S3 key (not `entry.name()` which is filename-only).
pub async fn populate_root(
    sync_root: &Path,
    op: &opendal::Operator,
    remote_prefix: &str,
) -> Result<usize> {
    let remote_prefix = remote_prefix.trim_end_matches('/');
    let index_prefix = if remote_prefix.is_empty() {
        "index/".to_string()
    } else {
        format!("{remote_prefix}/index/")
    };

    info!(
        root = %sync_root.display(),
        prefix = %index_prefix,
        "populating sync root with placeholders"
    );

    let entries = op
        .list_with(&index_prefix)
        .recursive(true)
        .await
        .context("listing remote index")?;

    let mut count = 0;
    for entry in entries {
        // Use entry.path() for the full S3 key path
        let entry_path = entry.path();
        let rel_path = entry_path.strip_prefix(&index_prefix).with_context(|| {
            format!(
                "remote placeholder listing escaped index prefix {index_prefix:?}: {entry_path:?}"
            )
        })?;

        if rel_path.is_empty() || rel_path.ends_with('/') {
            continue;
        }
        tcfs_sync::index_entry::validate_canonical_rel_path(rel_path)
            .with_context(|| format!("validating remote placeholder path: {rel_path:?}"))?;
        if rel_path == ".tcfs_dir" || rel_path.ends_with("/.tcfs_dir") {
            // Validate the reserved object instead of silently accepting a
            // corrupt marker. Directories are created as parents of visible
            // file placeholders, so the marker itself has no CFAPI entry.
            tcfs_sync::index_entry::directory_marker_is_visible(op, entry_path)
                .await
                .with_context(|| format!("validating directory marker: {entry_path}"))?;
            continue;
        }

        // Parse both legacy and versioned entries. A physical object carrying
        // a v4 tombstone (or a preparing record with no current value) is not
        // a visible placeholder. Corrupt records fail the population pass
        // rather than being silently omitted.
        let Some(record) =
            tcfs_sync::index_entry::read_index_entry_record_from_store(op, entry_path)
                .await
                .with_context(|| format!("reading remote placeholder index entry: {entry_path}"))?
        else {
            // The object disappeared after LIST; absence is not a placeholder.
            continue;
        };
        let Some(index_entry) = record.visible_entry() else {
            continue;
        };

        let info = PlaceholderInfo {
            relative_path: std::path::PathBuf::from(rel_path),
            file_size: index_entry.size,
            modified: std::time::SystemTime::now(),
            content_hash: index_entry.manifest_hash.clone(),
            manifest_path: if remote_prefix.is_empty() {
                format!("manifests/{}", index_entry.manifest_hash)
            } else {
                format!("{remote_prefix}/manifests/{}", index_entry.manifest_hash)
            },
            is_directory: false,
        };

        create_placeholder(sync_root, &info).await?;
        count += 1;
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

    // TODO: CfDehydratePlaceholder implementation
    //
    // use windows::Win32::Storage::CloudFilters::CfDehydratePlaceholder;
    // let handle = open_cf_handle(file_path)?;
    // unsafe { CfDehydratePlaceholder(handle, 0, file_size as i64, CF_DEHYDRATE_FLAG_NONE, None)? };

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

    // TODO: CfConvertToPlaceholder + CfSetInSyncState implementation
    //
    // use windows::Win32::Storage::CloudFilters::*;
    // let identity = info.content_hash.as_bytes();
    // let handle = open_cf_handle(file_path)?;
    // unsafe {
    //     CfConvertToPlaceholder(handle, identity.as_ptr() as _, identity.len() as u32,
    //                            CF_CONVERT_FLAG_MARK_IN_SYNC, 0, None)?;
    //     CfSetInSyncState(handle, CF_IN_SYNC_STATE_IN_SYNC, CF_SET_IN_SYNC_FLAG_NONE, None)?;
    // };

    Ok(())
}
