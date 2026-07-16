//! On-demand hydration: fetch a manifest's content from SeaweedFS chunks.
//!
//! Unlike `tcfs_sync::engine::download_file` (which writes to disk), this
//! returns the assembled bytes in memory so the FUSE driver can cache and
//! serve them without touching the local filesystem.

use anyhow::{Context, Result};
use base64::Engine as _;
use opendal::Operator;
use tcfs_sync::manifest::SyncManifest;
use tracing::{debug, warn};

use crate::cache::{cache_key_for_path, DiskCache};

struct HydrationCrypto {
    file_key: tcfs_crypto::FileKey,
    file_id: [u8; 32],
}

/// Parse and validate the regular-file manifest shape used by the VFS reader.
///
/// Legacy v1 manifests remain readable, but they carry no whole-file size or
/// hash; their chunks are still verified individually during hydration. Modern
/// manifests must carry a valid whole-file BLAKE3 identity. This VFS surface
/// currently has only a master key, so per-device-only v3 content fails closed
/// instead of returning ciphertext.
fn prepare_manifest(
    manifest_bytes: &[u8],
    manifest_path: &str,
    master_key: Option<&[u8; 32]>,
) -> Result<(SyncManifest, Option<HydrationCrypto>)> {
    let manifest = SyncManifest::from_bytes(manifest_bytes)
        .with_context(|| format!("parsing regular-file manifest: {manifest_path}"))?;

    match manifest.version {
        1 => {
            anyhow::ensure!(
                manifest.file_hash.is_empty()
                    && manifest.file_size == 0
                    && manifest.encrypted_file_key.is_none()
                    && manifest.wrapped_file_keys.is_empty(),
                "legacy v1 manifest carries unsupported modern metadata: {manifest_path}"
            );
        }
        2 => {}
        3 => {
            anyhow::bail!(
                "per-device-only manifest v3 is not supported by VFS hydration: {manifest_path}"
            );
        }
        version => {
            anyhow::bail!("unsupported regular-file manifest version {version}: {manifest_path}");
        }
    }

    for (index, hash) in manifest.chunk_hashes().iter().enumerate() {
        tcfs_chunks::hash_from_hex(hash).with_context(|| {
            format!("invalid chunk hash at index {index} in manifest: {manifest_path}")
        })?;
    }

    if manifest.is_legacy() {
        return Ok((manifest, None));
    }

    let file_hash = tcfs_chunks::hash_from_hex(&manifest.file_hash)
        .with_context(|| format!("invalid file hash in manifest: {manifest_path}"))?;

    let crypto = match manifest.encrypted_file_key.as_deref() {
        Some(wrapped_b64) => {
            let master_bytes = master_key.with_context(|| {
                format!(
                    "manifest is encrypted but no master key is available for VFS hydration: {manifest_path}"
                )
            })?;
            let wrapped = base64::engine::general_purpose::STANDARD
                .decode(wrapped_b64)
                .with_context(|| {
                    format!("decoding encrypted_file_key in manifest: {manifest_path}")
                })?;
            let master = tcfs_crypto::MasterKey::from_bytes(*master_bytes);
            let file_key = tcfs_crypto::unwrap_key(&master, &wrapped)
                .with_context(|| format!("unwrapping file key for manifest: {manifest_path}"))?;
            Some(HydrationCrypto {
                file_key,
                file_id: *file_hash.as_bytes(),
            })
        }
        None if !manifest.wrapped_file_keys.is_empty() => {
            anyhow::bail!(
                "manifest has per-device wrapped keys but no master wrap; VFS hydration cannot decrypt it: {manifest_path}"
            );
        }
        None => None,
    };

    Ok((manifest, crypto))
}

fn validate_plaintext(
    manifest: &SyncManifest,
    plaintext: &[u8],
    manifest_path: &str,
) -> Result<()> {
    // v1 has no whole-file identity. Its chunks are verified against their
    // content addresses while fetching, but cached v1 content cannot be
    // independently revalidated and is therefore bypassed below.
    if manifest.is_legacy() {
        return Ok(());
    }

    anyhow::ensure!(
        plaintext.len() as u64 == manifest.file_size,
        "hydrated file size mismatch for {manifest_path}: expected {}, got {}",
        manifest.file_size,
        plaintext.len()
    );
    let actual = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(plaintext));
    anyhow::ensure!(
        actual == manifest.file_hash.as_str(),
        "hydrated file integrity failure for {manifest_path}: expected {}, got {actual}",
        manifest.file_hash
    );
    Ok(())
}

async fn load_manifest(
    op: &Operator,
    manifest_path: &str,
    master_key: Option<&[u8; 32]>,
) -> Result<(SyncManifest, Option<HydrationCrypto>)> {
    let manifest_bytes = op
        .read(manifest_path)
        .await
        .with_context(|| format!("reading manifest: {manifest_path}"))?
        .to_bytes();
    prepare_manifest(&manifest_bytes, manifest_path, master_key)
}

/// Fetch the fully-assembled content for a manifest path.
///
/// Reads the manifest to get chunk hashes, verifies every stored chunk before
/// decrypting it, and verifies the final plaintext size and BLAKE3 identity for
/// modern manifests.
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
    let (manifest, crypto) = load_manifest(op, manifest_path, master_key).await?;
    fetch_prepared_content(op, manifest_path, remote_prefix, &manifest, crypto.as_ref()).await
}

async fn fetch_prepared_content(
    op: &Operator,
    manifest_path: &str,
    remote_prefix: &str,
    manifest: &SyncManifest,
    crypto: Option<&HydrationCrypto>,
) -> Result<Vec<u8>> {
    debug!(manifest = %manifest_path, "hydrating");

    let prefix = remote_prefix.trim_end_matches('/');
    let mut assembled = Vec::new();

    for (index, hash) in manifest.chunk_hashes().iter().enumerate() {
        let chunk_key = format!("{prefix}/chunks/{hash}");
        let chunk = op.read(&chunk_key).await.with_context(|| {
            format!(
                "downloading chunk {}/{}: {chunk_key}",
                index + 1,
                manifest.chunk_hashes().len()
            )
        })?;
        let chunk_bytes = chunk.to_bytes();
        let actual_chunk_hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(&chunk_bytes));
        anyhow::ensure!(
            actual_chunk_hash == hash.as_str(),
            "chunk integrity failure at index {index} for {manifest_path}: expected {hash}, got {actual_chunk_hash}"
        );

        let plaintext = match crypto {
            Some(crypto) => tcfs_crypto::decrypt_chunk(
                &crypto.file_key,
                index as u64,
                &crypto.file_id,
                &chunk_bytes,
            )
            .with_context(|| {
                format!(
                    "decrypting chunk {}/{} for {manifest_path}",
                    index + 1,
                    manifest.chunk_hashes().len()
                )
            })?,
            None => chunk_bytes.to_vec(),
        };

        assembled.extend_from_slice(&plaintext);
        if !manifest.is_legacy() {
            anyhow::ensure!(
                assembled.len() as u64 <= manifest.file_size,
                "hydrated data exceeds declared file size for {manifest_path}: expected {}, got at least {}",
                manifest.file_size,
                assembled.len()
            );
        }
    }

    validate_plaintext(manifest, &assembled, manifest_path)?;

    debug!(
        manifest = %manifest_path,
        bytes = assembled.len(),
        chunks = manifest.chunk_hashes().len(),
        encrypted = crypto.is_some(),
        "hydrated"
    );

    Ok(assembled)
}

/// Fetch content using the disk cache as a read-through layer.
///
/// Modern cached plaintext is checked against the current manifest before it
/// is returned. Corrupt cache entries are evicted and repaired from remote
/// chunks. Legacy v1 manifests have no whole-file identity, so their cache
/// entries are bypassed and their chunks are reverified on every hydration.
pub async fn fetch_cached(
    op: &Operator,
    manifest_path: &str,
    remote_prefix: &str,
    cache: &DiskCache,
    master_key: Option<&[u8; 32]>,
) -> Result<Vec<u8>> {
    let (manifest, crypto) = load_manifest(op, manifest_path, master_key).await?;
    fetch_cached_prepared(
        op,
        manifest_path,
        remote_prefix,
        cache,
        &manifest,
        crypto.as_ref(),
    )
    .await
}

/// Hydrate using manifest bytes already bound to an exact index snapshot.
///
/// Callers that validate index-to-manifest identity must pass those same bytes
/// here so a replacement of the manifest key cannot split authorization,
/// vector-clock state, cache validation, and chunk selection across two reads.
pub async fn fetch_cached_from_manifest_bytes(
    op: &Operator,
    manifest_path: &str,
    manifest_bytes: &[u8],
    remote_prefix: &str,
    cache: &DiskCache,
    master_key: Option<&[u8; 32]>,
) -> Result<Vec<u8>> {
    let (manifest, crypto) = prepare_manifest(manifest_bytes, manifest_path, master_key)?;
    fetch_cached_prepared(
        op,
        manifest_path,
        remote_prefix,
        cache,
        &manifest,
        crypto.as_ref(),
    )
    .await
}

async fn fetch_cached_prepared(
    op: &Operator,
    manifest_path: &str,
    remote_prefix: &str,
    cache: &DiskCache,
    manifest: &SyncManifest,
    crypto: Option<&HydrationCrypto>,
) -> Result<Vec<u8>> {
    let key = cache_key_for_path(manifest_path);

    if let Some(data) = cache.get(&key).await {
        if !manifest.is_legacy() && validate_plaintext(manifest, &data, manifest_path).is_ok() {
            debug!(manifest = %manifest_path, "hydration cache hit");
            return Ok(data);
        }

        warn!(manifest = %manifest_path, "discarding unverifiable or corrupt hydration cache entry");
        if let Err(error) = cache.evict(&key).await {
            warn!(manifest = %manifest_path, %error, "failed to evict invalid hydration cache entry");
        }
    }

    let data = fetch_prepared_content(op, manifest_path, remote_prefix, manifest, crypto).await?;

    if let Err(error) = cache.put(&key, &data).await {
        warn!(manifest = %manifest_path, %error, "failed to cache hydrated content");
    }

    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use opendal::services::Memory;
    use tcfs_sync::conflict::VectorClock;
    use tcfs_sync::manifest::WrappedFileKey;

    const PREFIX: &str = "data";
    const MANIFEST_PATH: &str = "data/manifests/test";

    fn memory_op() -> Operator {
        let op = Operator::new(Memory::default()).unwrap().finish();
        tcfs_sync::index_entry::register_memory_index_emulation_for_tests(&op).unwrap();
        op
    }

    fn manifest(content: &[u8], chunks: Vec<String>) -> SyncManifest {
        SyncManifest {
            version: 2,
            file_hash: tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(content)),
            file_size: content.len() as u64,
            chunks,
            vclock: VectorClock::new(),
            written_by: "test-device".into(),
            written_at: 0,
            rel_path: Some("test.bin".into()),
            mode: None,
            mtime: None,
            encrypted_file_key: None,
            wrapped_file_keys: Vec::new(),
        }
    }

    async fn write_manifest(op: &Operator, manifest: &SyncManifest) {
        op.write(MANIFEST_PATH, manifest.to_bytes().unwrap())
            .await
            .unwrap();
    }

    async fn write_plain_chunk(op: &Operator, content: &[u8]) -> String {
        let hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(content));
        op.write(&format!("{PREFIX}/chunks/{hash}"), content.to_vec())
            .await
            .unwrap();
        hash
    }

    #[tokio::test]
    async fn hydrates_typed_plaintext_manifest_with_integrity_checks() {
        let op = memory_op();
        let content = b"typed hydration content";
        let chunk_hash = write_plain_chunk(&op, content).await;
        write_manifest(&op, &manifest(content, vec![chunk_hash])).await;

        let hydrated = fetch_content(&op, MANIFEST_PATH, PREFIX, None)
            .await
            .unwrap();
        assert_eq!(hydrated, content);
    }

    #[tokio::test]
    async fn supports_zero_byte_files_without_chunks() {
        let op = memory_op();
        write_manifest(&op, &manifest(b"", Vec::new())).await;

        let hydrated = fetch_content(&op, MANIFEST_PATH, PREFIX, None)
            .await
            .unwrap();
        assert!(hydrated.is_empty());
    }

    #[tokio::test]
    async fn rejects_corrupt_stored_chunk_before_assembly() {
        let op = memory_op();
        let content = b"expected chunk";
        let declared = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(content));
        op.write(
            &format!("{PREFIX}/chunks/{declared}"),
            b"corrupt chunk".to_vec(),
        )
        .await
        .unwrap();
        write_manifest(&op, &manifest(content, vec![declared])).await;

        let error = fetch_content(&op, MANIFEST_PATH, PREFIX, None)
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("chunk integrity failure"));
    }

    #[tokio::test]
    async fn rejects_final_plaintext_size_and_hash_mismatches() {
        let op = memory_op();
        let content = b"assembled content";
        let chunk_hash = write_plain_chunk(&op, content).await;

        let mut wrong_size = manifest(content, vec![chunk_hash.clone()]);
        wrong_size.file_size += 1;
        write_manifest(&op, &wrong_size).await;
        let size_error = fetch_content(&op, MANIFEST_PATH, PREFIX, None)
            .await
            .unwrap_err();
        assert!(format!("{size_error:#}").contains("file size mismatch"));

        let mut wrong_hash = manifest(content, vec![chunk_hash]);
        wrong_hash.file_hash =
            tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(b"different content"));
        write_manifest(&op, &wrong_hash).await;
        let hash_error = fetch_content(&op, MANIFEST_PATH, PREFIX, None)
            .await
            .unwrap_err();
        assert!(format!("{hash_error:#}").contains("file integrity failure"));
    }

    #[tokio::test]
    async fn decrypts_master_wrapped_v2_and_fails_closed_without_valid_master() {
        let op = memory_op();
        let content = b"encrypted VFS content";
        let content_hash = tcfs_chunks::hash_bytes(content);
        let file_id = *content_hash.as_bytes();
        let file_key = tcfs_crypto::generate_file_key();
        let ciphertext = tcfs_crypto::encrypt_chunk(&file_key, 0, &file_id, content).unwrap();
        let chunk_hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(&ciphertext));
        op.write(&format!("{PREFIX}/chunks/{chunk_hash}"), ciphertext)
            .await
            .unwrap();

        let master_bytes = [7u8; 32];
        let master = tcfs_crypto::MasterKey::from_bytes(master_bytes);
        let wrapped = tcfs_crypto::wrap_key(&master, &file_key).unwrap();
        let mut encrypted_manifest = manifest(content, vec![chunk_hash]);
        encrypted_manifest.encrypted_file_key =
            Some(base64::engine::general_purpose::STANDARD.encode(wrapped));
        write_manifest(&op, &encrypted_manifest).await;

        let hydrated = fetch_content(&op, MANIFEST_PATH, PREFIX, Some(&master_bytes))
            .await
            .unwrap();
        assert_eq!(hydrated, content);

        let missing = fetch_content(&op, MANIFEST_PATH, PREFIX, None)
            .await
            .unwrap_err();
        assert!(format!("{missing:#}").contains("no master key"));

        let wrong_master = [8u8; 32];
        let wrong = fetch_content(&op, MANIFEST_PATH, PREFIX, Some(&wrong_master))
            .await
            .unwrap_err();
        assert!(format!("{wrong:#}").contains("unwrapping file key"));
    }

    #[tokio::test]
    async fn rejects_per_device_only_v3_without_reading_ciphertext() {
        let op = memory_op();
        let mut per_device = manifest(
            b"secret",
            vec![tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(
                b"ciphertext",
            ))],
        );
        per_device.version = 3;
        per_device.wrapped_file_keys = vec![WrappedFileKey {
            recipient_device_id: "device-a".into(),
            recipient: "age1recipient".into(),
            algorithm: "age-x25519-file-key-v1".into(),
            wrapped_key: "wrapped".into(),
        }];
        write_manifest(&op, &per_device).await;

        let error = fetch_content(&op, MANIFEST_PATH, PREFIX, Some(&[7u8; 32]))
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("per-device-only manifest v3"));
    }

    #[tokio::test]
    async fn malformed_odd_length_file_hash_returns_error_without_panicking() {
        let op = memory_op();
        let mut malformed = manifest(b"", Vec::new());
        malformed.file_hash = "abc".into();
        write_manifest(&op, &malformed).await;

        let error = fetch_content(&op, MANIFEST_PATH, PREFIX, None)
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("invalid file hash"));
    }

    #[tokio::test]
    async fn corrupt_cache_entry_is_evicted_and_rehydrated() {
        let op = memory_op();
        let content = b"authoritative remote content";
        let chunk_hash = write_plain_chunk(&op, content).await;
        write_manifest(&op, &manifest(content, vec![chunk_hash])).await;

        let temp = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(temp.path().to_path_buf(), 1024 * 1024);
        cache.put("test", b"corrupt cached bytes").await.unwrap();

        let hydrated = fetch_cached(&op, MANIFEST_PATH, PREFIX, &cache, None)
            .await
            .unwrap();
        assert_eq!(hydrated, content);
        assert_eq!(cache.get("test").await.unwrap(), content);
    }

    #[tokio::test]
    async fn bound_manifest_bytes_control_cache_validation_and_chunk_selection() {
        let op = memory_op();
        let bound_content = b"content selected by the bound index";
        let replacement_content = b"replacement at the same manifest key";
        let bound_chunk = write_plain_chunk(&op, bound_content).await;
        let replacement_chunk = write_plain_chunk(&op, replacement_content).await;
        let bound_manifest = manifest(bound_content, vec![bound_chunk]);
        let replacement_manifest = manifest(replacement_content, vec![replacement_chunk]);
        let bound_bytes = bound_manifest.to_bytes().unwrap();

        // Model replacement after the caller bound and validated the original
        // bytes. Hydration must not authorize with one generation and reread
        // another generation for its chunks or cached plaintext.
        write_manifest(&op, &replacement_manifest).await;

        let temp = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(temp.path().to_path_buf(), 1024 * 1024);
        let hydrated = fetch_cached_from_manifest_bytes(
            &op,
            MANIFEST_PATH,
            &bound_bytes,
            PREFIX,
            &cache,
            None,
        )
        .await
        .unwrap();

        assert_eq!(hydrated, bound_content);
    }

    #[tokio::test]
    async fn legacy_v1_verifies_each_chunk_content_address() {
        let op = memory_op();
        let content = b"legacy content";
        let chunk_hash = write_plain_chunk(&op, content).await;
        op.write(MANIFEST_PATH, format!("{chunk_hash}\n").into_bytes())
            .await
            .unwrap();

        let hydrated = fetch_content(&op, MANIFEST_PATH, PREFIX, None)
            .await
            .unwrap();
        assert_eq!(hydrated, content);
    }
}
