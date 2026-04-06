//! On-demand hydration: fetch a manifest's content from SeaweedFS chunks.
//!
//! Unlike `tcfs_sync::engine::download_file` (which writes to disk), this
//! returns the assembled bytes in memory so the FUSE driver can cache and
//! serve them without touching the local filesystem.

use anyhow::{Context, Result};
use opendal::Operator;
use tracing::{debug, warn};

use crate::cache::{cache_key_for_path, DiskCache};

/// Try to extract and unwrap the per-file encryption key from a JSON manifest.
/// Returns None if the manifest has no encrypted_file_key or decryption fails.
fn try_unwrap_file_key(manifest_json: &serde_json::Value, master_key: Option<&[u8; 32]>) -> Option<tcfs_crypto::FileKey> {
    let mk_bytes = master_key?;
    let mk = tcfs_crypto::MasterKey::from_bytes(*mk_bytes);
    let efk_b64 = manifest_json.get("encrypted_file_key")?.as_str()?;
    let wrapped = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, efk_b64).ok()?;
    tcfs_crypto::unwrap_key(&mk, &wrapped).ok()
}

/// Fetch the fully-assembled content for a manifest path.
///
/// Reads the manifest to get chunk hashes, fetches each chunk from
/// `{prefix}/chunks/{hash}`, decrypts if encrypted, and returns plaintext.
///
/// # Arguments
/// - `op` — OpenDAL operator pointing at the SeaweedFS bucket
/// - `manifest_path` — full path of the manifest object (e.g. `data/manifests/abc123`)
/// - `remote_prefix` — prefix used to look up chunks (e.g. `data`)
/// - `master_key` — optional master encryption key for decrypting chunks
pub async fn fetch_content(
    op: &Operator,
    manifest_path: &str,
    remote_prefix: &str,
    master_key: Option<&[u8; 32]>,
) -> Result<Vec<u8>> {
    debug!(manifest = %manifest_path, "hydrating");

    // Read manifest (supports both JSON v2 and legacy plaintext)
    let manifest_bytes = op
        .read(manifest_path)
        .await
        .with_context(|| format!("reading manifest: {}", manifest_path))?;

    let manifest_str = String::from_utf8(manifest_bytes.to_bytes().to_vec())
        .context("manifest is not valid UTF-8")?;

    // Parse chunk hashes and extract encryption metadata from JSON manifests
    let (chunk_hashes, file_key): (Vec<String>, Option<tcfs_crypto::FileKey>) =
        if manifest_str.trim_start().starts_with('{') {
            let parsed: serde_json::Value =
                serde_json::from_str(&manifest_str).context("parsing JSON manifest")?;
            let hashes = parsed["chunks"]
                .as_array()
                .context("manifest missing 'chunks' array")?
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
            let fk = try_unwrap_file_key(&parsed, master_key);
            debug!(
                master_key_present = master_key.is_some(),
                file_key_unwrapped = fk.is_some(),
                has_encrypted_file_key = parsed.get("encrypted_file_key").is_some(),
                "hydration crypto state"
            );
            (hashes, fk)
        } else {
            // Legacy plaintext: one chunk hash per line, no encryption
            let hashes = manifest_str
                .lines()
                .filter(|l| !l.is_empty())
                .map(|l| l.to_string())
                .collect();
            (hashes, None)
        };

    if chunk_hashes.is_empty() {
        anyhow::bail!("empty manifest: {}", manifest_path);
    }

    let prefix = remote_prefix.trim_end_matches('/');
    let mut assembled = Vec::new();

    // file_id for AEAD = BLAKE3(plaintext).as_bytes() (raw 32 bytes).
    // The manifest stores file_hash as hex(BLAKE3(plaintext)). We decode
    // the hex back to raw bytes to match what encrypt_chunk used.
    let file_id_bytes: Option<[u8; 32]> = if file_key.is_some() {
        let parsed: Option<String> = serde_json::from_str::<serde_json::Value>(&manifest_str)
            .ok()
            .and_then(|v| v.get("file_hash").and_then(|h| h.as_str().map(String::from)));
        parsed.and_then(|hex_hash| {
            debug!(file_hash_hex_len = hex_hash.len(), "parsing file_id for decryption");
            let raw: Vec<u8> = (0..hex_hash.len())
                .step_by(2)
                .filter_map(|i| u8::from_str_radix(&hex_hash[i..i+2], 16).ok())
                .collect();
            if raw.len() == 32 {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&raw);
                Some(arr)
            } else {
                None
            }
        })
    } else {
        None
    };

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
        let chunk_bytes = chunk.to_bytes();

        // Decrypt if we have a file key
        let plaintext = if let (Some(ref fk), Some(ref fid)) = (&file_key, &file_id_bytes) {
            tcfs_crypto::decrypt_chunk(fk, i as u64, fid, &chunk_bytes)
                .with_context(|| format!("decrypting chunk {}/{}", i + 1, chunk_hashes.len()))?
        } else {
            chunk_bytes.to_vec()
        };

        assembled.extend_from_slice(&plaintext);
    }

    debug!(
        manifest = %manifest_path,
        bytes = assembled.len(),
        chunks = chunk_hashes.len(),
        encrypted = file_key.is_some(),
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
    master_key: Option<&[u8; 32]>,
) -> Result<Vec<u8>> {
    let key = cache_key_for_path(manifest_path);

    // Cache hit (already decrypted on first fetch)
    if let Some(data) = cache.get(&key).await {
        debug!(manifest = %manifest_path, "hydration cache hit");
        return Ok(data);
    }

    // Cache miss — fetch from storage and decrypt
    let data = fetch_content(op, manifest_path, remote_prefix, master_key).await?;

    // Write to cache (best-effort; failure is non-fatal)
    if let Err(e) = cache.put(&key, &data).await {
        warn!(manifest = %manifest_path, "failed to cache hydrated content: {e}");
    }

    Ok(data)
}
