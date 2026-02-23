//! Hydration handler: responds to CFAPI FETCH_DATA callbacks.
//!
//! When a user opens a dehydrated placeholder file, the Cloud Files
//! minifilter driver intercepts the I/O and calls our registered
//! FETCH_DATA callback. This module:
//!
//! 1. Extracts the file identity (content hash) from the callback info
//! 2. Looks up the manifest path for the hash
//! 3. Fetches chunks from SeaweedFS via OpenDAL
//! 4. Streams data to the placeholder via CfExecute(CF_OPERATION_TYPE_TRANSFER_DATA)
//! 5. Acknowledges completion
//!
//! This is the Windows equivalent of tcfs-fuse's open() handler.

#![cfg(target_os = "windows")]

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

/// Handle a FETCH_DATA callback from the Cloud Files minifilter.
///
/// Called when a user or application opens a dehydrated placeholder.
/// Uses `tcfs_sync::manifest::SyncManifest` for manifest parsing and
/// `tcfs_chunks` for chunk integrity verification.
///
/// # Flow
/// 1. Parse file identity → content hash
/// 2. Look up manifest path: `{prefix}/manifests/{hash}`
/// 3. Read and parse SyncManifest (v1/v2 auto-detect)
/// 4. Fetch and verify each chunk via BLAKE3
/// 5. Return assembled data (Windows CfExecute transfer would go here)
pub async fn handle_fetch_data(
    op: &opendal::Operator,
    remote_prefix: &str,
    file_identity: &[u8],
    _required_offset: u64,
    _required_length: u64,
    // transfer_key: CF_TRANSFER_KEY, // Windows handle for data transfer
) -> Result<Vec<u8>> {
    let content_hash = String::from_utf8_lossy(file_identity);
    let prefix = remote_prefix.trim_end_matches('/');
    let manifest_path = format!("{}/manifests/{}", prefix, content_hash);

    debug!(hash = %content_hash, manifest = %manifest_path, "hydrating via CFAPI callback");

    // Read and parse manifest using SyncManifest (supports v1 + v2)
    let manifest_bytes = op
        .read(&manifest_path)
        .await
        .with_context(|| format!("reading manifest: {}", manifest_path))?;

    let manifest = tcfs_sync::manifest::SyncManifest::from_bytes(&manifest_bytes.to_bytes())
        .with_context(|| format!("parsing manifest: {}", manifest_path))?;

    let chunk_hashes = manifest.chunk_hashes();

    if chunk_hashes.is_empty() {
        anyhow::bail!("empty manifest: {}", manifest_path);
    }

    // Fetch and assemble all chunks with integrity verification
    let mut assembled = Vec::new();
    for (i, hash) in chunk_hashes.iter().enumerate() {
        let chunk_key = format!("{}/chunks/{}", prefix, hash);
        let chunk_data = op.read(&chunk_key).await.with_context(|| {
            format!(
                "downloading chunk {}/{}: {}",
                i + 1,
                chunk_hashes.len(),
                chunk_key
            )
        })?;

        let chunk_bytes = chunk_data.to_bytes();

        // Verify chunk integrity via BLAKE3
        let actual_hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(&chunk_bytes));
        if actual_hash != *hash {
            anyhow::bail!(
                "chunk integrity check failed for {}: expected {}, got {}",
                chunk_key,
                hash,
                actual_hash
            );
        }

        assembled.extend_from_slice(&chunk_bytes);
    }

    // Verify reassembled file hash if manifest has one (v2)
    if !manifest.file_hash.is_empty() {
        let actual_file_hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(&assembled));
        if actual_file_hash != manifest.file_hash {
            anyhow::bail!(
                "file integrity check failed: expected {}, got {}",
                manifest.file_hash,
                actual_file_hash
            );
        }
    }

    info!(
        hash = %content_hash,
        bytes = assembled.len(),
        chunks = chunk_hashes.len(),
        "CFAPI hydration complete"
    );

    // TODO: Instead of returning bytes, use CfExecute() to stream data
    // directly to the placeholder file via the transfer_key:
    //
    // use windows::Win32::Storage::CloudFilters::*;
    // for chunk in &chunks {
    //     let op_info = CF_OPERATION_INFO { TransferKey, ... };
    //     let op_params = CF_OPERATION_PARAMETERS {
    //         ParamSize: size_of::<CF_OPERATION_PARAMETERS>() as u32,
    //         TransferData: CF_OPERATION_TRANSFER_DATA_PARAMS {
    //             Buffer: chunk.as_ptr() as _,
    //             Length: chunk.len() as i64,
    //             Offset: current_offset as i64,
    //             ..Default::default()
    //         },
    //     };
    //     CfExecute(&op_info, &op_params)?;
    //     current_offset += chunk.len();
    // }

    Ok(assembled)
}

/// Handle a CANCEL_FETCH_DATA callback.
///
/// Called when the application closes the file before hydration completes,
/// or when the user cancels a download. Clean up any in-progress transfers.
pub async fn handle_cancel_fetch(file_identity: &[u8]) -> Result<()> {
    let content_hash = String::from_utf8_lossy(file_identity);
    warn!(hash = %content_hash, "CFAPI hydration cancelled");
    // TODO: Cancel any in-progress chunk downloads
    Ok(())
}

/// Report hydration progress to Explorer.
///
/// Shows a progress bar in File Explorer during large file downloads.
pub fn report_progress(
    // transfer_key: CF_TRANSFER_KEY,
    _total: u64,
    _completed: u64,
) {
    // TODO: CfExecute with CF_OPERATION_TYPE_REPORT_PROGRESS
}
