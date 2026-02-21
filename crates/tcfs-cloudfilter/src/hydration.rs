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

use anyhow::Result;
use tracing::{debug, info, warn};

/// Handle a FETCH_DATA callback from the Cloud Files minifilter.
///
/// Called when a user or application opens a dehydrated placeholder.
/// The callback provides:
/// - File identity (our content hash, set during placeholder creation)
/// - Required data range (offset + length)
/// - Transfer key (used to send data back)
///
/// # Flow
/// 1. Parse file identity → content hash
/// 2. Look up manifest path: `{prefix}/manifests/{hash}`
/// 3. Read manifest → list of chunk hashes
/// 4. For each chunk in the requested range:
///    a. Fetch from `{prefix}/chunks/{chunk_hash}`
///    b. Transfer to placeholder via CfExecute(TRANSFER_DATA)
/// 5. Acknowledge completion via CfExecute(ACK_DATA)
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

    // Read manifest
    let manifest_bytes = op
        .read(&manifest_path)
        .await
        .map_err(|e| anyhow::anyhow!("reading manifest {}: {}", manifest_path, e))?;
    let manifest_str = String::from_utf8(manifest_bytes.to_bytes().to_vec())
        .map_err(|e| anyhow::anyhow!("manifest not UTF-8: {}", e))?;

    let chunk_hashes: Vec<&str> = manifest_str.lines().filter(|l| !l.is_empty()).collect();

    if chunk_hashes.is_empty() {
        anyhow::bail!("empty manifest: {}", manifest_path);
    }

    // Fetch and assemble all chunks
    let mut assembled = Vec::new();
    for (i, hash) in chunk_hashes.iter().enumerate() {
        let chunk_key = format!("{}/chunks/{}", prefix, hash);
        let chunk = op
            .read(&chunk_key)
            .await
            .map_err(|e| anyhow::anyhow!("chunk {}/{}: {}", i + 1, chunk_hashes.len(), e))?;
        assembled.extend_from_slice(&chunk.to_bytes());
    }

    info!(
        hash = %content_hash,
        bytes = assembled.len(),
        chunks = chunk_hashes.len(),
        "CFAPI hydration complete"
    );

    // TODO: Phase 6c — instead of returning bytes, use CfExecute() to
    // stream data directly to the placeholder file via the transfer_key:
    //
    // for chunk in chunks {
    //     let op_info = CF_OPERATION_INFO { TransferKey, ... };
    //     let op_params = CF_OPERATION_PARAMETERS {
    //         ParamSize = size_of::<CF_OPERATION_PARAMETERS>(),
    //         TransferData = CF_OPERATION_TRANSFER_DATA_PARAMS {
    //             Buffer = chunk.as_ptr(),
    //             Length = chunk.len(),
    //             Offset = current_offset,
    //         },
    //     };
    //     CfExecute(&op_info, &op_params)?;
    //     current_offset += chunk.len();
    // }
    //
    // Then acknowledge:
    // CfExecute(&op_info, &ack_params)?;

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
    // TODO: Phase 6c
    // let op_info = CF_OPERATION_INFO { TransferKey, ... };
    // let progress_params = CF_OPERATION_PARAMETERS {
    //     ParamSize = ...,
    //     ReportProgress = CF_OPERATION_REPORT_PROGRESS_PARAMS {
    //         Total = total,
    //         Completed = completed,
    //     },
    // };
    // CfExecute(&op_info, &progress_params);
}
