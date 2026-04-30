//! Direct S3/OpenDAL backend for the FileProvider FFI.
//!
//! Talks directly to SeaweedFS/S3 without going through the daemon.
//! This is the original backend — no fleet sync, no NATS events.

use std::ffi::CStr;
use std::os::raw::c_char;
use std::panic::AssertUnwindSafe;
use std::ptr;
use std::sync::Mutex;

use crate::TcfsProgressCallback;

use base64::Engine;
use secrecy::SecretString;

use crate::{to_c_string, TcfsError, TcfsFileItem};

/// Opaque provider handle wrapping a tokio runtime + OpenDAL operator.
///
/// Created via `tcfs_provider_new`, freed via `tcfs_provider_free`.
pub struct TcfsProvider {
    runtime: tokio::runtime::Runtime,
    operator: opendal::Operator,
    remote_prefix: String,
    device_id: String,
    /// Master key for E2EE (None = plaintext mode for backwards compatibility)
    master_key: Option<tcfs_crypto::MasterKey>,
    last_error: Mutex<Option<String>>,
}

impl TcfsProvider {
    fn clear_last_error(&self) {
        if let Ok(mut last_error) = self.last_error.lock() {
            *last_error = None;
        }
    }

    fn set_last_error(&self, error: impl Into<String>) {
        if let Ok(mut last_error) = self.last_error.lock() {
            *last_error = Some(error.into());
        }
    }
}

fn parse_manifest_hash_from_index(bytes: &[u8]) -> anyhow::Result<String> {
    Ok(tcfs_sync::index_entry::parse_index_entry(bytes)?.manifest_hash)
}

fn parse_file_size_from_index(bytes: &[u8]) -> anyhow::Result<u64> {
    Ok(tcfs_sync::index_entry::parse_index_entry(bytes)?.size)
}

fn file_size_from_index(prov: &TcfsProvider, index_key: &str, fallback: u64) -> u64 {
    prov.runtime
        .block_on(async {
            let data = prov.operator.read(index_key).await?;
            let bytes = data.to_bytes();
            parse_file_size_from_index(&bytes)
        })
        .unwrap_or(fallback)
}

fn master_key_from_bytes(bytes: &[u8]) -> anyhow::Result<tcfs_crypto::MasterKey> {
    if bytes.len() != tcfs_crypto::KEY_SIZE {
        anyhow::bail!(
            "master key must be {} bytes, got {}",
            tcfs_crypto::KEY_SIZE,
            bytes.len()
        );
    }

    let mut key = [0u8; tcfs_crypto::KEY_SIZE];
    key.copy_from_slice(bytes);
    Ok(tcfs_crypto::MasterKey::from_bytes(key))
}

fn derive_master_key_from_config(config: &serde_json::Value) -> Option<tcfs_crypto::MasterKey> {
    if let Some(encoded) = config["master_key_base64"]
        .as_str()
        .filter(|s| !s.is_empty())
    {
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .ok()?;
        return master_key_from_bytes(&decoded).ok();
    }

    if let Some(path) = config["master_key_file"].as_str().filter(|s| !s.is_empty()) {
        let bytes = std::fs::read(path).ok()?;
        return master_key_from_bytes(&bytes).ok();
    }

    let passphrase = config["encryption_passphrase"]
        .as_str()
        .filter(|s| !s.is_empty())?;

    if passphrase.split_whitespace().count() >= 12 {
        return tcfs_crypto::mnemonic_to_master_key(passphrase).ok();
    }

    let salt_str = config["encryption_salt"]
        .as_str()
        .unwrap_or("tcfs-default-salt!");
    let mut salt = [0u8; 16];
    let salt_bytes = salt_str.as_bytes();
    let copy_len = salt_bytes.len().min(16);
    salt[..copy_len].copy_from_slice(&salt_bytes[..copy_len]);

    let params = tcfs_crypto::kdf::KdfParams {
        mem_cost_kib: config["argon2_mem_cost_kib"].as_u64().unwrap_or(65536) as u32,
        time_cost: config["argon2_time_cost"].as_u64().unwrap_or(3) as u32,
        parallelism: config["argon2_parallelism"].as_u64().unwrap_or(4) as u32,
    };

    tcfs_crypto::derive_master_key(&SecretString::from(passphrase.to_string()), &salt, &params).ok()
}

fn error_code_for_fetch_error(error: &anyhow::Error) -> TcfsError {
    if error.chain().any(|cause| {
        cause
            .downcast_ref::<opendal::Error>()
            .is_some_and(|e| e.kind() == opendal::ErrorKind::NotFound)
    }) {
        TcfsError::TcfsErrorNotFound
    } else {
        TcfsError::TcfsErrorStorage
    }
}

/// Return the last backend error message recorded on this provider.
///
/// The caller owns the returned string and must free it with `tcfs_string_free`.
///
/// # Safety
///
/// `provider` must be a valid pointer from `tcfs_provider_new`.
#[no_mangle]
pub unsafe extern "C" fn tcfs_provider_last_error(provider: *mut TcfsProvider) -> *mut c_char {
    if provider.is_null() {
        return ptr::null_mut();
    }

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let prov = unsafe { &*provider };
        prov.last_error
            .lock()
            .ok()
            .and_then(|last_error| last_error.clone())
            .map(|message| to_c_string(&message))
            .unwrap_or(ptr::null_mut())
    }));

    result.unwrap_or(ptr::null_mut())
}

/// Create a new provider from a JSON configuration string.
///
/// # Safety
///
/// `config_json` must be a valid null-terminated UTF-8 C string.
#[no_mangle]
pub unsafe extern "C" fn tcfs_provider_new(config_json: *const c_char) -> *mut TcfsProvider {
    if config_json.is_null() {
        return ptr::null_mut();
    }

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let c_str = unsafe { CStr::from_ptr(config_json) };
        let json_str = match c_str.to_str() {
            Ok(s) => s,
            Err(_) => return ptr::null_mut(),
        };

        let config: serde_json::Value = match serde_json::from_str(json_str) {
            Ok(v) => v,
            Err(_) => return ptr::null_mut(),
        };

        let endpoint = config["s3_endpoint"].as_str().unwrap_or_default();
        let bucket = config["s3_bucket"].as_str().unwrap_or("tcfs");
        let access = config["s3_access"].as_str().unwrap_or_default();
        let secret = config["s3_secret"].as_str().unwrap_or_default();
        let prefix = config["remote_prefix"]
            .as_str()
            .unwrap_or("default")
            .to_string();
        let device_id = config["device_id"]
            .as_str()
            .unwrap_or("unknown")
            .to_string();

        // Derive master key from encryption_passphrase if provided (enables E2EE).
        // Mnemonic recovery phrases must use the same derivation path as `tcfs init`;
        // generic passphrases keep the per-vault salt and Argon2 params.
        let master_key = derive_master_key_from_config(&config);

        // Single-threaded tokio runtime to avoid deadlock with fileproviderd.
        // Multi-threaded runtime spawns worker threads that contend with XPC
        // file coordination locks, causing EDEADLK. Single-threaded runs all
        // async work on the calling thread (the dispatch queue thread).
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(_) => return ptr::null_mut(),
        };

        let operator =
            tcfs_storage::operator::build_operator(&tcfs_storage::operator::StorageConfig {
                endpoint: endpoint.to_string(),
                region: "us-east-1".to_string(),
                bucket: bucket.to_string(),
                access_key_id: access.to_string(),
                secret_access_key: secret.to_string(),
            });

        let operator = match operator {
            Ok(op) => op,
            Err(_) => return ptr::null_mut(),
        };

        Box::into_raw(Box::new(TcfsProvider {
            runtime,
            operator,
            remote_prefix: prefix,
            device_id,
            master_key,
            last_error: Mutex::new(None),
        }))
    }));

    result.unwrap_or(ptr::null_mut())
}

/// Enumerate files under a relative path within the remote prefix.
///
/// # Safety
///
/// - `provider` must be a valid pointer from `tcfs_provider_new`.
/// - `path` must be a valid null-terminated UTF-8 C string (use "" for root).
/// - `out_items` and `out_count` must be valid writable pointers.
#[no_mangle]
pub unsafe extern "C" fn tcfs_provider_enumerate(
    provider: *mut TcfsProvider,
    path: *const c_char,
    out_items: *mut *mut TcfsFileItem,
    out_count: *mut usize,
) -> TcfsError {
    if provider.is_null() || path.is_null() || out_items.is_null() || out_count.is_null() {
        return TcfsError::TcfsErrorInvalidArg;
    }

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let prov = unsafe { &*provider };
        let c_path = unsafe { CStr::from_ptr(path) };
        let rel_path = match c_path.to_str() {
            Ok(s) => s,
            Err(_) => return TcfsError::TcfsErrorInvalidArg,
        };

        // Build S3 prefix with trailing slash for correct prefix matching.
        // Without trailing slash, listing "data/index/ansible" would also
        // match "data/index/ansible-lint" — the slash ensures we only get
        // children of the target directory.
        let prefix = if rel_path.is_empty() {
            format!("{}/index/", prov.remote_prefix.trim_end_matches('/'))
        } else {
            format!(
                "{}/index/{}/",
                prov.remote_prefix.trim_end_matches('/'),
                rel_path.trim_matches('/')
            )
        };

        let entries = match prov.runtime.block_on(prov.operator.list(&prefix)) {
            Ok(e) => e,
            Err(_) => return TcfsError::TcfsErrorStorage,
        };

        // Collect immediate children only. S3 list returns all descendants
        // recursively, so we extract the first path segment after the prefix
        // and deduplicate to get directories and files at this level.
        let mut seen = std::collections::HashSet::new();
        let mut items: Vec<TcfsFileItem> = Vec::new();

        for entry in entries {
            let entry_path = entry.path();
            let remainder = entry_path
                .strip_prefix(&prefix)
                .unwrap_or(entry_path)
                .trim_start_matches('/');

            if remainder.is_empty() {
                continue;
            }

            // Extract the immediate child name:
            // "roles/common/tasks/main.yml" → "roles"  (directory)
            // "README.md"                   → "README.md" (file)
            let (child_name, is_dir) = match remainder.find('/') {
                Some(slash_pos) => (&remainder[..slash_pos], true),
                None => (remainder, entry.metadata().is_dir()),
            };

            if child_name.is_empty() || !seen.insert(child_name.to_string()) {
                continue;
            }

            // For directories, the item_id is the full path relative to the
            // remote prefix (used by Swift as containerIdentifier.rawValue
            // when the user drills into the directory).
            let item_id = if rel_path.is_empty() {
                child_name.to_string()
            } else {
                format!("{}/{}", rel_path.trim_matches('/'), child_name)
            };

            let file_size = if is_dir {
                0
            } else {
                file_size_from_index(prov, entry_path, entry.metadata().content_length())
            };

            // item_id is the relative path (e.g. "ansible/roles") — NOT the
            // full S3 key. Swift uses item_id as containerIdentifier.rawValue
            // which becomes rel_path on the next enumerate call. Using the
            // full S3 key would cause double-prefixing.
            items.push(TcfsFileItem {
                item_id: to_c_string(&item_id),
                filename: to_c_string(child_name),
                file_size,
                modified_timestamp: 0,
                is_directory: is_dir,
                content_hash: to_c_string(""),
                hydration_state: to_c_string("not_synced"),
            });
        }

        let count = items.len();
        let boxed = items.into_boxed_slice();
        let ptr = Box::into_raw(boxed) as *mut TcfsFileItem;

        unsafe {
            *out_items = ptr;
            *out_count = count;
        }

        TcfsError::TcfsErrorNone
    }));

    result.unwrap_or(TcfsError::TcfsErrorInternal)
}

/// Fetch (hydrate) a file by its item ID to a local destination path.
///
/// # Safety
///
/// - `provider` must be a valid pointer from `tcfs_provider_new`.
/// - `item_id` and `dest_path` must be valid null-terminated UTF-8 C strings.
#[no_mangle]
pub unsafe extern "C" fn tcfs_provider_fetch(
    provider: *mut TcfsProvider,
    item_id: *const c_char,
    dest_path: *const c_char,
) -> TcfsError {
    if provider.is_null() || item_id.is_null() || dest_path.is_null() {
        return TcfsError::TcfsErrorInvalidArg;
    }

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let prov = unsafe { &*provider };
        let c_item = unsafe { CStr::from_ptr(item_id) };
        let c_dest = unsafe { CStr::from_ptr(dest_path) };
        prov.clear_last_error();

        let item_str = match c_item.to_str() {
            Ok(s) => s,
            Err(_) => return TcfsError::TcfsErrorInvalidArg,
        };
        let dest_str = match c_dest.to_str() {
            Ok(s) => s,
            Err(_) => return TcfsError::TcfsErrorInvalidArg,
        };

        let fetch_result = prov.runtime.block_on(async {
            // Reconstruct full S3 key from relative item_id
            let index_key = format!(
                "{}/index/{}",
                prov.remote_prefix.trim_end_matches('/'),
                item_str.trim_start_matches('/')
            );
            // Read the index entry to get manifest hash
            let data = prov.operator.read(&index_key).await?;
            let bytes = data.to_bytes();
            let manifest_hash = parse_manifest_hash_from_index(&bytes)?;

            let manifest_path = format!(
                "{}/manifests/{}",
                prov.remote_prefix.trim_end_matches('/'),
                manifest_hash
            );

            let manifest_bytes = prov.operator.read(&manifest_path).await?;
            let manifest =
                tcfs_sync::manifest::SyncManifest::from_bytes(&manifest_bytes.to_bytes())?;

            // Unwrap file key if E2EE manifest
            let file_key = match (&prov.master_key, &manifest.encrypted_file_key) {
                (Some(mk), Some(wrapped_b64)) => {
                    let wrapped = base64::engine::general_purpose::STANDARD.decode(wrapped_b64)?;
                    Some(tcfs_crypto::unwrap_key(mk, &wrapped)?)
                }
                (None, Some(_)) => {
                    anyhow::bail!("file is encrypted but no master key configured");
                }
                _ => None,
            };

            // Reconstruct file_id for AAD verification (BLAKE3 of plaintext)
            let file_id_bytes: [u8; 32] = tcfs_chunks::hash_from_hex(&manifest.file_hash)
                .map(|h| *h.as_bytes())
                .unwrap_or([0u8; 32]);

            let mut assembled = Vec::new();
            for (idx, hash) in manifest.chunk_hashes().iter().enumerate() {
                let chunk_key = format!(
                    "{}/chunks/{}",
                    prov.remote_prefix.trim_end_matches('/'),
                    hash
                );
                let chunk_data = prov.operator.read(&chunk_key).await?;
                let chunk_bytes = chunk_data.to_bytes();

                // BLAKE3 integrity verification
                let actual = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(&chunk_bytes));
                if actual != *hash {
                    anyhow::bail!("chunk integrity failure: expected {}, got {}", hash, actual);
                }

                // Decrypt if E2EE
                if let Some(ref fk) = file_key {
                    let plaintext =
                        tcfs_crypto::decrypt_chunk(fk, idx as u64, &file_id_bytes, &chunk_bytes)?;
                    assembled.extend_from_slice(&plaintext);
                } else {
                    assembled.extend_from_slice(&chunk_bytes);
                }
            }

            tokio::fs::write(dest_str, &assembled).await?;
            Ok::<(), anyhow::Error>(())
        });

        match fetch_result {
            Ok(()) => TcfsError::TcfsErrorNone,
            Err(e) => {
                let message = format!("{e:#}");
                tracing::error!(item_id = item_str, error = %message, "tcfs_provider_fetch failed");
                prov.set_last_error(message);
                error_code_for_fetch_error(&e)
            }
        }
    }));

    result.unwrap_or(TcfsError::TcfsErrorInternal)
}

/// Download remote content to a local file, reporting progress via callback.
///
/// Same as `tcfs_provider_fetch` but calls `callback(completed_bytes, total_bytes, context)`
/// after each chunk download. Finder uses this to render a progress bar.
///
/// # Safety
///
/// - `provider` must be a valid pointer from `tcfs_provider_new`.
/// - `item_id` and `dest_path` must be valid null-terminated UTF-8 C strings.
/// - `callback` may be null (progress not reported).
/// - `context` is passed through to the callback (may be null).
#[no_mangle]
pub unsafe extern "C" fn tcfs_provider_fetch_with_progress(
    provider: *mut TcfsProvider,
    item_id: *const c_char,
    dest_path: *const c_char,
    callback: TcfsProgressCallback,
    context: *const std::ffi::c_void,
) -> TcfsError {
    if provider.is_null() || item_id.is_null() || dest_path.is_null() {
        return TcfsError::TcfsErrorInvalidArg;
    }

    // context pointer must be safe to send across threads
    let ctx = context as usize;

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let prov = unsafe { &*provider };
        let c_item = unsafe { CStr::from_ptr(item_id) };
        let c_dest = unsafe { CStr::from_ptr(dest_path) };
        prov.clear_last_error();

        let item_str = match c_item.to_str() {
            Ok(s) => s,
            Err(_) => return TcfsError::TcfsErrorInvalidArg,
        };
        let dest_str = match c_dest.to_str() {
            Ok(s) => s,
            Err(_) => return TcfsError::TcfsErrorInvalidArg,
        };

        let fetch_result = prov.runtime.block_on(async {
            let index_key = format!(
                "{}/index/{}",
                prov.remote_prefix.trim_end_matches('/'),
                item_str.trim_start_matches('/')
            );
            let data = prov.operator.read(&index_key).await?;
            let bytes = data.to_bytes();
            let manifest_hash = parse_manifest_hash_from_index(&bytes)?;

            let manifest_path = format!(
                "{}/manifests/{}",
                prov.remote_prefix.trim_end_matches('/'),
                manifest_hash
            );
            let manifest_bytes = prov.operator.read(&manifest_path).await?;
            let manifest =
                tcfs_sync::manifest::SyncManifest::from_bytes(&manifest_bytes.to_bytes())?;

            let file_key = match (&prov.master_key, &manifest.encrypted_file_key) {
                (Some(mk), Some(wrapped_b64)) => {
                    let wrapped = base64::engine::general_purpose::STANDARD.decode(wrapped_b64)?;
                    Some(tcfs_crypto::unwrap_key(mk, &wrapped)?)
                }
                (None, Some(_)) => {
                    anyhow::bail!("file is encrypted but no master key configured");
                }
                _ => None,
            };

            let file_id_bytes: [u8; 32] = tcfs_chunks::hash_from_hex(&manifest.file_hash)
                .map(|h| *h.as_bytes())
                .unwrap_or([0u8; 32]);

            let total_bytes = manifest.file_size;
            let mut bytes_received: u64 = 0;

            // Signal start
            if let Some(cb) = callback {
                unsafe { cb(0, total_bytes, ctx as *const std::ffi::c_void) };
            }

            let mut assembled = Vec::with_capacity(total_bytes as usize);
            for (idx, hash) in manifest.chunk_hashes().iter().enumerate() {
                let chunk_key = format!(
                    "{}/chunks/{}",
                    prov.remote_prefix.trim_end_matches('/'),
                    hash
                );
                let chunk_data = prov.operator.read(&chunk_key).await?;
                let chunk_bytes = chunk_data.to_bytes();

                let actual = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(&chunk_bytes));
                if actual != *hash {
                    anyhow::bail!("chunk integrity failure: expected {}, got {}", hash, actual);
                }

                if let Some(ref fk) = file_key {
                    let plaintext =
                        tcfs_crypto::decrypt_chunk(fk, idx as u64, &file_id_bytes, &chunk_bytes)?;
                    bytes_received += plaintext.len() as u64;
                    assembled.extend_from_slice(&plaintext);
                } else {
                    bytes_received += chunk_bytes.len() as u64;
                    assembled.extend_from_slice(&chunk_bytes);
                }

                // Report progress after each chunk
                if let Some(cb) = callback {
                    unsafe { cb(bytes_received, total_bytes, ctx as *const std::ffi::c_void) };
                }
            }

            tokio::fs::write(dest_str, &assembled).await?;
            Ok::<(), anyhow::Error>(())
        });

        match fetch_result {
            Ok(()) => TcfsError::TcfsErrorNone,
            Err(e) => {
                let message = format!("{e:#}");
                tracing::error!(
                    item_id = item_str,
                    error = %message,
                    "tcfs_provider_fetch_with_progress failed"
                );
                prov.set_last_error(message);
                error_code_for_fetch_error(&e)
            }
        }
    }));

    result.unwrap_or(TcfsError::TcfsErrorInternal)
}

/// Upload a local file to the remote prefix.
///
/// # Safety
///
/// - `provider` must be a valid pointer from `tcfs_provider_new`.
/// - `local_path` and `remote_rel` must be valid null-terminated UTF-8 C strings.
#[no_mangle]
pub unsafe extern "C" fn tcfs_provider_upload(
    provider: *mut TcfsProvider,
    local_path: *const c_char,
    remote_rel: *const c_char,
) -> TcfsError {
    if provider.is_null() || local_path.is_null() || remote_rel.is_null() {
        return TcfsError::TcfsErrorInvalidArg;
    }

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let prov = unsafe { &*provider };
        let c_local = unsafe { CStr::from_ptr(local_path) };
        let c_remote = unsafe { CStr::from_ptr(remote_rel) };

        let local_str = match c_local.to_str() {
            Ok(s) => s,
            Err(_) => return TcfsError::TcfsErrorInvalidArg,
        };
        let remote_str = match c_remote.to_str() {
            Ok(s) => s,
            Err(_) => return TcfsError::TcfsErrorInvalidArg,
        };

        let upload_result = prov.runtime.block_on(async {
            let data = tokio::fs::read(local_str).await?;
            let file_hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(&data));

            // Generate per-file encryption key if E2EE is enabled
            let file_key = prov
                .master_key
                .as_ref()
                .map(|_| tcfs_crypto::generate_file_key());
            let file_id_bytes: [u8; 32] = {
                let h = tcfs_chunks::hash_bytes(&data);
                let mut arr = [0u8; 32];
                arr.copy_from_slice(h.as_bytes());
                arr
            };

            let chunks = tcfs_chunks::chunk_data(&data, tcfs_chunks::ChunkSizes::SMALL);
            let mut chunk_hashes = Vec::new();

            for (idx, chunk) in chunks.iter().enumerate() {
                let chunk_bytes =
                    &data[chunk.offset as usize..chunk.offset as usize + chunk.length];

                let upload_bytes = if let Some(ref fk) = file_key {
                    tcfs_crypto::encrypt_chunk(fk, idx as u64, &file_id_bytes, chunk_bytes)?
                } else {
                    chunk_bytes.to_vec()
                };

                let hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(&upload_bytes));
                let chunk_key = format!(
                    "{}/chunks/{}",
                    prov.remote_prefix.trim_end_matches('/'),
                    hash
                );
                prov.operator.write(&chunk_key, upload_bytes).await?;
                chunk_hashes.push(hash);
            }

            // Build vclock: read existing manifest if present and merge
            let mut vclock = tcfs_sync::conflict::VectorClock::new();
            let existing_index_key = format!(
                "{}/index/{}",
                prov.remote_prefix.trim_end_matches('/'),
                remote_str.trim_start_matches('/')
            );
            if let Ok(existing_data) = prov.operator.read(&existing_index_key).await {
                let existing_bytes = existing_data.to_bytes();
                if let Ok(hash) = parse_manifest_hash_from_index(&existing_bytes) {
                    let manifest_path = format!(
                        "{}/manifests/{}",
                        prov.remote_prefix.trim_end_matches('/'),
                        hash
                    );
                    if let Ok(mb) = prov.operator.read(&manifest_path).await {
                        if let Ok(existing_manifest) =
                            tcfs_sync::manifest::SyncManifest::from_bytes(&mb.to_bytes())
                        {
                            vclock.merge(&existing_manifest.vclock);
                        }
                    }
                }
            }
            vclock.tick(&prov.device_id);

            let written_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            // Wrap file key with master key for storage in manifest
            let encrypted_file_key = match (&prov.master_key, &file_key) {
                (Some(mk), Some(fk)) => {
                    let wrapped = tcfs_crypto::wrap_key(mk, fk)?;
                    Some(base64::engine::general_purpose::STANDARD.encode(&wrapped))
                }
                _ => None,
            };

            let manifest = tcfs_sync::manifest::SyncManifest {
                version: 2,
                file_hash: file_hash.clone(),
                file_size: data.len() as u64,
                chunks: chunk_hashes,
                vclock,
                written_by: prov.device_id.clone(),
                written_at,
                rel_path: Some(remote_str.to_string()),
                mode: None,
                encrypted_file_key,
            };

            let manifest_json = serde_json::to_vec_pretty(&manifest)?;
            let manifest_key = format!(
                "{}/manifests/{}",
                prov.remote_prefix.trim_end_matches('/'),
                file_hash
            );
            prov.operator.write(&manifest_key, manifest_json).await?;

            // Write index entry
            let index_key = format!(
                "{}/index/{}",
                prov.remote_prefix.trim_end_matches('/'),
                remote_str.trim_start_matches('/')
            );
            let index_entry = tcfs_sync::index_entry::RemoteIndexEntry::new(
                file_hash,
                data.len() as u64,
                chunks.len(),
            );
            tcfs_sync::index_entry::write_committed_index_entry(
                &prov.operator,
                &index_key,
                &index_entry,
            )
            .await?;

            Ok::<(), anyhow::Error>(())
        });

        match upload_result {
            Ok(()) => TcfsError::TcfsErrorNone,
            Err(_) => TcfsError::TcfsErrorStorage,
        }
    }));

    result.unwrap_or(TcfsError::TcfsErrorInternal)
}

/// Delete a file or directory by its item ID.
///
/// # Safety
///
/// - `provider` must be a valid pointer from `tcfs_provider_new`.
/// - `item_id` must be a valid null-terminated UTF-8 C string.
#[no_mangle]
pub unsafe extern "C" fn tcfs_provider_delete(
    provider: *mut TcfsProvider,
    item_id: *const c_char,
) -> TcfsError {
    if provider.is_null() || item_id.is_null() {
        return TcfsError::TcfsErrorInvalidArg;
    }

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let prov = unsafe { &*provider };
        let c_item = unsafe { CStr::from_ptr(item_id) };
        let item_str = match c_item.to_str() {
            Ok(s) => s,
            Err(_) => return TcfsError::TcfsErrorInvalidArg,
        };

        let delete_result = prov.runtime.block_on(async {
            // Reconstruct full S3 key from relative item_id
            let index_key = format!(
                "{}/index/{}",
                prov.remote_prefix.trim_end_matches('/'),
                item_str.trim_start_matches('/')
            );
            let index_data = prov.operator.read(&index_key).await;
            if let Ok(data) = index_data {
                let bytes = data.to_bytes();
                let manifest_prefix =
                    format!("{}/manifests", prov.remote_prefix.trim_end_matches('/'));
                if let Ok(entry) = tcfs_sync::index_entry::parse_index_entry_record(&bytes) {
                    for manifest_path in entry.referenced_object_keys(&manifest_prefix) {
                        let _ = prov.operator.delete(&manifest_path).await;
                    }
                }
            }

            prov.operator.delete(&index_key).await?;
            Ok::<(), anyhow::Error>(())
        });

        match delete_result {
            Ok(()) => TcfsError::TcfsErrorNone,
            Err(_) => TcfsError::TcfsErrorStorage,
        }
    }));

    result.unwrap_or(TcfsError::TcfsErrorInternal)
}

/// Create a directory under the given parent path.
///
/// # Safety
///
/// - `provider` must be a valid pointer from `tcfs_provider_new`.
/// - `parent_path` and `dir_name` must be valid null-terminated UTF-8 C strings.
#[no_mangle]
pub unsafe extern "C" fn tcfs_provider_create_dir(
    provider: *mut TcfsProvider,
    parent_path: *const c_char,
    dir_name: *const c_char,
) -> TcfsError {
    if provider.is_null() || parent_path.is_null() || dir_name.is_null() {
        return TcfsError::TcfsErrorInvalidArg;
    }

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let prov = unsafe { &*provider };
        let c_parent = unsafe { CStr::from_ptr(parent_path) };
        let c_name = unsafe { CStr::from_ptr(dir_name) };

        let parent_str = match c_parent.to_str() {
            Ok(s) => s,
            Err(_) => return TcfsError::TcfsErrorInvalidArg,
        };
        let name_str = match c_name.to_str() {
            Ok(s) => s,
            Err(_) => return TcfsError::TcfsErrorInvalidArg,
        };

        let dir_path = format!(
            "{}/index/{}{}/",
            prov.remote_prefix.trim_end_matches('/'),
            if parent_str.is_empty() {
                String::new()
            } else {
                format!("{}/", parent_str.trim_matches('/'))
            },
            name_str.trim_matches('/')
        );

        let create_result = prov
            .runtime
            .block_on(prov.operator.write(&dir_path, Vec::<u8>::new()));

        match create_result {
            Ok(_) => TcfsError::TcfsErrorNone,
            Err(_) => TcfsError::TcfsErrorStorage,
        }
    }));

    result.unwrap_or(TcfsError::TcfsErrorInternal)
}

/// Enumerate changes since a given timestamp.
///
/// The direct backend has no event stream (no daemon), so this always
/// returns an empty change set.  The caller (FileProvider) falls back
/// to a full re-enumerate when the list is empty.
///
/// # Safety
///
/// - `provider` must be a valid `TcfsProvider` pointer.
/// - `path` must be a valid UTF-8 C string.
/// - `out_events` and `out_count` must be valid, non-null pointers.
#[no_mangle]
pub unsafe extern "C" fn tcfs_provider_enumerate_changes(
    _provider: *mut TcfsProvider,
    _path: *const c_char,
    _since_timestamp: i64,
    out_events: *mut *mut crate::TcfsChangeEvent,
    out_count: *mut usize,
) -> TcfsError {
    if out_events.is_null() || out_count.is_null() {
        return TcfsError::TcfsErrorInvalidArg;
    }
    unsafe {
        *out_events = ptr::null_mut();
        *out_count = 0;
    }
    TcfsError::TcfsErrorNone
}

/// Start a background change watch.
///
/// The direct backend has no daemon event stream. Export a no-op symbol so the
/// Swift extension can link against either backend; callers should use explicit
/// re-enumeration for direct-mode refresh.
///
/// # Safety
///
/// `provider` must be a valid pointer from `tcfs_provider_new`.
#[no_mangle]
pub unsafe extern "C" fn tcfs_provider_start_watch(
    provider: *mut TcfsProvider,
    _callback: crate::TcfsWatchCallback,
    _callback_context: *const std::ffi::c_void,
) -> TcfsError {
    if provider.is_null() {
        return TcfsError::TcfsErrorInvalidArg;
    }

    TcfsError::TcfsErrorNone
}

/// Free a provider handle.
///
/// # Safety
///
/// `provider` must be a valid pointer from `tcfs_provider_new`, or null.
#[no_mangle]
pub unsafe extern "C" fn tcfs_provider_free(provider: *mut TcfsProvider) {
    if !provider.is_null() {
        unsafe {
            drop(Box::from_raw(provider));
        }
    }
}

// ── Functional tests (memory-backed provider) ────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    #[test]
    fn mnemonic_config_uses_recovery_derivation() {
        let (words, expected) = tcfs_crypto::generate_mnemonic().unwrap();
        let config = serde_json::json!({
            "encryption_passphrase": words,
            "encryption_salt": "ignored-for-mnemonic",
        });

        let actual = derive_master_key_from_config(&config).unwrap();
        assert_eq!(actual.as_bytes(), expected.as_bytes());
    }

    #[test]
    fn raw_master_key_config_takes_precedence_over_passphrase() {
        let expected = tcfs_crypto::MasterKey::from_bytes([7u8; tcfs_crypto::KEY_SIZE]);
        let config = serde_json::json!({
            "master_key_base64": base64::engine::general_purpose::STANDARD.encode(expected.as_bytes()),
            "encryption_passphrase": "stale passphrase",
            "encryption_salt": "stale salt",
        });

        let actual = derive_master_key_from_config(&config).unwrap();
        assert_eq!(actual.as_bytes(), expected.as_bytes());
    }

    /// Create a TcfsProvider backed by opendal::services::Memory (no network).
    fn memory_provider(prefix: &str) -> *mut TcfsProvider {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");

        let operator = opendal::Operator::new(opendal::services::Memory::default())
            .expect("memory operator")
            .finish();

        let provider = TcfsProvider {
            runtime,
            operator,
            remote_prefix: prefix.to_string(),
            device_id: "test-device".to_string(),
            master_key: None,
            last_error: Mutex::new(None),
        };

        Box::into_raw(Box::new(provider))
    }

    /// Helper: call enumerate and return (items_ptr, count).
    /// Caller must free items via tcfs_file_items_free.
    unsafe fn enumerate(
        prov: *mut TcfsProvider,
        path: &str,
    ) -> (TcfsError, *mut TcfsFileItem, usize) {
        let c_path = CString::new(path).unwrap();
        let mut items: *mut TcfsFileItem = ptr::null_mut();
        let mut count: usize = 0;
        let err = tcfs_provider_enumerate(prov, c_path.as_ptr(), &mut items, &mut count);
        (err, items, count)
    }

    // ── Enumerate ────────────────────────────────────────────────────────

    #[test]
    fn enumerate_empty_returns_zero() {
        let prov = memory_provider("test");
        unsafe {
            let (err, items, count) = enumerate(prov, "");
            assert_eq!(err, TcfsError::TcfsErrorNone);
            assert_eq!(count, 0);
            crate::tcfs_file_items_free(items, count);
            tcfs_provider_free(prov);
        }
    }

    // ── Upload + Enumerate ───────────────────────────────────────────────

    #[test]
    fn upload_then_enumerate() {
        let prov = memory_provider("pfx");
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("hello.txt");
        std::fs::write(&file, b"Hello, TCFS!").unwrap();

        unsafe {
            let c_local = CString::new(file.to_str().unwrap()).unwrap();
            let c_remote = CString::new("hello.txt").unwrap();
            let err = tcfs_provider_upload(prov, c_local.as_ptr(), c_remote.as_ptr());
            assert_eq!(err, TcfsError::TcfsErrorNone);

            // Enumerate root should show the file
            let (err, items, count) = enumerate(prov, "");
            assert_eq!(err, TcfsError::TcfsErrorNone);
            assert_eq!(count, 1);

            let item = &*items;
            let name = CStr::from_ptr(item.filename).to_str().unwrap();
            assert_eq!(name, "hello.txt");
            assert!(!item.is_directory);
            assert_eq!(item.file_size, b"Hello, TCFS!".len() as u64);

            crate::tcfs_file_items_free(items, count);
            tcfs_provider_free(prov);
        }
    }

    // ── Upload + Fetch roundtrip ─────────────────────────────────────────

    #[test]
    fn upload_then_fetch_roundtrip() {
        let prov = memory_provider("rt");
        let dir = tempfile::tempdir().unwrap();

        // Write source file
        let src = dir.path().join("source.bin");
        let data = b"binary content for roundtrip test";
        std::fs::write(&src, data).unwrap();

        // Upload
        unsafe {
            let c_local = CString::new(src.to_str().unwrap()).unwrap();
            let c_remote = CString::new("source.bin").unwrap();
            let err = tcfs_provider_upload(prov, c_local.as_ptr(), c_remote.as_ptr());
            assert_eq!(err, TcfsError::TcfsErrorNone);

            // Fetch to a different location
            let dest = dir.path().join("fetched.bin");
            let c_item = CString::new("source.bin").unwrap();
            let c_dest = CString::new(dest.to_str().unwrap()).unwrap();
            let err = tcfs_provider_fetch(prov, c_item.as_ptr(), c_dest.as_ptr());
            assert_eq!(err, TcfsError::TcfsErrorNone);

            // Verify content matches
            let fetched = std::fs::read(&dest).unwrap();
            assert_eq!(&fetched, data);

            tcfs_provider_free(prov);
        }
    }

    // ── Upload + Delete ──────────────────────────────────────────────────

    #[test]
    fn upload_then_delete() {
        let prov = memory_provider("del");
        let dir = tempfile::tempdir().unwrap();

        let file = dir.path().join("to_delete.txt");
        std::fs::write(&file, b"delete me").unwrap();

        unsafe {
            let c_local = CString::new(file.to_str().unwrap()).unwrap();
            let c_remote = CString::new("to_delete.txt").unwrap();
            let err = tcfs_provider_upload(prov, c_local.as_ptr(), c_remote.as_ptr());
            assert_eq!(err, TcfsError::TcfsErrorNone);

            // Verify it exists
            let (_, _, count) = enumerate(prov, "");
            assert_eq!(count, 1);

            // Delete
            let c_item = CString::new("to_delete.txt").unwrap();
            let err = tcfs_provider_delete(prov, c_item.as_ptr());
            assert_eq!(err, TcfsError::TcfsErrorNone);

            // Should be gone
            let (_, items, count) = enumerate(prov, "");
            assert_eq!(count, 0);
            crate::tcfs_file_items_free(items, count);

            tcfs_provider_free(prov);
        }
    }

    // ── Create directory ─────────────────────────────────────────────────
    // Note: create_dir writes trailing-slash keys which the Memory backend
    // does not support (it rejects directory-like paths). S3 backends
    // handle this correctly. The null-safety tests in ffi_safety_test.rs
    // cover the FFI boundary; functional coverage requires an S3 backend.

    // ── Multiple files ──────────────────────────────────────────────────

    #[test]
    fn enumerate_multiple_files() {
        let prov = memory_provider("multi");
        let dir = tempfile::tempdir().unwrap();

        std::fs::write(dir.path().join("a.txt"), b"aaa").unwrap();
        std::fs::write(dir.path().join("b.txt"), b"bbb").unwrap();
        std::fs::write(dir.path().join("c.txt"), b"ccc").unwrap();

        unsafe {
            for name in &["a.txt", "b.txt", "c.txt"] {
                let local = dir.path().join(name);
                let c_local = CString::new(local.to_str().unwrap()).unwrap();
                let c_remote = CString::new(*name).unwrap();
                let err = tcfs_provider_upload(prov, c_local.as_ptr(), c_remote.as_ptr());
                assert_eq!(err, TcfsError::TcfsErrorNone);
            }

            let (err, items, count) = enumerate(prov, "");
            assert_eq!(err, TcfsError::TcfsErrorNone);
            assert_eq!(count, 3);

            // Collect names
            let mut names: Vec<String> = Vec::new();
            for i in 0..count {
                let item = &*items.add(i);
                names.push(CStr::from_ptr(item.filename).to_str().unwrap().to_string());
            }
            names.sort();
            assert_eq!(names, vec!["a.txt", "b.txt", "c.txt"]);

            crate::tcfs_file_items_free(items, count);
            tcfs_provider_free(prov);
        }
    }

    // ── Fetch nonexistent file ───────────────────────────────────────────

    #[test]
    fn fetch_nonexistent_errors() {
        let prov = memory_provider("miss");
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("output.bin");

        unsafe {
            let c_item = CString::new("does-not-exist.txt").unwrap();
            let c_dest = CString::new(dest.to_str().unwrap()).unwrap();
            let err = tcfs_provider_fetch(prov, c_item.as_ptr(), c_dest.as_ptr());
            // Should fail (storage error or not found)
            assert_ne!(err, TcfsError::TcfsErrorNone);
            let last_error = tcfs_provider_last_error(prov);
            assert!(!last_error.is_null());
            let message = CStr::from_ptr(last_error).to_string_lossy();
            assert!(
                message.contains("does-not-exist.txt") || !message.is_empty(),
                "last error should describe the failed fetch"
            );
            crate::tcfs_string_free(last_error);
            // Destination should not have been created
            assert!(!dest.exists());

            tcfs_provider_free(prov);
        }
    }

    // ── Enumerate changes (always empty for direct backend) ──────────────

    #[test]
    fn enumerate_changes_returns_empty() {
        let prov = memory_provider("chg");
        unsafe {
            let c_path = CString::new("").unwrap();
            let mut events: *mut crate::TcfsChangeEvent = ptr::null_mut();
            let mut count: usize = 0;
            let err =
                tcfs_provider_enumerate_changes(prov, c_path.as_ptr(), 0, &mut events, &mut count);
            assert_eq!(err, TcfsError::TcfsErrorNone);
            assert_eq!(count, 0);

            crate::tcfs_change_events_free(events, count);
            tcfs_provider_free(prov);
        }
    }

    // ── Fetch with progress callback ─────────────────────────────────────

    #[test]
    fn fetch_with_progress_reports_bytes() {
        let prov = memory_provider("prog");
        let dir = tempfile::tempdir().unwrap();

        // Upload a file first
        let src = dir.path().join("progress_test.bin");
        std::fs::write(&src, vec![0xAA; 4096]).unwrap();

        unsafe {
            let c_local = CString::new(src.to_str().unwrap()).unwrap();
            let c_remote = CString::new("progress_test.bin").unwrap();
            let err = tcfs_provider_upload(prov, c_local.as_ptr(), c_remote.as_ptr());
            assert_eq!(err, TcfsError::TcfsErrorNone);

            // Fetch with progress callback — use atomic counters to avoid static mut
            use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
            static PROGRESS_CALLED: AtomicBool = AtomicBool::new(false);
            static LAST_TOTAL: AtomicU64 = AtomicU64::new(0);

            unsafe extern "C" fn on_progress(
                _completed: u64,
                total: u64,
                _context: *const std::ffi::c_void,
            ) {
                PROGRESS_CALLED.store(true, Ordering::SeqCst);
                LAST_TOTAL.store(total, Ordering::SeqCst);
            }

            let dest = dir.path().join("progress_out.bin");
            let c_item = CString::new("progress_test.bin").unwrap();
            let c_dest = CString::new(dest.to_str().unwrap()).unwrap();
            let err = tcfs_provider_fetch_with_progress(
                prov,
                c_item.as_ptr(),
                c_dest.as_ptr(),
                Some(on_progress),
                ptr::null(),
            );
            assert_eq!(err, TcfsError::TcfsErrorNone);
            assert!(
                PROGRESS_CALLED.load(Ordering::SeqCst),
                "progress callback should have been called"
            );
            assert_eq!(
                LAST_TOTAL.load(Ordering::SeqCst),
                4096,
                "total should match file size"
            );

            // Verify data
            let fetched = std::fs::read(&dest).unwrap();
            assert_eq!(fetched.len(), 4096);

            tcfs_provider_free(prov);
        }
    }

    // ── Subdirectory enumeration ─────────────────────────────────────────

    #[test]
    fn upload_nested_then_enumerate_parent() {
        let prov = memory_provider("nest");
        let dir = tempfile::tempdir().unwrap();

        let file = dir.path().join("readme.md");
        std::fs::write(&file, b"# Docs").unwrap();

        unsafe {
            // Upload into a subdirectory
            let c_local = CString::new(file.to_str().unwrap()).unwrap();
            let c_remote = CString::new("docs/readme.md").unwrap();
            let err = tcfs_provider_upload(prov, c_local.as_ptr(), c_remote.as_ptr());
            assert_eq!(err, TcfsError::TcfsErrorNone);

            // Enumerate root — should show "docs" as a directory
            let (err, items, count) = enumerate(prov, "");
            assert_eq!(err, TcfsError::TcfsErrorNone);
            assert_eq!(count, 1);

            let item = &*items;
            let name = CStr::from_ptr(item.filename).to_str().unwrap();
            assert_eq!(name, "docs");
            assert!(item.is_directory);

            crate::tcfs_file_items_free(items, count);

            // Enumerate docs/ — should show the file
            let (err, items, count) = enumerate(prov, "docs");
            assert_eq!(err, TcfsError::TcfsErrorNone);
            assert_eq!(count, 1);

            let item = &*items;
            let name = CStr::from_ptr(item.filename).to_str().unwrap();
            assert_eq!(name, "readme.md");
            assert!(!item.is_directory);

            crate::tcfs_file_items_free(items, count);
            tcfs_provider_free(prov);
        }
    }
}
