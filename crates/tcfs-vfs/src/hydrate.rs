//! On-demand hydration: fetch a manifest's content from SeaweedFS chunks.
//!
//! Unlike `tcfs_sync::engine::download_file` (which writes to disk), this
//! returns the assembled bytes in memory so the FUSE driver can cache and
//! serve them without touching the local filesystem.

use anyhow::{Context, Result};
use opendal::Operator;
use tracing::{debug, warn};

use crate::cache::{cache_key_for_path, DiskCache};

/// Fetch the fully-assembled content for a manifest path.
///
/// Reads the manifest to get chunk hashes, fetches each chunk from
/// `{prefix}/chunks/{hash}`, and returns the concatenated bytes.
///
/// # Arguments
/// - `op` — OpenDAL operator pointing at the SeaweedFS bucket
/// - `manifest_path` — full path of the manifest object (e.g. `data/manifests/abc123`)
/// - `remote_prefix` — prefix used to look up chunks (e.g. `data`)
pub async fn fetch_content(
    op: &Operator,
    manifest_path: &str,
    remote_prefix: &str,
) -> Result<Vec<u8>> {
    debug!(manifest = %manifest_path, "hydrating");

    // Read manifest: newline-separated chunk hashes
    let manifest_bytes = op
        .read(manifest_path)
        .await
        .with_context(|| format!("reading manifest: {}", manifest_path))?;

    let manifest_str = String::from_utf8(manifest_bytes.to_bytes().to_vec())
        .context("manifest is not valid UTF-8")?;

    let chunk_hashes: Vec<&str> = manifest_str.lines().filter(|l| !l.is_empty()).collect();

    if chunk_hashes.is_empty() {
        anyhow::bail!("empty manifest: {}", manifest_path);
    }

    let prefix = remote_prefix.trim_end_matches('/');
    let mut assembled = Vec::new();

    for (i, hash) in chunk_hashes.iter().enumerate() {
        let chunk_key = format!("{}/chunks/{}", prefix, hash);
        let chunk = op.read(&chunk_key).await.with_context(|| {
            format!(
                "downloading chunk {}/{}: {}",
                i + 1,
                chunk_hashes.len(),
                chunk_key
            )
        })?;
        assembled.extend_from_slice(&chunk.to_bytes());
    }

    debug!(
        manifest = %manifest_path,
        bytes = assembled.len(),
        chunks = chunk_hashes.len(),
        "hydrated"
    );

    Ok(assembled)
}

/// Fetch content using the disk cache as a read-through layer.
///
/// Returns cached bytes if present; otherwise fetches from SeaweedFS and
/// stores in the cache before returning.
pub async fn fetch_cached(
    op: &Operator,
    manifest_path: &str,
    remote_prefix: &str,
    cache: &DiskCache,
) -> Result<Vec<u8>> {
    let key = cache_key_for_path(manifest_path);

    // Cache hit
    if let Some(data) = cache.get(&key).await {
        debug!(manifest = %manifest_path, "hydration cache hit");
        return Ok(data);
    }

    // Cache miss — fetch from storage
    let data = fetch_content(op, manifest_path, remote_prefix).await?;

    // Write to cache (best-effort; failure is non-fatal)
    if let Err(e) = cache.put(&key, &data).await {
        warn!(manifest = %manifest_path, "failed to cache hydrated content: {e}");
    }

    Ok(data)
}
