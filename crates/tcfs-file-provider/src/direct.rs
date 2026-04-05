//! Direct S3/OpenDAL backend for the FileProvider FFI.
//!
//! Talks directly to SeaweedFS/S3 without going through the daemon.
//! This is the original backend — no fleet sync, no NATS events.

use std::ffi::CStr;
use std::os::raw::c_char;
use std::panic::AssertUnwindSafe;
use std::ptr;

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

        // Derive master key from encryption_passphrase if provided (enables E2EE)
        let master_key = config["encryption_passphrase"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|passphrase| {
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

                tcfs_crypto::derive_master_key(
                    &SecretString::from(passphrase.to_string()),
                    &salt,
                    &params,
                )
                .ok()
            })
            .flatten();

        let runtime = match tokio::runtime::Runtime::new() {
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

            // item_id is the relative path (e.g. "ansible/roles") — NOT the
            // full S3 key. Swift uses item_id as containerIdentifier.rawValue
            // which becomes rel_path on the next enumerate call. Using the
            // full S3 key would cause double-prefixing.
            items.push(TcfsFileItem {
                item_id: to_c_string(&item_id),
                filename: to_c_string(child_name),
                file_size: if is_dir {
                    0
                } else {
                    entry.metadata().content_length()
                },
                modified_timestamp: 0,
                is_directory: is_dir,
                content_hash: to_c_string(""),
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
            let text = String::from_utf8_lossy(&bytes);

            let mut manifest_hash = String::new();
            for line in text.lines() {
                if let Some(val) = line.strip_prefix("manifest_hash=") {
                    manifest_hash = val.to_string();
                }
            }

            if manifest_hash.is_empty() {
                anyhow::bail!("no manifest_hash in index entry");
            }

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
            Err(_) => TcfsError::TcfsErrorStorage,
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
                let existing_text = String::from_utf8_lossy(&existing_bytes);
                for line in existing_text.lines() {
                    if let Some(hash) = line.strip_prefix("manifest_hash=") {
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
            let index_entry = format!(
                "manifest_hash={}\nsize={}\nchunks={}\n",
                file_hash,
                data.len(),
                chunks.len()
            );
            prov.operator.write(&index_key, index_entry).await?;

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
                let text = String::from_utf8_lossy(&bytes);
                for line in text.lines() {
                    if let Some(hash) = line.strip_prefix("manifest_hash=") {
                        let manifest_path = format!(
                            "{}/manifests/{}",
                            prov.remote_prefix.trim_end_matches('/'),
                            hash
                        );
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
