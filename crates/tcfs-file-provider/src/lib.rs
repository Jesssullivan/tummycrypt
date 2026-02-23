//! tcfs-file-provider: C FFI bridge for macOS/iOS FileProvider extensions via cbindgen
//!
//! This crate exposes tcfs storage, chunking, and sync operations
//! as a C-compatible FFI layer via cbindgen, enabling Swift consumers
//! to build native FileProvider extensions (.appex).
//!
//! ## Architecture
//!
//! ```text
//! iOS Files App / macOS Finder
//!       |
//!       +-- NSFileProviderReplicatedExtension (Swift ~200 LOC)
//!       |         |
//!       |         +-- C header (tcfs_file_provider.h, cbindgen-generated)
//!       |                   |
//!       +-- tcfs-file-provider (this crate, staticlib)
//!                   |
//!                   +-- tcfs-storage  -> S3/SeaweedFS access
//!                   +-- tcfs-chunks   -> FastCDC + BLAKE3 + zstd
//!                   +-- tcfs-sync     -> state cache, manifests
//!                   +-- tcfs-core     -> config, proto types
//! ```
//!
//! ## Status
//!
//! Initial FFI skeleton -- see [RFC 0002](../../docs/rfc/0002-darwin-integration.md)
//! and [RFC 0003](../../docs/rfc/0003-ios-file-provider.md) for roadmap.

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::panic::AssertUnwindSafe;
use std::ptr;

/// Error codes returned by FFI functions.
#[repr(C)]
pub enum TcfsError {
    /// Success (no error).
    TcfsErrorNone = 0,
    /// Invalid argument (null pointer, bad JSON, etc.).
    TcfsErrorInvalidArg = 1,
    /// Storage/network error communicating with S3/SeaweedFS.
    TcfsErrorStorage = 2,
    /// File or item not found.
    TcfsErrorNotFound = 3,
    /// Internal error (panic caught, unexpected state).
    TcfsErrorInternal = 4,
}

/// A file item returned by directory enumeration.
///
/// The Swift layer reads these fields and maps them to
/// `NSFileProviderItem` properties.
#[repr(C)]
pub struct TcfsFileItem {
    /// Unique item identifier (UTF-8 C string, caller must free via `tcfs_string_free`).
    pub item_id: *mut c_char,
    /// Display filename (UTF-8 C string).
    pub filename: *mut c_char,
    /// File size in bytes.
    pub file_size: u64,
    /// Last-modified timestamp (Unix epoch seconds).
    pub modified_timestamp: i64,
    /// Whether this item is a directory.
    pub is_directory: bool,
    /// Content hash (BLAKE3 hex, UTF-8 C string).
    pub content_hash: *mut c_char,
}

/// Opaque provider handle wrapping a tokio runtime + OpenDAL operator.
///
/// Created via `tcfs_provider_new`, freed via `tcfs_provider_free`.
pub struct TcfsProvider {
    runtime: tokio::runtime::Runtime,
    operator: opendal::Operator,
    remote_prefix: String,
}

/// Create a new provider from a JSON configuration string.
///
/// The JSON should contain:
/// ```json
/// {
///   "s3_endpoint": "http://...",
///   "s3_bucket": "tcfs",
///   "s3_access": "...",
///   "s3_secret": "...",
///   "remote_prefix": "devices/mydevice"
/// }
/// ```
///
/// Returns a pointer to `TcfsProvider` on success, or null on failure.
/// The caller must free the provider via `tcfs_provider_free`.
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
        }))
    }));

    result.unwrap_or(ptr::null_mut())
}

/// Enumerate files under a relative path within the remote prefix.
///
/// On success, writes an array of `TcfsFileItem` to `*out_items` and the
/// count to `*out_count`. The caller must free the items via
/// `tcfs_file_items_free`.
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

        let prefix = format!(
            "{}/index/{}",
            prov.remote_prefix.trim_end_matches('/'),
            rel_path.trim_start_matches('/')
        );

        let entries = match prov.runtime.block_on(prov.operator.list(&prefix)) {
            Ok(e) => e,
            Err(_) => return TcfsError::TcfsErrorStorage,
        };

        let mut items: Vec<TcfsFileItem> = Vec::new();
        for entry in entries {
            let entry_path = entry.path();
            let name = entry_path
                .strip_prefix(&prefix)
                .unwrap_or(entry_path)
                .trim_start_matches('/');

            if name.is_empty() {
                continue;
            }

            let is_dir = name.ends_with('/');
            let display_name = name.trim_end_matches('/');

            items.push(TcfsFileItem {
                item_id: to_c_string(entry_path),
                filename: to_c_string(display_name),
                file_size: entry.metadata().content_length(),
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
/// Downloads chunks from S3, reassembles with integrity verification,
/// and writes the result to `dest_path`.
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
            // Read the index entry to get manifest hash
            let data = prov.operator.read(item_str).await?;
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

            let mut assembled = Vec::new();
            for hash in manifest.chunk_hashes() {
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
                assembled.extend_from_slice(&chunk_bytes);
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
/// Chunks the file with FastCDC, hashes with BLAKE3, uploads chunks
/// and manifest to S3.
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

            let chunks = tcfs_chunks::chunk_data(&data, tcfs_chunks::ChunkSizes::SMALL);
            let mut chunk_hashes = Vec::new();

            for chunk in &chunks {
                let chunk_bytes =
                    &data[chunk.offset as usize..chunk.offset as usize + chunk.length];
                let hash = tcfs_chunks::hash_to_hex(&chunk.hash);
                let chunk_key = format!(
                    "{}/chunks/{}",
                    prov.remote_prefix.trim_end_matches('/'),
                    hash
                );
                prov.operator
                    .write(&chunk_key, chunk_bytes.to_vec())
                    .await?;
                chunk_hashes.push(hash);
            }

            let manifest = tcfs_sync::manifest::SyncManifest {
                version: 2,
                file_hash: file_hash.clone(),
                file_size: data.len() as u64,
                chunks: chunk_hashes,
                vclock: Default::default(),
                written_by: String::new(),
                written_at: 0,
                rel_path: Some(remote_str.to_string()),
                encrypted_file_key: None,
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

/// Free a provider handle.
///
/// # Safety
///
/// `provider` must be a valid pointer from `tcfs_provider_new`, or null (no-op).
/// Must not be called more than once for the same pointer.
#[no_mangle]
pub unsafe extern "C" fn tcfs_provider_free(provider: *mut TcfsProvider) {
    if !provider.is_null() {
        unsafe {
            drop(Box::from_raw(provider));
        }
    }
}

/// Free an array of `TcfsFileItem` returned by `tcfs_provider_enumerate`.
///
/// # Safety
///
/// - `items` must be a pointer returned by `tcfs_provider_enumerate`, or null.
/// - `count` must match the count returned by the same call.
#[no_mangle]
pub unsafe extern "C" fn tcfs_file_items_free(items: *mut TcfsFileItem, count: usize) {
    if items.is_null() || count == 0 {
        return;
    }

    unsafe {
        let slice = std::slice::from_raw_parts_mut(items, count);
        for item in slice.iter_mut() {
            free_c_string(item.item_id);
            free_c_string(item.filename);
            free_c_string(item.content_hash);
        }
        // Reconstruct the Box<[TcfsFileItem]> to drop it
        let _ = Box::from_raw(std::ptr::slice_from_raw_parts_mut(items, count));
    }
}

/// Free a C string allocated by this crate.
///
/// # Safety
///
/// `s` must be a pointer returned by an FFI function in this crate, or null.
#[no_mangle]
pub unsafe extern "C" fn tcfs_string_free(s: *mut c_char) {
    free_c_string(s);
}

// --- Internal helpers ---

fn to_c_string(s: &str) -> *mut c_char {
    CString::new(s)
        .unwrap_or_else(|_| CString::new("").unwrap())
        .into_raw()
}

unsafe fn free_c_string(s: *mut c_char) {
    if !s.is_null() {
        unsafe {
            drop(CString::from_raw(s));
        }
    }
}
