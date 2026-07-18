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

use anyhow::Context;
use base64::Engine;
use secrecy::SecretString;

use crate::{to_c_string, TcfsError, TcfsFileItem};

const FILE_PROVIDER_READ_ONLY_ERROR: &str =
    "TCFS FileProvider is read-only until exact version-token conditional publication is available";

/// Opaque provider handle wrapping a tokio runtime + OpenDAL operator.
///
/// Created via `tcfs_provider_new`, freed via `tcfs_provider_free`.
pub struct TcfsProvider {
    runtime: tokio::runtime::Runtime,
    operator: opendal::Operator,
    remote_prefix: String,
    /// Retained by the test-only publication harness while the production
    /// FileProvider mutation surface is deliberately read-only.
    #[cfg_attr(not(test), allow(dead_code))]
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

    fn reject_file_provider_mutation(&self) -> TcfsError {
        self.clear_last_error();
        self.set_last_error(FILE_PROVIDER_READ_ONLY_ERROR);
        TcfsError::TcfsErrorConflict
    }
}

fn parse_visible_entry_from_index(
    bytes: &[u8],
) -> anyhow::Result<Option<tcfs_sync::index_entry::RemoteIndexEntry>> {
    let record = tcfs_sync::index_entry::parse_index_entry_record(bytes)?;
    Ok(record.visible_entry().cloned())
}

fn validate_assembled_file(
    manifest: &tcfs_sync::manifest::SyncManifest,
    assembled: &[u8],
) -> anyhow::Result<()> {
    anyhow::ensure!(
        assembled.len() as u64 == manifest.file_size,
        "assembled file size mismatch: expected {}, got {}",
        manifest.file_size,
        assembled.len()
    );
    let actual = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(assembled));
    anyhow::ensure!(
        actual == manifest.file_hash,
        "assembled file integrity failure: expected {}, got {}",
        manifest.file_hash,
        actual
    );
    Ok(())
}

fn visible_entry_from_index(
    prov: &TcfsProvider,
    index_key: &str,
) -> anyhow::Result<Option<tcfs_sync::index_entry::RemoteIndexEntry>> {
    prov.runtime.block_on(async {
        let data = prov.operator.read(index_key).await?;
        let bytes = data.to_bytes();
        parse_visible_entry_from_index(&bytes)
    })
}

/// Return whether an index subtree contains any logically visible entry.
///
/// Version-4 tombstones intentionally retain their object keys, so a physical
/// listing prefix alone is not evidence that FileProvider should expose a
/// directory. Preparing entries retain in-flight authority and keep their
/// ancestors discoverable until publication resolves.
async fn index_prefix_has_visible_entries(
    operator: &opendal::Operator,
    index_prefix: &str,
) -> anyhow::Result<bool> {
    let entries = operator
        .list_with(index_prefix)
        .recursive(true)
        .await
        .with_context(|| format!("listing logical index subtree: {index_prefix}"))?;

    for entry in entries {
        let key = entry.path();
        if key.ends_with('/') {
            continue;
        }
        if key.ends_with("/.tcfs_dir") {
            if tcfs_sync::index_entry::directory_marker_is_visible(operator, key).await? {
                return Ok(true);
            }
            continue;
        }
        let Some(record) =
            tcfs_sync::index_entry::read_index_entry_record_from_store(operator, key).await?
        else {
            continue;
        };
        if record.visible_entry().is_some() || record.pending_entry().is_some() {
            return Ok(true);
        }
    }

    Ok(false)
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

fn required_encryption_flag(config: &serde_json::Value) -> anyhow::Result<bool> {
    for key in [
        "encryption_required",
        "require_encryption",
        "crypto_required",
    ] {
        let Some(value) = config.get(key) else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        let enabled = value
            .as_bool()
            .with_context(|| format!("{key} must be a boolean"))?;
        if enabled {
            return Ok(true);
        }
    }

    for section_name in ["crypto", "encryption"] {
        let Some(section) = config.get(section_name) else {
            continue;
        };
        match section {
            serde_json::Value::Null | serde_json::Value::Bool(false) => {}
            serde_json::Value::Bool(true) => return Ok(true),
            serde_json::Value::String(mode)
                if matches!(mode.as_str(), "plaintext" | "disabled" | "none") => {}
            serde_json::Value::String(mode) if matches!(mode.as_str(), "required" | "enabled") => {
                return Ok(true);
            }
            serde_json::Value::Object(values) => {
                for key in ["required", "enabled"] {
                    let Some(value) = values.get(key) else {
                        continue;
                    };
                    let enabled = value
                        .as_bool()
                        .with_context(|| format!("{section_name}.{key} must be a boolean"))?;
                    if enabled {
                        return Ok(true);
                    }
                }
            }
            _ => anyhow::bail!("{section_name} must be a boolean, a supported mode, or an object"),
        }
    }
    Ok(false)
}

fn configured_nonempty_string<'a>(
    config: &'a serde_json::Value,
    key: &str,
) -> anyhow::Result<Option<&'a str>> {
    let Some(value) = config.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let value = value
        .as_str()
        .with_context(|| format!("{key} must be a string"))?;
    if value.is_empty() {
        return Ok(None);
    }
    anyhow::ensure!(
        !value.trim().is_empty(),
        "{key} must not be whitespace-only"
    );
    Ok(Some(value))
}

fn configured_u32(config: &serde_json::Value, key: &str, default: u32) -> anyhow::Result<u32> {
    let Some(value) = config.get(key) else {
        return Ok(default);
    };
    if value.is_null() {
        return Ok(default);
    }
    let value = value
        .as_u64()
        .with_context(|| format!("{key} must be an unsigned integer"))?;
    u32::try_from(value).with_context(|| format!("{key} exceeds u32 range"))
}

fn derive_master_key_from_config(
    config: &serde_json::Value,
) -> anyhow::Result<Option<tcfs_crypto::MasterKey>> {
    let encryption_required = required_encryption_flag(config)?;

    if let Some(encoded) = configured_nonempty_string(config, "master_key_base64")? {
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .context("decoding master_key_base64")?;
        return master_key_from_bytes(&decoded)
            .context("validating master_key_base64")
            .map(Some);
    }

    if let Some(path) = configured_nonempty_string(config, "master_key_file")? {
        let bytes = std::fs::read(path)
            .with_context(|| format!("reading configured master_key_file: {path}"))?;
        return master_key_from_bytes(&bytes)
            .with_context(|| format!("validating configured master_key_file: {path}"))
            .map(Some);
    }

    let Some(passphrase) = configured_nonempty_string(config, "encryption_passphrase")? else {
        anyhow::ensure!(
            !encryption_required,
            "encryption is required but no master key or passphrase is configured"
        );
        return Ok(None);
    };

    if passphrase.split_whitespace().count() >= 12 {
        return tcfs_crypto::mnemonic_to_master_key(passphrase)
            .context("deriving master key from configured mnemonic")
            .map(Some);
    }

    let salt_str = match config.get("encryption_salt") {
        None | Some(serde_json::Value::Null) => "tcfs-default-salt!",
        Some(value) => value.as_str().context("encryption_salt must be a string")?,
    };
    let mut salt = [0u8; 16];
    let salt_bytes = salt_str.as_bytes();
    let copy_len = salt_bytes.len().min(16);
    salt[..copy_len].copy_from_slice(&salt_bytes[..copy_len]);

    let params = tcfs_crypto::kdf::KdfParams {
        mem_cost_kib: configured_u32(config, "argon2_mem_cost_kib", 65536)?,
        time_cost: configured_u32(config, "argon2_time_cost", 3)?,
        parallelism: configured_u32(config, "argon2_parallelism", 4)?,
    };

    tcfs_crypto::derive_master_key(&SecretString::from(passphrase.to_string()), &salt, &params)
        .context("deriving master key from configured passphrase")
        .map(Some)
}

fn error_code_for_fetch_error(error: &anyhow::Error) -> TcfsError {
    if crate::is_file_provider_version_mismatch(error) {
        TcfsError::TcfsErrorVersionMismatch
    } else if error.chain().any(|cause| {
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
        let master_key = match derive_master_key_from_config(&config) {
            Ok(master_key) => master_key,
            Err(error) => {
                tracing::error!(error = %format!("{error:#}"), "refusing invalid FileProvider encryption configuration");
                return ptr::null_mut();
            }
        };
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

        let operator = crate::storage_bounds::build_operator_from_json(&config);

        let operator = match operator {
            Some(op) => op,
            None => return ptr::null_mut(),
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
            // The reserved marker makes its parent directory visible; it is
            // never itself a FileProvider child.
            if remainder == ".tcfs_dir" {
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

            let (file_size, version_token) = if is_dir {
                let child_prefix = format!("{prefix}{child_name}/");
                match prov.runtime.block_on(index_prefix_has_visible_entries(
                    &prov.operator,
                    &child_prefix,
                )) {
                    Ok(true) => (0, String::new()),
                    Ok(false) => continue,
                    Err(error) => {
                        let message = format!(
                            "checking visible FileProvider directory {child_prefix}: {error:#}"
                        );
                        tracing::error!(error = %message, "FileProvider enumeration failed closed");
                        prov.set_last_error(message);
                        return TcfsError::TcfsErrorStorage;
                    }
                }
            } else {
                match visible_entry_from_index(prov, entry_path) {
                    Ok(Some(entry)) => (entry.size, entry.manifest_hash),
                    Ok(None) => continue,
                    Err(error) => {
                        let message =
                            format!("reading visible index entry {entry_path}: {error:#}");
                        tracing::error!(error = %message, "FileProvider enumeration failed closed");
                        prov.set_last_error(message);
                        return TcfsError::TcfsErrorStorage;
                    }
                }
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
                // The Apple item version is the immutable manifest object ID,
                // not the plaintext content hash. Metadata-only manifest
                // changes must produce a distinct FileProvider version.
                content_hash: to_c_string(&version_token),
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
    unsafe { tcfs_provider_fetch_versioned_impl(provider, item_id, dest_path, ptr::null()) }
}

/// Fetch a file only if `requested_version` still names the manifest selected
/// by the exact current index entry. An empty token requests the current
/// version; a non-empty stale token fails before reading manifest or chunk data.
///
/// # Safety
///
/// All pointers must be valid null-terminated UTF-8 strings. `requested_version`
/// may be null to request the current version without a conditional check.
#[no_mangle]
pub unsafe extern "C" fn tcfs_provider_fetch_versioned(
    provider: *mut TcfsProvider,
    item_id: *const c_char,
    dest_path: *const c_char,
    requested_version: *const c_char,
) -> TcfsError {
    unsafe { tcfs_provider_fetch_versioned_impl(provider, item_id, dest_path, requested_version) }
}

unsafe fn tcfs_provider_fetch_versioned_impl(
    provider: *mut TcfsProvider,
    item_id: *const c_char,
    dest_path: *const c_char,
    requested_version: *const c_char,
) -> TcfsError {
    if provider.is_null() || item_id.is_null() || dest_path.is_null() {
        return TcfsError::TcfsErrorInvalidArg;
    }

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let prov = unsafe { &*provider };
        let c_item = unsafe { CStr::from_ptr(item_id) };
        let c_dest = unsafe { CStr::from_ptr(dest_path) };
        let c_requested_version = if requested_version.is_null() {
            None
        } else {
            Some(unsafe { CStr::from_ptr(requested_version) })
        };
        prov.clear_last_error();

        let item_str = match c_item.to_str() {
            Ok(s) => s,
            Err(_) => return TcfsError::TcfsErrorInvalidArg,
        };
        let dest_str = match c_dest.to_str() {
            Ok(s) => s,
            Err(_) => return TcfsError::TcfsErrorInvalidArg,
        };
        let requested_version = match c_requested_version.map(CStr::to_str).transpose() {
            Ok(version) => version.filter(|version| !version.is_empty()),
            Err(_) => return TcfsError::TcfsErrorInvalidArg,
        };

        let fetch_result = prov.runtime.block_on(async {
            // Reconstruct full S3 key from relative item_id
            let index_key = format!(
                "{}/index/{}",
                prov.remote_prefix.trim_end_matches('/'),
                item_str.trim_start_matches('/')
            );
            let rel_path = item_str.trim_start_matches('/');
            if requested_version.is_some() {
                let selected = tcfs_sync::engine::read_exact_visible_index_selection(
                    &prov.operator,
                    rel_path,
                    prov.remote_prefix.trim_end_matches('/'),
                )
                .await?
                .ok_or_else(|| {
                    opendal::Error::new(
                        opendal::ErrorKind::NotFound,
                        "no exact visible index version for FileProvider item",
                    )
                    .with_context("path", item_str.to_string())
                })?;
                crate::ensure_file_provider_version(requested_version, &selected.manifest_hash)?;
            }
            // Read the index entry to get the bound manifest object.
            let manifest_prefix = format!("{}/manifests", prov.remote_prefix.trim_end_matches('/'));
            let index_entry = tcfs_sync::index_entry::resolve_visible_index_entry(
                &prov.operator,
                &index_key,
                &manifest_prefix,
            )
            .await?
            .ok_or_else(|| {
                opendal::Error::new(
                    opendal::ErrorKind::NotFound,
                    "no visible committed manifest for FileProvider item",
                )
                .with_context("path", item_str.to_string())
            })?;

            crate::ensure_file_provider_version(requested_version, &index_entry.manifest_hash)?;

            let manifest_path = format!(
                "{}/manifests/{}",
                prov.remote_prefix.trim_end_matches('/'),
                index_entry.manifest_hash
            );

            let manifest_bytes = prov.operator.read(&manifest_path).await?;
            let manifest_bytes = manifest_bytes.to_bytes();
            tcfs_sync::engine::validate_indexed_manifest_entry_binding(
                &manifest_bytes,
                &index_entry.manifest_hash,
                &index_entry,
                rel_path,
            )?;
            let manifest = tcfs_sync::manifest::SyncManifest::from_bytes(&manifest_bytes)?;

            // Fail CLOSED on PerDevice/v3 manifests: this direct backend only
            // unwraps master-wrapped keys. A `wrapped_file_keys` manifest with no
            // master wrap would otherwise fall through to "no file key" and copy
            // raw ciphertext. Dual/v2 (both wraps present) is permitted and read
            // via the master wrap below (TIN-1898, mirrors the engine switch).
            crate::device_ctx::ensure_master_decryptable(&manifest)?;

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

            validate_assembled_file(&manifest, &assembled)?;
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
    unsafe {
        tcfs_provider_fetch_versioned_with_progress_impl(
            provider,
            item_id,
            dest_path,
            ptr::null(),
            callback,
            context,
        )
    }
}

/// Progress-reporting conditional fetch; see [`tcfs_provider_fetch_versioned`].
///
/// # Safety
///
/// String pointers must be valid null-terminated UTF-8. `requested_version`
/// may be null. `context` must remain valid until this synchronous call returns.
#[no_mangle]
pub unsafe extern "C" fn tcfs_provider_fetch_versioned_with_progress(
    provider: *mut TcfsProvider,
    item_id: *const c_char,
    dest_path: *const c_char,
    requested_version: *const c_char,
    callback: TcfsProgressCallback,
    context: *const std::ffi::c_void,
) -> TcfsError {
    unsafe {
        tcfs_provider_fetch_versioned_with_progress_impl(
            provider,
            item_id,
            dest_path,
            requested_version,
            callback,
            context,
        )
    }
}

unsafe fn tcfs_provider_fetch_versioned_with_progress_impl(
    provider: *mut TcfsProvider,
    item_id: *const c_char,
    dest_path: *const c_char,
    requested_version: *const c_char,
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
        let c_requested_version = if requested_version.is_null() {
            None
        } else {
            Some(unsafe { CStr::from_ptr(requested_version) })
        };
        prov.clear_last_error();

        let item_str = match c_item.to_str() {
            Ok(s) => s,
            Err(_) => return TcfsError::TcfsErrorInvalidArg,
        };
        let dest_str = match c_dest.to_str() {
            Ok(s) => s,
            Err(_) => return TcfsError::TcfsErrorInvalidArg,
        };
        let requested_version = match c_requested_version.map(CStr::to_str).transpose() {
            Ok(version) => version.filter(|version| !version.is_empty()),
            Err(_) => return TcfsError::TcfsErrorInvalidArg,
        };

        let fetch_result = prov.runtime.block_on(async {
            let index_key = format!(
                "{}/index/{}",
                prov.remote_prefix.trim_end_matches('/'),
                item_str.trim_start_matches('/')
            );
            let rel_path = item_str.trim_start_matches('/');
            if requested_version.is_some() {
                let selected = tcfs_sync::engine::read_exact_visible_index_selection(
                    &prov.operator,
                    rel_path,
                    prov.remote_prefix.trim_end_matches('/'),
                )
                .await?
                .ok_or_else(|| {
                    opendal::Error::new(
                        opendal::ErrorKind::NotFound,
                        "no exact visible index version for FileProvider item",
                    )
                    .with_context("path", item_str.to_string())
                })?;
                crate::ensure_file_provider_version(requested_version, &selected.manifest_hash)?;
            }
            let manifest_prefix = format!("{}/manifests", prov.remote_prefix.trim_end_matches('/'));
            let index_entry = tcfs_sync::index_entry::resolve_visible_index_entry(
                &prov.operator,
                &index_key,
                &manifest_prefix,
            )
            .await?
            .ok_or_else(|| {
                opendal::Error::new(
                    opendal::ErrorKind::NotFound,
                    "no visible committed manifest for FileProvider item",
                )
                .with_context("path", item_str.to_string())
            })?;

            crate::ensure_file_provider_version(requested_version, &index_entry.manifest_hash)?;

            let manifest_path = format!(
                "{}/manifests/{}",
                prov.remote_prefix.trim_end_matches('/'),
                index_entry.manifest_hash
            );
            let manifest_bytes = prov.operator.read(&manifest_path).await?;
            let manifest_bytes = manifest_bytes.to_bytes();
            tcfs_sync::engine::validate_indexed_manifest_entry_binding(
                &manifest_bytes,
                &index_entry.manifest_hash,
                &index_entry,
                rel_path,
            )?;
            let manifest = tcfs_sync::manifest::SyncManifest::from_bytes(&manifest_bytes)?;

            // Fail CLOSED on PerDevice/v3 manifests only (see fetch path above);
            // Dual/v2 reads via the master wrap (TIN-1898).
            crate::device_ctx::ensure_master_decryptable(&manifest)?;

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

            validate_assembled_file(&manifest, &assembled)?;
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
    if unsafe { CStr::from_ptr(local_path) }.to_str().is_err()
        || unsafe { CStr::from_ptr(remote_rel) }.to_str().is_err()
    {
        return TcfsError::TcfsErrorInvalidArg;
    }

    unsafe { &*provider }.reject_file_provider_mutation()
}

#[cfg(test)]
unsafe fn tcfs_provider_upload_for_test(
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
            let rel_path = remote_str.trim_start_matches('/');
            let publish_baseline = tcfs_sync::engine::bind_indexed_publish_baseline(
                &prov.operator,
                &prov.remote_prefix,
                rel_path,
            )
            .await?;

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
            if let Some(existing_entry) = publish_baseline.current() {
                let manifest_path = format!(
                    "{}/manifests/{}",
                    prov.remote_prefix.trim_end_matches('/'),
                    existing_entry.manifest_hash
                );
                let manifest_bytes = prov.operator.read(&manifest_path).await?.to_bytes();
                tcfs_sync::engine::validate_indexed_manifest_entry_binding(
                    &manifest_bytes,
                    &existing_entry.manifest_hash,
                    existing_entry,
                    rel_path,
                )?;
                anyhow::ensure!(
                    existing_entry.kind == tcfs_sync::index_entry::RemoteEntryKind::RegularFile,
                    "cannot upload a regular file over a remote symlink index entry: {rel_path}"
                );
                let existing_manifest =
                    tcfs_sync::manifest::SyncManifest::from_bytes(&manifest_bytes)?;
                vclock.merge(&existing_manifest.vclock);
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
                rel_path: Some(rel_path.to_string()),
                mode: None,
                mtime: None,
                encrypted_file_key,
                wrapped_file_keys: Vec::new(),
            };

            let manifest_json = manifest.to_bytes()?;
            let manifest_object_id = tcfs_sync::index_entry::manifest_object_id(&manifest_json);
            let index_entry = tcfs_sync::index_entry::RemoteIndexEntry::new(
                manifest_object_id,
                data.len() as u64,
                chunks.len(),
            );
            tcfs_sync::engine::publish_indexed_manifest(
                &prov.operator,
                &prov.remote_prefix,
                rel_path,
                manifest_json,
                index_entry,
                publish_baseline,
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
    if unsafe { CStr::from_ptr(item_id) }.to_str().is_err() {
        return TcfsError::TcfsErrorInvalidArg;
    }

    unsafe { &*provider }.reject_file_provider_mutation()
}

#[cfg(test)]
unsafe fn tcfs_provider_delete_for_test(
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
            tcfs_sync::engine::delete_remote_index_entry(
                &prov.operator,
                item_str,
                &prov.remote_prefix,
            )
            .await?;
            Ok::<(), anyhow::Error>(())
        });

        match delete_result {
            Ok(()) => TcfsError::TcfsErrorNone,
            Err(_) => TcfsError::TcfsErrorStorage,
        }
    }));

    result.unwrap_or(TcfsError::TcfsErrorInternal)
}

/// Mark an item as unsynced without deleting remote storage.
///
/// The direct backend has no daemon-local materialization state to evict, so
/// this is intentionally a no-op. Swift uses this separate entry point for the
/// FileProvider "Free Up Space" action so the action cannot accidentally share
/// the destructive remote-delete path.
///
/// # Safety
///
/// - `provider` must be a valid pointer from `tcfs_provider_new`.
/// - `item_id` must be a valid null-terminated UTF-8 C string.
#[no_mangle]
pub unsafe extern "C" fn tcfs_provider_unsync(
    provider: *mut TcfsProvider,
    item_id: *const c_char,
) -> TcfsError {
    if provider.is_null() || item_id.is_null() {
        return TcfsError::TcfsErrorInvalidArg;
    }

    TcfsError::TcfsErrorNone
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
    if unsafe { CStr::from_ptr(parent_path) }.to_str().is_err()
        || unsafe { CStr::from_ptr(dir_name) }.to_str().is_err()
    {
        return TcfsError::TcfsErrorInvalidArg;
    }

    unsafe { &*provider }.reject_file_provider_mutation()
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

    fn memory_operator() -> opendal::Operator {
        let op = opendal::Operator::new(opendal::services::Memory::default())
            .expect("memory operator")
            .finish();
        tcfs_sync::index_entry::register_memory_index_emulation_for_tests(&op).unwrap();
        op
    }

    #[test]
    fn mnemonic_config_uses_recovery_derivation() {
        let (words, expected) = tcfs_crypto::generate_mnemonic().unwrap();
        let config = serde_json::json!({
            "encryption_passphrase": words,
            "encryption_salt": "ignored-for-mnemonic",
        });

        let actual = derive_master_key_from_config(&config).unwrap().unwrap();
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

        let actual = derive_master_key_from_config(&config).unwrap().unwrap();
        assert_eq!(actual.as_bytes(), expected.as_bytes());
    }

    #[test]
    fn malformed_configured_master_key_fails_before_remote_mutation() {
        let operator = memory_operator();
        let config = serde_json::json!({
            "master_key_base64": "not-valid-base64%%%",
            "encryption_required": true,
        });

        let error = derive_master_key_from_config(&config)
            .expect_err("invalid configured key must not become plaintext mode");

        assert!(format!("{error:#}").contains("decoding master_key_base64"));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        assert!(runtime.block_on(operator.list("")).unwrap().is_empty());
    }

    /// Create a TcfsProvider backed by opendal::services::Memory (no network).
    fn memory_provider(prefix: &str) -> *mut TcfsProvider {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");

        let operator = memory_operator();

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

    #[test]
    fn file_provider_mutations_fail_closed_without_remote_mutation() {
        let prov = memory_provider("guard");
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.txt");
        std::fs::write(&source, b"must never be uploaded").unwrap();

        unsafe {
            let c_local = CString::new(source.to_str().unwrap()).unwrap();
            let c_remote = CString::new("guarded.txt").unwrap();
            let error = tcfs_provider_upload(prov, c_local.as_ptr(), c_remote.as_ptr());
            assert_eq!(error, TcfsError::TcfsErrorConflict);

            let error = tcfs_provider_delete(prov, c_remote.as_ptr());
            assert_eq!(error, TcfsError::TcfsErrorConflict);

            let parent = CString::new("").unwrap();
            let child = CString::new("new-dir").unwrap();
            let error = tcfs_provider_create_dir(prov, parent.as_ptr(), child.as_ptr());
            assert_eq!(error, TcfsError::TcfsErrorConflict);

            let last_error = tcfs_provider_last_error(prov);
            assert!(!last_error.is_null());
            assert!(CStr::from_ptr(last_error)
                .to_string_lossy()
                .contains("read-only"));
            crate::tcfs_string_free(last_error);
            assert!((*prov)
                .runtime
                .block_on((*prov).operator.list(""))
                .unwrap()
                .is_empty());
            tcfs_provider_free(prov);
        }
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

    #[test]
    fn enumerate_skips_deleted_index_records_as_logical_absence() {
        let prov = memory_provider("deleted-list");
        unsafe {
            (*prov)
                .runtime
                .block_on(async {
                    (*prov)
                        .operator
                        .write(
                            "deleted-list/index/gone.txt",
                            tcfs_sync::index_entry::VersionedIndexEntry::deleted()
                                .to_json_bytes()
                                .unwrap(),
                        )
                        .await?;
                    (*prov)
                        .operator
                        .write(
                            "deleted-list/index/visible.txt",
                            tcfs_sync::index_entry::RemoteIndexEntry::new("visible-object", 7, 1)
                                .to_legacy_bytes(),
                        )
                        .await?;
                    Ok::<(), anyhow::Error>(())
                })
                .unwrap();

            let (err, items, count) = enumerate(prov, "");
            assert_eq!(err, TcfsError::TcfsErrorNone);
            assert_eq!(count, 1);
            let item = &*items;
            assert_eq!(
                CStr::from_ptr(item.filename).to_string_lossy(),
                "visible.txt"
            );
            assert_eq!(item.file_size, 7);
            assert_eq!(
                CStr::from_ptr(item.content_hash).to_string_lossy(),
                "visible-object"
            );

            crate::tcfs_file_items_free(items, count);
            tcfs_provider_free(prov);
        }
    }

    #[test]
    fn enumerate_hides_directories_backed_only_by_tombstones() {
        let prov = memory_provider("deleted-tree");
        unsafe {
            (*prov)
                .runtime
                .block_on(async {
                    let tombstone = tcfs_sync::index_entry::VersionedIndexEntry::deleted()
                        .to_json_bytes()
                        .unwrap();
                    (*prov)
                        .operator
                        .write("deleted-tree/index/ghost/gone.txt", tombstone.clone())
                        .await?;
                    (*prov)
                        .operator
                        .write("deleted-tree/index/ghost-empty/.tcfs_dir", tombstone)
                        .await?;
                    (*prov)
                        .operator
                        .write(
                            "deleted-tree/index/live/present.txt",
                            tcfs_sync::index_entry::RemoteIndexEntry::new("visible-object", 7, 1)
                                .to_legacy_bytes(),
                        )
                        .await?;
                    (*prov)
                        .operator
                        .write(
                            "deleted-tree/index/live-empty/.tcfs_dir",
                            tcfs_sync::index_entry::DIRECTORY_MARKER_BYTES.to_vec(),
                        )
                        .await?;
                    Ok::<(), anyhow::Error>(())
                })
                .unwrap();

            let (err, items, count) = enumerate(prov, "");
            assert_eq!(err, TcfsError::TcfsErrorNone);
            let mut names = (0..count)
                .map(|index| {
                    CStr::from_ptr((*items.add(index)).filename)
                        .to_string_lossy()
                        .into_owned()
                })
                .collect::<Vec<_>>();
            names.sort();
            assert_eq!(names, vec!["live", "live-empty"]);

            crate::tcfs_file_items_free(items, count);

            let (err, marker_items, marker_count) = enumerate(prov, "live-empty");
            assert_eq!(err, TcfsError::TcfsErrorNone);
            assert_eq!(marker_count, 0);
            crate::tcfs_file_items_free(marker_items, marker_count);
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
            let err = tcfs_provider_upload_for_test(prov, c_local.as_ptr(), c_remote.as_ptr());
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
            let err = tcfs_provider_upload_for_test(prov, c_local.as_ptr(), c_remote.as_ptr());
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

    #[test]
    fn versioned_fetch_rejects_stale_manifest_before_destination_io() {
        let prov = memory_provider("versioned-fetch");
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.bin");
        std::fs::write(&source, b"version-bound content").unwrap();

        unsafe {
            let c_source = CString::new(source.to_str().unwrap()).unwrap();
            let c_item = CString::new("nested/source.bin").unwrap();
            assert_eq!(
                tcfs_provider_upload_for_test(prov, c_source.as_ptr(), c_item.as_ptr()),
                TcfsError::TcfsErrorNone
            );

            let (enumerate_error, items, count) = enumerate(prov, "nested");
            assert_eq!(enumerate_error, TcfsError::TcfsErrorNone);
            assert_eq!(count, 1);
            let current_version = CStr::from_ptr((*items).content_hash)
                .to_string_lossy()
                .into_owned();
            assert!(!current_version.is_empty());
            crate::tcfs_file_items_free(items, count);

            let destination = dir.path().join("destination.bin");
            std::fs::write(&destination, b"keep-local-bytes").unwrap();
            let c_destination = CString::new(destination.to_str().unwrap()).unwrap();
            let stale = CString::new("stale-manifest-id").unwrap();
            assert_eq!(
                tcfs_provider_fetch_versioned(
                    prov,
                    c_item.as_ptr(),
                    c_destination.as_ptr(),
                    stale.as_ptr(),
                ),
                TcfsError::TcfsErrorVersionMismatch
            );
            assert_eq!(std::fs::read(&destination).unwrap(), b"keep-local-bytes");

            let current = CString::new(current_version).unwrap();
            assert_eq!(
                tcfs_provider_fetch_versioned(
                    prov,
                    c_item.as_ptr(),
                    c_destination.as_ptr(),
                    current.as_ptr(),
                ),
                TcfsError::TcfsErrorNone
            );
            assert_eq!(
                std::fs::read(destination).unwrap(),
                b"version-bound content"
            );
            tcfs_provider_free(prov);
        }
    }

    #[test]
    fn versioned_fetch_rejects_stale_before_missing_manifest_io() {
        let prov = memory_provider("versioned-missing-manifest");
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("destination.bin");
        std::fs::write(&destination, b"keep-local-bytes").unwrap();

        unsafe {
            (*prov)
                .runtime
                .block_on(
                    (*prov).operator.write(
                        "versioned-missing-manifest/index/file.bin",
                        tcfs_sync::index_entry::RemoteIndexEntry::new(
                            "missing-current-manifest",
                            7,
                            1,
                        )
                        .to_legacy_bytes(),
                    ),
                )
                .unwrap();

            let item = CString::new("file.bin").unwrap();
            let dest = CString::new(destination.to_str().unwrap()).unwrap();
            let stale = CString::new("stale-manifest").unwrap();
            assert_eq!(
                tcfs_provider_fetch_versioned(prov, item.as_ptr(), dest.as_ptr(), stale.as_ptr()),
                TcfsError::TcfsErrorVersionMismatch
            );
            assert_eq!(std::fs::read(destination).unwrap(), b"keep-local-bytes");
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
            let err = tcfs_provider_upload_for_test(prov, c_local.as_ptr(), c_remote.as_ptr());
            assert_eq!(err, TcfsError::TcfsErrorNone);

            // Verify it exists
            let (_, _, count) = enumerate(prov, "");
            assert_eq!(count, 1);

            // Delete
            let c_item = CString::new("to_delete.txt").unwrap();
            let err = tcfs_provider_delete_for_test(prov, c_item.as_ptr());
            assert_eq!(err, TcfsError::TcfsErrorNone);

            // Should be gone
            let (_, items, count) = enumerate(prov, "");
            assert_eq!(count, 0);
            crate::tcfs_file_items_free(items, count);

            tcfs_provider_free(prov);
        }
    }

    #[test]
    fn delete_one_duplicate_content_path_preserves_other_path() {
        let prov = memory_provider("dedup-delete");
        let dir = tempfile::tempdir().unwrap();

        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        let data = b"same content for both remote paths";
        std::fs::write(&a, data).unwrap();
        std::fs::write(&b, data).unwrap();

        unsafe {
            let c_a_local = CString::new(a.to_str().unwrap()).unwrap();
            let c_a_remote = CString::new("a.txt").unwrap();
            let err = tcfs_provider_upload_for_test(prov, c_a_local.as_ptr(), c_a_remote.as_ptr());
            assert_eq!(err, TcfsError::TcfsErrorNone);

            let c_b_local = CString::new(b.to_str().unwrap()).unwrap();
            let c_b_remote = CString::new("b.txt").unwrap();
            let err = tcfs_provider_upload_for_test(prov, c_b_local.as_ptr(), c_b_remote.as_ptr());
            assert_eq!(err, TcfsError::TcfsErrorNone);

            for (remote, output_name) in [
                (&c_a_remote, "a-before-delete.txt"),
                (&c_b_remote, "b-before-delete.txt"),
            ] {
                let fetched = dir.path().join(output_name);
                let c_fetched = CString::new(fetched.to_str().unwrap()).unwrap();
                let err = tcfs_provider_fetch(prov, remote.as_ptr(), c_fetched.as_ptr());
                assert_eq!(
                    err,
                    TcfsError::TcfsErrorNone,
                    "each duplicate-content path must retain its own bound manifest"
                );
                assert_eq!(std::fs::read(fetched).unwrap(), data);
            }

            let err = tcfs_provider_delete_for_test(prov, c_a_remote.as_ptr());
            assert_eq!(err, TcfsError::TcfsErrorNone);

            let fetched = dir.path().join("b-fetched.txt");
            let c_fetched = CString::new(fetched.to_str().unwrap()).unwrap();
            let err = tcfs_provider_fetch(prov, c_b_remote.as_ptr(), c_fetched.as_ptr());
            assert_eq!(err, TcfsError::TcfsErrorNone);
            assert_eq!(std::fs::read(fetched).unwrap(), data);

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
                let err = tcfs_provider_upload_for_test(prov, c_local.as_ptr(), c_remote.as_ptr());
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
            assert_eq!(err, TcfsError::TcfsErrorNotFound);
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
            let err = tcfs_provider_upload_for_test(prov, c_local.as_ptr(), c_remote.as_ptr());
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
            let err = tcfs_provider_upload_for_test(prov, c_local.as_ptr(), c_remote.as_ptr());
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

    // ── TIN-1898: Dual/master-wrap fallback on the master-only direct backend ──
    //
    // The direct backend carries no per-device age identity. Mirroring the engine
    // read switch (tcfs-sync/src/engine.rs ~:2395), a Dual/v2 manifest (BOTH a
    // master `encrypted_file_key` AND `wrapped_file_keys`) must be READABLE here
    // via the master wrap; a PerDevice/v3 manifest (wrapped_file_keys, NO master
    // wrap) must stay strictly FAIL-CLOSED; a plain master-only manifest is
    // unchanged.

    /// Build a `TcfsProvider` over a caller-supplied operator + master key so the
    /// test can seed the same in-memory store the provider reads from.
    fn master_provider_on(
        operator: opendal::Operator,
        prefix: &str,
        master_key: tcfs_crypto::MasterKey,
    ) -> *mut TcfsProvider {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        Box::into_raw(Box::new(TcfsProvider {
            runtime,
            operator,
            remote_prefix: prefix.to_string(),
            device_id: "test-device".to_string(),
            master_key: Some(master_key),
            last_error: Mutex::new(None),
        }))
    }

    /// Seed a single-chunk encrypted file under `prefix` for `rel`, returning the
    /// FileKey-derived ciphertext layout. `include_master` toggles the master
    /// `encrypted_file_key`; `include_wraps` toggles a `wrapped_file_keys` entry.
    ///
    /// Combinations:
    /// - master only       -> (true,  false): plain v2 master manifest.
    /// - dual              -> (true,  true) : v2 with BOTH wraps.
    /// - per-device / v3   -> (false, true) : wrapped_file_keys, NO master wrap.
    ///
    /// The per-device wrap is a placeholder `WrappedFileKey` — the master-only
    /// backend never attempts to unwrap it (it has no identity), exactly as in
    /// production; only its presence/absence drives the guard decision.
    fn seed_encrypted_file(
        operator: &opendal::Operator,
        prefix: &str,
        rel: &str,
        content: &[u8],
        master_key: &tcfs_crypto::MasterKey,
        include_master: bool,
        include_wraps: bool,
    ) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("seed runtime");
        rt.block_on(async {
            let file_hash = tcfs_chunks::hash_bytes(content);
            let file_hash_hex = tcfs_chunks::hash_to_hex(&file_hash);
            let file_id: [u8; 32] = *file_hash.as_bytes();

            let file_key = tcfs_crypto::generate_file_key();
            let encrypted =
                tcfs_crypto::encrypt_chunk(&file_key, 0, &file_id, content).expect("encrypt chunk");
            let chunk_hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(&encrypted));

            // chunks/<hash> = encrypted chunk bytes
            operator
                .write(
                    &format!("{}/chunks/{}", prefix.trim_end_matches('/'), chunk_hash),
                    encrypted,
                )
                .await
                .expect("write chunk");

            let encrypted_file_key = if include_master {
                let wrapped = tcfs_crypto::wrap_key(master_key, &file_key).expect("wrap master");
                Some(base64::engine::general_purpose::STANDARD.encode(wrapped))
            } else {
                None
            };
            let wrapped_file_keys = if include_wraps {
                // Placeholder per-device wrap: never unwrapped by this backend.
                vec![tcfs_sync::manifest::WrappedFileKey {
                    recipient_device_id: "device-b".to_string(),
                    recipient: "age1placeholderrecipientnotparsedbymasterbackend".to_string(),
                    algorithm: "age-x25519-file-key-v1".to_string(),
                    wrapped_key: "PLACEHOLDER-WRAP".to_string(),
                }]
            } else {
                Vec::new()
            };

            let manifest = tcfs_sync::manifest::SyncManifest {
                // v3 iff per-device-only (no master wrap); else v2.
                version: if include_wraps && !include_master {
                    3
                } else {
                    2
                },
                file_hash: file_hash_hex.clone(),
                file_size: content.len() as u64,
                chunks: vec![chunk_hash],
                vclock: tcfs_sync::conflict::VectorClock::default(),
                written_by: "device-b".to_string(),
                written_at: 0,
                rel_path: Some(rel.to_string()),
                mode: None,
                mtime: None,
                encrypted_file_key,
                wrapped_file_keys,
            };

            operator
                .write(
                    &format!(
                        "{}/manifests/{}",
                        prefix.trim_end_matches('/'),
                        file_hash_hex
                    ),
                    manifest.to_bytes().expect("manifest bytes"),
                )
                .await
                .expect("write manifest");

            let index = tcfs_sync::index_entry::RemoteIndexEntry::new(
                file_hash_hex,
                content.len() as u64,
                1,
            );
            operator
                .write(
                    &format!("{}/index/{}", prefix.trim_end_matches('/'), rel),
                    index.to_legacy_bytes(),
                )
                .await
                .expect("write index");
        });
    }

    #[test]
    fn fetch_rejects_index_size_and_chunk_count_mismatches_before_destination_write() {
        for (prefix, expected_size, expected_chunks, use_progress, expected_error) in [
            ("bad-size", 999, 1, false, "size mismatch"),
            ("bad-chunks", 7, 2, true, "chunk-count mismatch"),
        ] {
            let operator = memory_operator();
            let master = tcfs_crypto::MasterKey::from_bytes([9u8; tcfs_crypto::KEY_SIZE]);
            let content = b"payload";
            let rel_path = "bound.txt";
            seed_encrypted_file(&operator, prefix, rel_path, content, &master, true, false);

            let object_id = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(content));
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime
                .block_on(
                    operator.write(
                        &format!("{prefix}/index/{rel_path}"),
                        tcfs_sync::index_entry::RemoteIndexEntry::new(
                            object_id,
                            expected_size,
                            expected_chunks,
                        )
                        .to_legacy_bytes(),
                    ),
                )
                .unwrap();

            let provider = master_provider_on(operator, prefix, master);
            let temp = tempfile::tempdir().unwrap();
            let destination = temp.path().join("destination.txt");
            std::fs::write(&destination, b"keep-local-bytes").unwrap();
            unsafe {
                let item_id = CString::new(rel_path).unwrap();
                let destination_c = CString::new(destination.to_str().unwrap()).unwrap();
                let error = if use_progress {
                    tcfs_provider_fetch_with_progress(
                        provider,
                        item_id.as_ptr(),
                        destination_c.as_ptr(),
                        None,
                        ptr::null(),
                    )
                } else {
                    tcfs_provider_fetch(provider, item_id.as_ptr(), destination_c.as_ptr())
                };

                assert_eq!(error, TcfsError::TcfsErrorStorage);
                assert_eq!(std::fs::read(&destination).unwrap(), b"keep-local-bytes");
                let message = tcfs_provider_last_error(provider);
                assert!(!message.is_null());
                assert!(
                    CStr::from_ptr(message)
                        .to_string_lossy()
                        .contains(expected_error),
                    "expected {expected_error}"
                );
                crate::tcfs_string_free(message);
                tcfs_provider_free(provider);
            }
        }
    }

    /// A Dual/v2 manifest (master + per-device wraps) is READABLE via the
    /// master-only direct backend through the master wrap — fetch succeeds and
    /// the plaintext round-trips. This is the core TIN-1898 fix.
    #[test]
    fn dual_manifest_reads_via_master_wrap_on_direct_backend() {
        let operator = memory_operator();
        let master = tcfs_crypto::MasterKey::from_bytes([9u8; tcfs_crypto::KEY_SIZE]);
        let prefix = "dual";
        let content = b"dual manifest readable via master fallback on the FP direct backend";
        seed_encrypted_file(&operator, prefix, "dual.txt", content, &master, true, true);

        let prov = master_provider_on(operator, prefix, master);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.bin");
        unsafe {
            let c_item = CString::new("dual.txt").unwrap();
            let c_dest = CString::new(dest.to_str().unwrap()).unwrap();
            let err = tcfs_provider_fetch(prov, c_item.as_ptr(), c_dest.as_ptr());
            assert_eq!(
                err,
                TcfsError::TcfsErrorNone,
                "Dual manifest must be readable via the master wrap"
            );
            assert_eq!(std::fs::read(&dest).unwrap(), content);
            tcfs_provider_free(prov);
        }
    }

    /// A PerDevice/v3 manifest (wrapped_file_keys, NO master wrap) still FAILS
    /// CLOSED on the master-only direct backend: fetch errors and no file is
    /// materialized (never raw ciphertext).
    #[test]
    fn per_device_manifest_fails_closed_on_direct_backend() {
        let operator = memory_operator();
        let master = tcfs_crypto::MasterKey::from_bytes([9u8; tcfs_crypto::KEY_SIZE]);
        let prefix = "pd";
        let content = b"per-device-only payload that the master-only backend cannot read";
        seed_encrypted_file(&operator, prefix, "pd.txt", content, &master, false, true);

        let prov = master_provider_on(operator, prefix, master);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.bin");
        unsafe {
            let c_item = CString::new("pd.txt").unwrap();
            let c_dest = CString::new(dest.to_str().unwrap()).unwrap();
            let err = tcfs_provider_fetch(prov, c_item.as_ptr(), c_dest.as_ptr());
            assert_ne!(
                err,
                TcfsError::TcfsErrorNone,
                "PerDevice/v3 manifest must FAIL CLOSED on the master-only backend"
            );
            assert!(
                !dest.exists(),
                "fail-closed fetch must not materialize a (corrupt) file"
            );
            tcfs_provider_free(prov);
        }
    }

    /// A plain master-only manifest reads unchanged on the direct backend —
    /// regression guard for the default `wrap_mode=master` path.
    #[test]
    fn master_only_manifest_reads_unchanged_on_direct_backend() {
        let operator = memory_operator();
        let master = tcfs_crypto::MasterKey::from_bytes([9u8; tcfs_crypto::KEY_SIZE]);
        let prefix = "mo";
        let content = b"plain master-only payload, unchanged";
        seed_encrypted_file(&operator, prefix, "mo.txt", content, &master, true, false);

        let prov = master_provider_on(operator, prefix, master);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.bin");
        unsafe {
            let c_item = CString::new("mo.txt").unwrap();
            let c_dest = CString::new(dest.to_str().unwrap()).unwrap();
            let err = tcfs_provider_fetch(prov, c_item.as_ptr(), c_dest.as_ptr());
            assert_eq!(err, TcfsError::TcfsErrorNone);
            assert_eq!(std::fs::read(&dest).unwrap(), content);
            tcfs_provider_free(prov);
        }
    }
}
