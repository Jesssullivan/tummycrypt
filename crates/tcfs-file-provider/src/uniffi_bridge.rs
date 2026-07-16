//! UniFFI bindings for iOS FileProvider extension.
//!
//! Exposes tcfs storage operations as safe Rust types that UniFFI
//! auto-generates Swift bindings for. Uses the `direct` backend
//! (S3 via OpenDAL) since iOS sandboxing prevents UDS gRPC.
//!
//! Enable with `--features uniffi`.

use std::sync::Arc;

use base64::Engine;
use secrecy::SecretString;

const FILE_PROVIDER_READ_ONLY_ERROR: &str =
    "TCFS FileProvider is read-only until exact version-token conditional publication is available";

/// Configuration for the TCFS provider.
///
/// On iOS, these values come from the Keychain via the Swift layer.
/// `s3_endpoint` must use HTTPS. This record intentionally exposes no
/// plaintext opt-in; lower-level development tests can use the process-only
/// `TCFS_STORAGE_ALLOW_INSECURE_HTTP` escape hatch.
#[derive(uniffi::Record)]
pub struct ProviderConfig {
    pub s3_endpoint: String,
    pub s3_bucket: String,
    pub access_key: String,
    pub s3_secret: String,
    pub remote_prefix: String,
    pub device_id: String,
    /// Passphrase for E2EE (empty string = plaintext mode).
    pub encryption_passphrase: String,
    /// Salt for Argon2id key derivation.
    pub encryption_salt: String,
}

/// A file item returned by directory enumeration.
#[derive(uniffi::Record)]
pub struct FileItem {
    pub item_id: String,
    pub filename: String,
    pub file_size: u64,
    pub modified_timestamp: i64,
    pub is_directory: bool,
    /// Opaque FileProvider version token (selected manifest object ID).
    pub content_hash: String,
    /// If non-empty, this file has a conflict with the named device.
    pub conflict_with: String,
}

/// Sync status summary.
#[derive(uniffi::Record)]
pub struct SyncStatus {
    pub connected: bool,
    pub files_synced: u64,
    pub files_pending: u64,
    pub last_error: Option<String>,
}

/// Progress callback for hydration/upload operations.
///
/// Implemented by Swift to update `Progress.completedUnitCount`.
#[uniffi::export(callback_interface)]
pub trait ProgressCallback: Send + Sync {
    /// Called when progress updates (completed out of total bytes).
    fn on_progress(&self, completed: u64, total: u64);
}

/// Result of a TOTP enrollment.
#[derive(uniffi::Record)]
pub struct TotpEnrollment {
    /// Base32-encoded shared secret.
    pub secret: String,
    /// otpauth:// URI for authenticator apps.
    pub qr_uri: String,
    /// Human-readable instructions.
    pub instructions: String,
}

/// Result of an authentication attempt.
#[derive(uniffi::Record)]
pub struct AuthResult {
    pub success: bool,
    pub session_token: String,
    pub error_message: String,
}

/// Result of a device enrollment via invite.
///
/// Contains all credentials needed to configure the new device.
#[derive(uniffi::Record)]
pub struct EnrollmentResult {
    pub success: bool,
    pub error_message: String,
    pub device_id: String,
    pub storage_endpoint: String,
    pub storage_bucket: String,
    pub access_key: String,
    pub s3_secret: String,
    pub remote_prefix: String,
    pub encryption_passphrase: String,
    pub encryption_salt: String,
    pub session_token: String,
}

/// Errors returned by provider operations.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum ProviderError {
    #[error("Storage error: {message}")]
    Storage { message: String },
    #[error("Decryption error: {message}")]
    Decryption { message: String },
    #[error("Network error: {message}")]
    Network { message: String },
    #[error("Not found: {path}")]
    NotFound { path: String },
    #[error("Invalid argument: {message}")]
    InvalidArgument { message: String },
    #[error("Conflict detected: {path}")]
    Conflict { path: String },
    #[error("Requested version {requested} is no longer current (current: {current})")]
    VersionMismatch { requested: String, current: String },
    #[error("Authentication error: {message}")]
    Auth { message: String },
}

impl From<anyhow::Error> for ProviderError {
    fn from(e: anyhow::Error) -> Self {
        ProviderError::Storage {
            message: e.to_string(),
        }
    }
}

impl From<opendal::Error> for ProviderError {
    fn from(e: opendal::Error) -> Self {
        ProviderError::Storage {
            message: e.to_string(),
        }
    }
}

fn parse_visible_index_entry(
    bytes: &[u8],
    path: &str,
) -> Result<Option<tcfs_sync::index_entry::RemoteIndexEntry>, ProviderError> {
    tcfs_sync::index_entry::parse_index_entry_record(bytes)
        .map(|record| record.visible_entry().cloned())
        .map_err(|e| ProviderError::Storage {
            message: format!("parsing index entry {path}: {e}"),
        })
}

fn logical_index_key(
    remote_prefix: &str,
    item_id: &str,
) -> Result<(String, String), ProviderError> {
    let rel_path = item_id.trim_matches('/');
    if rel_path.is_empty() {
        return Err(ProviderError::InvalidArgument {
            message: "FileProvider item id must not be empty".into(),
        });
    }
    tcfs_sync::index_entry::validate_canonical_rel_path(rel_path).map_err(|e| {
        ProviderError::InvalidArgument {
            message: format!("invalid logical FileProvider item id {item_id}: {e}"),
        }
    })?;
    Ok((
        rel_path.to_string(),
        format!("{}/index/{rel_path}", remote_prefix.trim_end_matches('/')),
    ))
}

fn ensure_requested_version(
    requested_version: &str,
    current_manifest_id: &str,
) -> Result<(), ProviderError> {
    if requested_version.is_empty() || requested_version == current_manifest_id {
        return Ok(());
    }
    Err(ProviderError::VersionMismatch {
        requested: requested_version.to_string(),
        current: current_manifest_id.to_string(),
    })
}

/// Return whether an index subtree contains any logically visible entry.
/// Physical v4 tombstones remain in object storage and must not keep a
/// FileProvider directory visible by key prefix alone.
async fn index_prefix_has_visible_entries(
    operator: &opendal::Operator,
    index_prefix: &str,
) -> Result<bool, ProviderError> {
    let entries = operator
        .list_with(index_prefix)
        .recursive(true)
        .await
        .map_err(|e| ProviderError::Storage {
            message: format!("listing logical index subtree {index_prefix}: {e}"),
        })?;

    for entry in entries {
        let key = entry.path();
        if key.ends_with('/') {
            continue;
        }
        if key.ends_with("/.tcfs_dir") {
            if tcfs_sync::index_entry::directory_marker_is_visible(operator, key)
                .await
                .map_err(|e| ProviderError::Storage {
                    message: format!("reading directory marker {key}: {e:#}"),
                })?
            {
                return Ok(true);
            }
            continue;
        }
        let Some(record) =
            tcfs_sync::index_entry::read_index_entry_record_from_store(operator, key)
                .await
                .map_err(|e| ProviderError::Storage {
                    message: format!("reading logical index entry {key}: {e:#}"),
                })?
        else {
            continue;
        };
        if record.visible_entry().is_some() || record.pending_entry().is_some() {
            return Ok(true);
        }
    }

    Ok(false)
}

async fn count_visible_index_files(
    operator: &opendal::Operator,
    index_prefix: &str,
) -> Result<u64, ProviderError> {
    let entries = operator
        .list_with(index_prefix)
        .recursive(true)
        .await
        .map_err(|e| ProviderError::Storage {
            message: format!("listing index files for status {index_prefix}: {e}"),
        })?;

    let mut count = 0u64;
    for entry in entries {
        let key = entry.path();
        if key.ends_with('/') || key.ends_with("/.tcfs_dir") {
            continue;
        }
        let Some(record) =
            tcfs_sync::index_entry::read_index_entry_record_from_store(operator, key)
                .await
                .map_err(|e| ProviderError::Storage {
                    message: format!("reading index file for status {key}: {e:#}"),
                })?
        else {
            continue;
        };
        if record.visible_entry().is_some() {
            count += 1;
        }
    }

    Ok(count)
}

fn validate_assembled_file(
    manifest: &tcfs_sync::manifest::SyncManifest,
    assembled: &[u8],
) -> Result<(), ProviderError> {
    if assembled.len() as u64 != manifest.file_size {
        return Err(ProviderError::Storage {
            message: format!(
                "assembled file size mismatch: expected {}, got {}",
                manifest.file_size,
                assembled.len()
            ),
        });
    }
    let actual = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(assembled));
    if actual != manifest.file_hash {
        return Err(ProviderError::Storage {
            message: format!(
                "assembled file integrity failure: expected {}, got {}",
                manifest.file_hash, actual
            ),
        });
    }
    Ok(())
}

fn derive_provider_master_key(
    encryption_passphrase: &str,
    encryption_salt: &str,
) -> Result<Option<tcfs_crypto::MasterKey>, ProviderError> {
    if encryption_passphrase.is_empty() {
        return Ok(None);
    }
    if encryption_passphrase.trim().is_empty() {
        return Err(ProviderError::Decryption {
            message: "configured encryption passphrase must not be whitespace-only".into(),
        });
    }

    if encryption_passphrase.split_whitespace().count() >= 12 {
        return tcfs_crypto::mnemonic_to_master_key(encryption_passphrase)
            .map(Some)
            .map_err(|e| ProviderError::Decryption {
                message: format!("invalid configured encryption mnemonic: {e}"),
            });
    }

    let mut salt = [0u8; 16];
    let salt_bytes = encryption_salt.as_bytes();
    let copy_len = salt_bytes.len().min(16);
    salt[..copy_len].copy_from_slice(&salt_bytes[..copy_len]);

    let params = tcfs_crypto::kdf::KdfParams {
        mem_cost_kib: 65536,
        time_cost: 3,
        parallelism: 4,
    };
    tcfs_crypto::derive_master_key(
        &SecretString::from(encryption_passphrase.to_string()),
        &salt,
        &params,
    )
    .map(Some)
    .map_err(|e| ProviderError::Decryption {
        message: format!("invalid configured encryption passphrase: {e}"),
    })
}

/// The TCFS provider — holds a tokio runtime and OpenDAL operator.
///
/// Created once and shared across FileProvider extension calls.
/// Thread-safe via `Arc` (UniFFI `Object` types are always `Arc`-wrapped).
#[derive(uniffi::Object)]
pub struct TcfsProviderHandle {
    runtime: tokio::runtime::Runtime,
    operator: opendal::Operator,
    remote_prefix: String,
    device_id: String,
    master_key: Option<tcfs_crypto::MasterKey>,
    #[cfg(feature = "uniffi")]
    totp_provider: Arc<tcfs_auth::totp::TotpProvider>,
    #[cfg(feature = "uniffi")]
    session_store: tcfs_auth::SessionStore,
}

#[uniffi::export]
impl TcfsProviderHandle {
    /// Create a new provider from configuration.
    #[uniffi::constructor]
    pub fn new(config: ProviderConfig) -> Result<Arc<Self>, ProviderError> {
        // Resolve encryption before constructing a storage operator. A
        // malformed non-empty passphrase must never yield a live plaintext
        // provider that can mutate chunks or the index.
        let master_key =
            derive_provider_master_key(&config.encryption_passphrase, &config.encryption_salt)?;
        let runtime = tokio::runtime::Runtime::new().map_err(|e| ProviderError::Storage {
            message: format!("failed to create tokio runtime: {e}"),
        })?;

        let operator = crate::storage_bounds::build_operator_from_parts_with_env(
            config.s3_endpoint,
            config.s3_bucket,
            config.access_key,
            config.s3_secret,
        )
        .map_err(|e| ProviderError::Storage {
            message: e.to_string(),
        })?;

        Ok(Arc::new(Self {
            runtime,
            operator,
            remote_prefix: config.remote_prefix,
            device_id: config.device_id,
            master_key,
            #[cfg(feature = "uniffi")]
            totp_provider: Arc::new(tcfs_auth::totp::TotpProvider::new(
                tcfs_auth::totp::TotpConfig::default(),
            )),
            #[cfg(feature = "uniffi")]
            session_store: tcfs_auth::SessionStore::new(),
        }))
    }

    /// List files at a given relative path.
    pub fn list_items(&self, path: &str) -> Result<Vec<FileItem>, ProviderError> {
        self.runtime.block_on(async {
            let rel_dir = path.trim_matches('/');
            if !rel_dir.is_empty() {
                tcfs_sync::index_entry::validate_canonical_rel_path(rel_dir).map_err(|e| {
                    ProviderError::InvalidArgument {
                        message: format!("invalid logical FileProvider directory {path}: {e}"),
                    }
                })?;
            }
            let prefix = if rel_dir.is_empty() {
                format!("{}/index/", self.remote_prefix.trim_end_matches('/'))
            } else {
                format!(
                    "{}/index/{rel_dir}/",
                    self.remote_prefix.trim_end_matches('/')
                )
            };

            let entries = self
                .operator
                .list(&prefix)
                .await
                .map_err(ProviderError::from)?;

            let mut items = Vec::new();
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

                if display_name == ".tcfs_dir" {
                    continue;
                }

                let visible_entry = if is_dir {
                    if !index_prefix_has_visible_entries(&self.operator, entry_path).await? {
                        continue;
                    }
                    None
                } else {
                    let bytes = self.operator.read(entry_path).await?.to_bytes();
                    let Some(entry) = parse_visible_index_entry(&bytes, entry_path)? else {
                        continue;
                    };
                    Some(entry)
                };

                let modified_ts = entry
                    .metadata()
                    .last_modified()
                    .map(|t| t.into_inner().as_second())
                    .unwrap_or(0);

                // Check for conflict via vclock divergence
                let conflict_with = if !is_dir {
                    self.check_conflict_async(entry_path)
                        .await
                        .unwrap_or(None)
                        .unwrap_or_default()
                } else {
                    String::new()
                };

                let logical_item_id = if rel_dir.is_empty() {
                    display_name.to_string()
                } else {
                    format!("{rel_dir}/{display_name}")
                };
                let version_token = visible_entry
                    .as_ref()
                    .map(|entry| entry.manifest_hash.clone())
                    .unwrap_or_default();

                items.push(FileItem {
                    item_id: logical_item_id,
                    filename: display_name.to_string(),
                    file_size: visible_entry.as_ref().map_or(0, |entry| entry.size),
                    modified_timestamp: modified_ts,
                    is_directory: is_dir,
                    content_hash: version_token,
                    conflict_with,
                });
            }

            Ok(items)
        })
    }

    /// Hydrate (download + decrypt + reassemble) a file to a local path.
    pub fn hydrate_file(&self, item_id: &str, destination_path: &str) -> Result<(), ProviderError> {
        self.runtime.block_on(async {
            let (rel_path, index_key) = logical_index_key(&self.remote_prefix, item_id)?;
            let manifest_prefix = format!("{}/manifests", self.remote_prefix.trim_end_matches('/'));
            let index_entry = tcfs_sync::index_entry::resolve_visible_index_entry(
                &self.operator,
                &index_key,
                &manifest_prefix,
            )
            .await
            .map_err(ProviderError::from)?
            .ok_or_else(|| ProviderError::NotFound {
                path: item_id.to_string(),
            })?;

            let manifest_path = format!(
                "{}/manifests/{}",
                self.remote_prefix.trim_end_matches('/'),
                index_entry.manifest_hash
            );

            let manifest_bytes = self.operator.read(&manifest_path).await?;
            let manifest_bytes = manifest_bytes.to_bytes();
            tcfs_sync::engine::validate_indexed_manifest_entry_binding(
                &manifest_bytes,
                &index_entry.manifest_hash,
                &index_entry,
                &rel_path,
            )
            .map_err(|e| ProviderError::Storage {
                message: format!("validating manifest binding for {item_id}: {e}"),
            })?;
            let manifest =
                tcfs_sync::manifest::SyncManifest::from_bytes(&manifest_bytes).map_err(|e| {
                    ProviderError::Storage {
                        message: e.to_string(),
                    }
                })?;

            // Fail CLOSED on PerDevice/v3 manifests only: this backend unwraps
            // master-wrapped keys only (no per-device age identity). A manifest
            // with `wrapped_file_keys` AND no master `encrypted_file_key` would
            // otherwise fall through to "no file key" and copy raw ciphertext.
            // Dual/v2 manifests (both wraps present) are readable via the master
            // wrap below, mirroring the engine read switch (TIN-1898).
            if !manifest.wrapped_file_keys.is_empty() && manifest.encrypted_file_key.is_none() {
                return Err(ProviderError::Decryption {
                    message: "manifest is per-device encrypted (wrapped_file_keys present, \
                              no master wrap); this backend only supports master-key unwrapping"
                        .to_string(),
                });
            }

            let file_key = match (&self.master_key, &manifest.encrypted_file_key) {
                (Some(mk), Some(wrapped_b64)) => {
                    let wrapped = base64::engine::general_purpose::STANDARD
                        .decode(wrapped_b64)
                        .map_err(|e| ProviderError::Decryption {
                            message: e.to_string(),
                        })?;
                    Some(tcfs_crypto::unwrap_key(mk, &wrapped).map_err(|e| {
                        ProviderError::Decryption {
                            message: e.to_string(),
                        }
                    })?)
                }
                (None, Some(_)) => {
                    return Err(ProviderError::Decryption {
                        message: "file is encrypted but no master key configured".into(),
                    });
                }
                _ => None,
            };

            let file_id_bytes: [u8; 32] = tcfs_chunks::hash_from_hex(&manifest.file_hash)
                .map(|h| *h.as_bytes())
                .unwrap_or([0u8; 32]);

            let mut assembled = Vec::new();
            for (idx, hash) in manifest.chunk_hashes().iter().enumerate() {
                let chunk_key = format!(
                    "{}/chunks/{}",
                    self.remote_prefix.trim_end_matches('/'),
                    hash
                );
                let chunk_data = self.operator.read(&chunk_key).await?;
                let chunk_bytes = chunk_data.to_bytes();

                let actual = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(&chunk_bytes));
                if actual != *hash {
                    return Err(ProviderError::Storage {
                        message: format!(
                            "chunk integrity failure: expected {}, got {}",
                            hash, actual
                        ),
                    });
                }

                if let Some(ref fk) = file_key {
                    let plaintext =
                        tcfs_crypto::decrypt_chunk(fk, idx as u64, &file_id_bytes, &chunk_bytes)
                            .map_err(|e| ProviderError::Decryption {
                                message: e.to_string(),
                            })?;
                    assembled.extend_from_slice(&plaintext);
                } else {
                    assembled.extend_from_slice(&chunk_bytes);
                }
            }

            validate_assembled_file(&manifest, &assembled)?;
            tokio::fs::write(destination_path, &assembled)
                .await
                .map_err(|e| ProviderError::Storage {
                    message: format!("write to {}: {}", destination_path, e),
                })?;

            Ok(())
        })
    }

    /// Hydrate a file with progress reporting.
    ///
    /// Same as `hydrate_file` but calls the progress callback after each chunk.
    pub fn hydrate_file_with_progress(
        &self,
        item_id: &str,
        destination_path: &str,
        callback: Box<dyn ProgressCallback>,
    ) -> Result<(), ProviderError> {
        self.hydrate_file_version_with_progress(item_id, destination_path, "", callback)
    }

    /// Hydrate only the immutable manifest version exposed during enumeration.
    /// A stale non-empty token fails before manifest, chunk, or destination I/O.
    pub fn hydrate_file_version_with_progress(
        &self,
        item_id: &str,
        destination_path: &str,
        requested_version: &str,
        callback: Box<dyn ProgressCallback>,
    ) -> Result<(), ProviderError> {
        self.runtime.block_on(async {
            let (rel_path, index_key) = logical_index_key(&self.remote_prefix, item_id)?;
            if !requested_version.is_empty() {
                let selected = tcfs_sync::engine::read_exact_visible_index_selection(
                    &self.operator,
                    &rel_path,
                    self.remote_prefix.trim_end_matches('/'),
                )
                .await
                .map_err(ProviderError::from)?
                .ok_or_else(|| ProviderError::NotFound {
                    path: item_id.to_string(),
                })?;
                ensure_requested_version(requested_version, &selected.manifest_hash)?;
            }
            let manifest_prefix = format!("{}/manifests", self.remote_prefix.trim_end_matches('/'));
            let index_entry = tcfs_sync::index_entry::resolve_visible_index_entry(
                &self.operator,
                &index_key,
                &manifest_prefix,
            )
            .await
            .map_err(ProviderError::from)?
            .ok_or_else(|| ProviderError::NotFound {
                path: item_id.to_string(),
            })?;

            ensure_requested_version(requested_version, &index_entry.manifest_hash)?;

            // Fetch manifest
            let manifest_path = format!(
                "{}/manifests/{}",
                self.remote_prefix.trim_end_matches('/'),
                index_entry.manifest_hash
            );
            let manifest_bytes = self.operator.read(&manifest_path).await?;
            let manifest_bytes = manifest_bytes.to_bytes();
            tcfs_sync::engine::validate_indexed_manifest_entry_binding(
                &manifest_bytes,
                &index_entry.manifest_hash,
                &index_entry,
                &rel_path,
            )
            .map_err(|e| ProviderError::Storage {
                message: format!("validating manifest binding for {item_id}: {e}"),
            })?;
            let manifest =
                tcfs_sync::manifest::SyncManifest::from_bytes(&manifest_bytes).map_err(|e| {
                    ProviderError::Storage {
                        message: e.to_string(),
                    }
                })?;

            // Fail CLOSED on PerDevice/v3 manifests only (see hydrate path above):
            // Dual/v2 (both wraps) is readable via the master wrap; only a
            // wrapped_file_keys manifest with NO master wrap is refused.
            if !manifest.wrapped_file_keys.is_empty() && manifest.encrypted_file_key.is_none() {
                return Err(ProviderError::Decryption {
                    message: "manifest is per-device encrypted (wrapped_file_keys present, \
                              no master wrap); this backend only supports master-key unwrapping"
                        .to_string(),
                });
            }

            // Unwrap file key if encrypted
            let file_key = match (&self.master_key, &manifest.encrypted_file_key) {
                (Some(mk), Some(wrapped_b64)) => {
                    let wrapped = base64::engine::general_purpose::STANDARD
                        .decode(wrapped_b64)
                        .map_err(|e| ProviderError::Decryption {
                            message: e.to_string(),
                        })?;
                    Some(tcfs_crypto::unwrap_key(mk, &wrapped).map_err(|e| {
                        ProviderError::Decryption {
                            message: e.to_string(),
                        }
                    })?)
                }
                (None, Some(_)) => {
                    return Err(ProviderError::Decryption {
                        message: "file is encrypted but no master key configured".into(),
                    });
                }
                _ => None,
            };

            let file_id_bytes: [u8; 32] = tcfs_chunks::hash_from_hex(&manifest.file_hash)
                .map(|h| *h.as_bytes())
                .unwrap_or([0u8; 32]);

            let chunk_hashes = manifest.chunk_hashes();
            let total_chunks = chunk_hashes.len() as u64;
            let mut assembled = Vec::new();

            callback.on_progress(0, total_chunks);

            for (idx, hash) in chunk_hashes.iter().enumerate() {
                let chunk_key = format!(
                    "{}/chunks/{}",
                    self.remote_prefix.trim_end_matches('/'),
                    hash
                );
                let chunk_data = self.operator.read(&chunk_key).await?;
                let chunk_bytes = chunk_data.to_bytes();

                let actual = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(&chunk_bytes));
                if actual != *hash {
                    return Err(ProviderError::Storage {
                        message: format!(
                            "chunk integrity failure: expected {}, got {}",
                            hash, actual
                        ),
                    });
                }

                if let Some(ref fk) = file_key {
                    let plaintext =
                        tcfs_crypto::decrypt_chunk(fk, idx as u64, &file_id_bytes, &chunk_bytes)
                            .map_err(|e| ProviderError::Decryption {
                                message: e.to_string(),
                            })?;
                    assembled.extend_from_slice(&plaintext);
                } else {
                    assembled.extend_from_slice(&chunk_bytes);
                }

                callback.on_progress((idx + 1) as u64, total_chunks);
            }

            validate_assembled_file(&manifest, &assembled)?;
            tokio::fs::write(destination_path, &assembled)
                .await
                .map_err(|e| ProviderError::Storage {
                    message: format!("write to {}: {}", destination_path, e),
                })?;

            Ok(())
        })
    }

    /// Upload a local file to remote storage.
    pub fn upload_file(&self, local_path: &str, remote_path: &str) -> Result<(), ProviderError> {
        let _ = (local_path, remote_path);
        Err(ProviderError::Storage {
            message: FILE_PROVIDER_READ_ONLY_ERROR.into(),
        })
    }

    /// Delete a file or directory by its item ID.
    pub fn delete_item(&self, item_id: &str) -> Result<(), ProviderError> {
        let _ = item_id;
        Err(ProviderError::Storage {
            message: FILE_PROVIDER_READ_ONLY_ERROR.into(),
        })
    }

    /// Create a directory under the given parent path.
    pub fn create_directory(&self, parent_path: &str, dir_name: &str) -> Result<(), ProviderError> {
        let _ = (parent_path, dir_name);
        Err(ProviderError::Storage {
            message: FILE_PROVIDER_READ_ONLY_ERROR.into(),
        })
    }

    /// Check if a file has a conflict (remote vclock diverged from ours).
    ///
    /// Returns the conflicting device ID if diverged, or None if clean.
    pub fn check_conflict(&self, item_id: &str) -> Result<Option<String>, ProviderError> {
        let (_, index_key) = logical_index_key(&self.remote_prefix, item_id)?;
        self.runtime.block_on(self.check_conflict_async(&index_key))
    }

    /// Enroll a TOTP authenticator for this device.
    ///
    /// Returns the shared secret and otpauth URI for the user to add
    /// to their authenticator app.
    #[cfg(feature = "uniffi")]
    pub fn auth_enroll_totp(&self) -> Result<TotpEnrollment, ProviderError> {
        use tcfs_auth::AuthProvider;

        self.runtime.block_on(async {
            let reg = self
                .totp_provider
                .register(&self.device_id)
                .await
                .map_err(|e| ProviderError::Auth {
                    message: e.to_string(),
                })?;

            // Parse the registration data JSON
            let json: serde_json::Value =
                serde_json::from_slice(&reg.data).map_err(|e| ProviderError::Auth {
                    message: format!("invalid registration data: {e}"),
                })?;

            Ok(TotpEnrollment {
                secret: json
                    .get("secret")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                qr_uri: json
                    .get("qr_uri")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                instructions: reg.instructions,
            })
        })
    }

    /// Verify a TOTP code and create a session.
    #[cfg(feature = "uniffi")]
    pub fn auth_verify_totp(&self, code: &str) -> Result<AuthResult, ProviderError> {
        use tcfs_auth::AuthProvider;

        self.runtime.block_on(async {
            let challenge = self
                .totp_provider
                .challenge(&self.device_id)
                .await
                .map_err(|e| ProviderError::Auth {
                    message: e.to_string(),
                })?;

            let response = tcfs_auth::AuthResponse {
                challenge_id: challenge.challenge_id,
                data: code.as_bytes().to_vec(),
                device_id: self.device_id.clone(),
            };

            match self.totp_provider.verify(&response).await {
                Ok(tcfs_auth::VerifyResult::Success {
                    session_token: _,
                    device_id,
                }) => {
                    let session =
                        tcfs_auth::Session::new(&device_id, &device_id, "totp").with_expiry(24);
                    let token = session.token.clone();
                    self.session_store.insert(session).await;

                    Ok(AuthResult {
                        success: true,
                        session_token: token,
                        error_message: String::new(),
                    })
                }
                Ok(tcfs_auth::VerifyResult::Failure { reason }) => Ok(AuthResult {
                    success: false,
                    session_token: String::new(),
                    error_message: reason,
                }),
                Ok(tcfs_auth::VerifyResult::Expired) => Ok(AuthResult {
                    success: false,
                    session_token: String::new(),
                    error_message: "challenge expired".into(),
                }),
                Err(e) => Err(ProviderError::Auth {
                    message: e.to_string(),
                }),
            }
        })
    }

    /// Check if there's an active authenticated session.
    #[cfg(feature = "uniffi")]
    pub fn auth_is_authenticated(&self) -> bool {
        self.runtime
            .block_on(self.session_store.has_active_session())
    }

    /// Process a device enrollment invite (from QR code or deep link).
    ///
    /// Direct iOS enrollment cannot verify admin-signed invites today because
    /// the new device does not yet have the fleet signing key. Until the
    /// pairing flow supplies a verifiable trust path, fail closed instead of
    /// extracting brokered credentials from an attacker-controlled payload.
    #[cfg(feature = "uniffi")]
    pub fn process_enrollment_invite(
        &self,
        invite_data: &str,
    ) -> Result<EnrollmentResult, ProviderError> {
        let invite =
            tcfs_auth::enrollment::EnrollmentInvite::decode_any(invite_data).map_err(|e| {
                ProviderError::Auth {
                    message: format!("invalid invite: {e}"),
                }
            })?;

        if invite.is_expired() {
            return Ok(EnrollmentResult {
                success: false,
                error_message: "invite has expired".into(),
                device_id: String::new(),
                storage_endpoint: String::new(),
                storage_bucket: String::new(),
                access_key: String::new(),
                s3_secret: String::new(),
                remote_prefix: String::new(),
                encryption_passphrase: String::new(),
                encryption_salt: String::new(),
                session_token: String::new(),
            });
        }

        Ok(EnrollmentResult {
            success: false,
            error_message: "enrollment invite signature cannot be verified on this device yet; use daemon-mediated enrollment or a trusted operator bootstrap config".into(),
            device_id: String::new(),
            storage_endpoint: String::new(),
            storage_bucket: String::new(),
            access_key: String::new(),
            s3_secret: String::new(),
            remote_prefix: String::new(),
            encryption_passphrase: String::new(),
            encryption_salt: String::new(),
            session_token: String::new(),
        })
    }

    /// Get sync status (connected check + file count from index).
    pub fn get_sync_status(&self) -> Result<SyncStatus, ProviderError> {
        let index_prefix = format!("{}/index/", self.remote_prefix.trim_end_matches('/'));

        self.runtime.block_on(async {
            match count_visible_index_files(&self.operator, &index_prefix).await {
                Ok(file_count) => Ok(SyncStatus {
                    connected: true,
                    files_synced: file_count,
                    files_pending: 0,
                    last_error: None,
                }),
                Err(e) => Ok(SyncStatus {
                    connected: false,
                    files_synced: 0,
                    files_pending: 0,
                    last_error: Some(e.to_string()),
                }),
            }
        })
    }
}

/// Verify a BLAKE3-keyed-MAC signature on a bootstrap config payload.
///
/// The signing key is the raw 32-byte key hex-encoded (64 chars).
/// The payload is the JSON string that was signed (everything except the `signature` field).
/// Returns true if the signature is valid, false otherwise.
#[cfg(feature = "uniffi")]
#[uniffi::export]
fn verify_bootstrap_signature(
    payload: String,
    signature: String,
    signing_key_hex: String,
) -> Result<bool, ProviderError> {
    // Decode hex key to 32 bytes
    let key_bytes: [u8; 32] = {
        let decoded: Vec<u8> = (0..signing_key_hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&signing_key_hex[i..i + 2], 16))
            .collect::<Result<Vec<u8>, _>>()
            .map_err(|e| ProviderError::Auth {
                message: format!("invalid signing key hex: {e}"),
            })?;
        decoded.try_into().map_err(|_| ProviderError::Auth {
            message: "signing key must be 32 bytes (64 hex chars)".into(),
        })?
    };

    let mac = blake3::keyed_hash(&key_bytes, payload.as_bytes());
    let expected = mac.to_hex();

    // Constant-time comparison
    let sig_bytes = signature.as_bytes();
    let exp_bytes = expected.as_bytes();
    if sig_bytes.len() != exp_bytes.len() {
        return Ok(false);
    }
    let mut diff = 0u8;
    for (a, b) in sig_bytes.iter().zip(exp_bytes.iter()) {
        diff |= a ^ b;
    }
    Ok(diff == 0)
}

/// Test-only constructor: build a handle directly over a caller-supplied
/// operator + optional master key, bypassing the S3/network constructor so
/// unit tests can seed an in-memory store and exercise the read path.
#[cfg(test)]
impl TcfsProviderHandle {
    fn new_for_test(
        operator: opendal::Operator,
        remote_prefix: &str,
        master_key: Option<tcfs_crypto::MasterKey>,
    ) -> Arc<Self> {
        let runtime = tokio::runtime::Runtime::new().expect("test runtime");
        Arc::new(Self {
            runtime,
            operator,
            remote_prefix: remote_prefix.to_string(),
            device_id: "ios-test".to_string(),
            master_key,
            #[cfg(feature = "uniffi")]
            totp_provider: Arc::new(tcfs_auth::totp::TotpProvider::new(
                tcfs_auth::totp::TotpConfig::default(),
            )),
            #[cfg(feature = "uniffi")]
            session_store: tcfs_auth::SessionStore::new(),
        })
    }
}

#[cfg(all(test, feature = "uniffi"))]
mod tests {
    use super::*;
    use base64::Engine;

    /// Seed a single-chunk encrypted file under `prefix` for `rel`. See the
    /// direct-backend `seed_encrypted_file` for the layout rationale (TIN-1898).
    /// `include_master`/`include_wraps` select master-only / dual / per-device.
    fn seed_encrypted_file(
        operator: &opendal::Operator,
        prefix: &str,
        rel: &str,
        content: &[u8],
        master_key: &tcfs_crypto::MasterKey,
        include_master: bool,
        include_wraps: bool,
    ) {
        let rt = tokio::runtime::Runtime::new().expect("seed runtime");
        rt.block_on(async {
            let file_hash = tcfs_chunks::hash_bytes(content);
            let file_hash_hex = tcfs_chunks::hash_to_hex(&file_hash);
            let file_id: [u8; 32] = *file_hash.as_bytes();

            let file_key = tcfs_crypto::generate_file_key();
            let encrypted =
                tcfs_crypto::encrypt_chunk(&file_key, 0, &file_id, content).expect("encrypt chunk");
            let chunk_hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(&encrypted));

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

    fn memory_operator() -> opendal::Operator {
        let op = opendal::Operator::new(opendal::services::Memory::default())
            .expect("memory operator")
            .finish();
        tcfs_sync::index_entry::register_memory_index_emulation_for_tests(&op).unwrap();
        op
    }

    struct NoopProgress;

    impl ProgressCallback for NoopProgress {
        fn on_progress(&self, _completed: u64, _total: u64) {}
    }

    #[test]
    fn list_items_skips_deleted_index_records_as_logical_absence() {
        let operator = memory_operator();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime
            .block_on(async {
                operator
                    .write(
                        "data/index/gone.txt",
                        tcfs_sync::index_entry::VersionedIndexEntry::deleted()
                            .to_json_bytes()
                            .unwrap(),
                    )
                    .await?;
                operator
                    .write(
                        "data/index/visible.txt",
                        tcfs_sync::index_entry::RemoteIndexEntry::new("visible-object", 7, 1)
                            .to_legacy_bytes(),
                    )
                    .await?;
                Ok::<(), opendal::Error>(())
            })
            .unwrap();

        let handle = TcfsProviderHandle::new_for_test(operator, "data", None);
        let items = handle.list_items("").unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].item_id, "visible.txt");
        assert_eq!(items[0].filename, "visible.txt");
        assert_eq!(items[0].file_size, 7);
        assert_eq!(items[0].content_hash, "visible-object");
    }

    #[test]
    fn list_and_status_hide_physical_tombstone_subtrees() {
        let operator = memory_operator();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime
            .block_on(async {
                let tombstone = tcfs_sync::index_entry::VersionedIndexEntry::deleted()
                    .to_json_bytes()
                    .unwrap();
                operator
                    .write("data/index/ghost/gone.txt", tombstone.clone())
                    .await?;
                operator
                    .write("data/index/ghost-empty/.tcfs_dir", tombstone)
                    .await?;
                operator
                    .write(
                        "data/index/live/present.txt",
                        tcfs_sync::index_entry::RemoteIndexEntry::new("visible-object", 7, 1)
                            .to_legacy_bytes(),
                    )
                    .await?;
                operator
                    .write(
                        "data/index/live-empty/.tcfs_dir",
                        tcfs_sync::index_entry::DIRECTORY_MARKER_BYTES.to_vec(),
                    )
                    .await?;
                Ok::<(), opendal::Error>(())
            })
            .unwrap();

        let handle = TcfsProviderHandle::new_for_test(operator, "data", None);
        let mut names = handle
            .list_items("")
            .unwrap()
            .into_iter()
            .map(|item| item.filename)
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(names, vec!["live", "live-empty"]);
        assert!(handle.list_items("live-empty/").unwrap().is_empty());
        let nested = handle.list_items("live").unwrap();
        assert_eq!(nested.len(), 1);
        assert_eq!(nested[0].item_id, "live/present.txt");
        assert_eq!(nested[0].content_hash, "visible-object");

        let status = handle.get_sync_status().unwrap();
        assert!(status.connected);
        assert_eq!(status.files_synced, 1);
    }

    #[test]
    fn versioned_hydrate_rejects_stale_manifest_before_destination_io() {
        let operator = memory_operator();
        let master = tcfs_crypto::MasterKey::from_bytes([9u8; tcfs_crypto::KEY_SIZE]);
        let content = b"version-bound payload";
        seed_encrypted_file(
            &operator,
            "versioned",
            "nested/file.txt",
            content,
            &master,
            true,
            false,
        );
        let current = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(content));
        let handle = TcfsProviderHandle::new_for_test(operator, "versioned", Some(master));
        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join("destination.txt");
        std::fs::write(&destination, b"keep-local-bytes").unwrap();

        let error = handle
            .hydrate_file_version_with_progress(
                "nested/file.txt",
                destination.to_str().unwrap(),
                "stale-manifest-id",
                Box::new(NoopProgress),
            )
            .unwrap_err();
        assert!(matches!(
            error,
            ProviderError::VersionMismatch { requested, current: observed }
                if requested == "stale-manifest-id" && observed == current
        ));
        assert_eq!(std::fs::read(&destination).unwrap(), b"keep-local-bytes");

        handle
            .hydrate_file_version_with_progress(
                "nested/file.txt",
                destination.to_str().unwrap(),
                &current,
                Box::new(NoopProgress),
            )
            .unwrap();
        assert_eq!(std::fs::read(destination).unwrap(), content);
    }

    #[test]
    fn versioned_hydrate_rejects_stale_before_missing_manifest_io() {
        let operator = memory_operator();
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(
                operator.write(
                    "versioned-missing/index/file.txt",
                    tcfs_sync::index_entry::RemoteIndexEntry::new("missing-current-manifest", 7, 1)
                        .to_legacy_bytes(),
                ),
            )
            .unwrap();
        let handle = TcfsProviderHandle::new_for_test(operator, "versioned-missing", None);
        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join("destination.txt");
        std::fs::write(&destination, b"keep-local-bytes").unwrap();

        let error = handle
            .hydrate_file_version_with_progress(
                "file.txt",
                destination.to_str().unwrap(),
                "stale-manifest",
                Box::new(NoopProgress),
            )
            .unwrap_err();
        assert!(matches!(
            error,
            ProviderError::VersionMismatch { requested, current }
                if requested == "stale-manifest" && current == "missing-current-manifest"
        ));
        assert_eq!(std::fs::read(destination).unwrap(), b"keep-local-bytes");
    }

    #[test]
    fn hydrate_rejects_index_size_and_chunk_count_mismatches_before_destination_write() {
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
            let runtime = tokio::runtime::Runtime::new().unwrap();
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

            let handle = TcfsProviderHandle::new_for_test(operator, prefix, Some(master));
            let temp = tempfile::tempdir().unwrap();
            let destination = temp.path().join("destination.txt");
            std::fs::write(&destination, b"keep-local-bytes").unwrap();
            let item_id = rel_path.to_string();
            let result = if use_progress {
                handle.hydrate_file_with_progress(
                    &item_id,
                    destination.to_str().unwrap(),
                    Box::new(NoopProgress),
                )
            } else {
                handle.hydrate_file(&item_id, destination.to_str().unwrap())
            };

            assert!(
                matches!(&result, Err(ProviderError::Storage { message }) if message.contains(expected_error)),
                "expected {expected_error}, got {result:?}"
            );
            assert_eq!(std::fs::read(&destination).unwrap(), b"keep-local-bytes");
        }
    }

    #[test]
    fn malformed_configured_passphrase_fails_before_remote_mutation() {
        let operator = memory_operator();
        let malformed = "notaword notaword notaword notaword notaword notaword \
                         notaword notaword notaword notaword notaword notaword";

        let result = derive_provider_master_key(malformed, "test-salt");

        assert!(matches!(result, Err(ProviderError::Decryption { .. })));
        let runtime = tokio::runtime::Runtime::new().expect("inspection runtime");
        assert!(runtime.block_on(operator.list("")).unwrap().is_empty());
    }

    #[test]
    fn file_provider_mutations_are_read_only_before_remote_mutation() {
        let operator = memory_operator();
        let handle = TcfsProviderHandle::new_for_test(operator.clone(), "guard", None);
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.txt");
        std::fs::write(&source, b"must never be uploaded").unwrap();

        let result = handle.upload_file(source.to_str().unwrap(), "guarded.txt");

        assert!(matches!(
            result,
            Err(ProviderError::Storage { message }) if message.contains("read-only")
        ));
        assert!(matches!(
            handle.delete_item("guarded.txt"),
            Err(ProviderError::Storage { message }) if message.contains("read-only")
        ));
        assert!(matches!(
            handle.create_directory("", "new-dir"),
            Err(ProviderError::Storage { message }) if message.contains("read-only")
        ));
        let runtime = tokio::runtime::Runtime::new().expect("inspection runtime");
        assert!(runtime.block_on(operator.list("")).unwrap().is_empty());
    }

    /// TIN-1898: a Dual/v2 manifest (master + per-device wraps) is READABLE via
    /// the master-only uniffi backend through the master wrap.
    #[test]
    fn dual_manifest_reads_via_master_wrap_on_uniffi_backend() {
        let operator = memory_operator();
        let master = tcfs_crypto::MasterKey::from_bytes([9u8; tcfs_crypto::KEY_SIZE]);
        let prefix = "dual";
        let content = b"dual manifest readable via master fallback on the uniffi backend";
        seed_encrypted_file(&operator, prefix, "dual.txt", content, &master, true, true);

        let handle = TcfsProviderHandle::new_for_test(operator, prefix, Some(master));
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.bin");
        handle
            .hydrate_file("dual.txt", dest.to_str().unwrap())
            .expect("Dual manifest must hydrate via the master wrap");
        assert_eq!(std::fs::read(&dest).unwrap(), content);
    }

    /// TIN-1898: a PerDevice/v3 manifest (wrapped_file_keys, NO master wrap)
    /// still FAILS CLOSED on the master-only uniffi backend and writes no file.
    #[test]
    fn per_device_manifest_fails_closed_on_uniffi_backend() {
        let operator = memory_operator();
        let master = tcfs_crypto::MasterKey::from_bytes([9u8; tcfs_crypto::KEY_SIZE]);
        let prefix = "pd";
        let content = b"per-device-only payload the uniffi master backend cannot read";
        seed_encrypted_file(&operator, prefix, "pd.txt", content, &master, false, true);

        let handle = TcfsProviderHandle::new_for_test(operator, prefix, Some(master));
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.bin");
        let res = handle.hydrate_file("pd.txt", dest.to_str().unwrap());
        assert!(
            matches!(res, Err(ProviderError::Decryption { .. })),
            "PerDevice/v3 must FAIL CLOSED with a Decryption error, got {res:?}"
        );
        assert!(
            !dest.exists(),
            "fail-closed hydrate must not materialize a (corrupt) file"
        );
    }

    /// A plain master-only manifest reads unchanged on the uniffi backend.
    #[test]
    fn master_only_manifest_reads_unchanged_on_uniffi_backend() {
        let operator = memory_operator();
        let master = tcfs_crypto::MasterKey::from_bytes([9u8; tcfs_crypto::KEY_SIZE]);
        let prefix = "mo";
        let content = b"plain master-only payload, unchanged (uniffi)";
        seed_encrypted_file(&operator, prefix, "mo.txt", content, &master, true, false);

        let handle = TcfsProviderHandle::new_for_test(operator, prefix, Some(master));
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.bin");
        handle
            .hydrate_file("mo.txt", dest.to_str().unwrap())
            .expect("master-only manifest must hydrate unchanged");
        assert_eq!(std::fs::read(&dest).unwrap(), content);
    }

    fn test_provider() -> Arc<TcfsProviderHandle> {
        TcfsProviderHandle::new(ProviderConfig {
            s3_endpoint: "https://127.0.0.1:8333".into(),
            s3_bucket: "tcfs-test".into(),
            access_key: "test-access".into(),
            s3_secret: "test-secret".into(),
            remote_prefix: "default".into(),
            device_id: "ios-test".into(),
            encryption_passphrase: String::new(),
            encryption_salt: String::new(),
        })
        .expect("provider config should be valid")
    }

    #[test]
    fn process_enrollment_invite_rejects_unverifiable_brokered_credentials() {
        let signing_key = [42u8; 32];
        let mut invite = tcfs_auth::EnrollmentInvite::new(
            "admin-device",
            &signing_key,
            24,
            tcfs_auth::session::DevicePermissions::default(),
        );
        invite.storage_endpoint = Some("https://s3.example.invalid".into());
        invite.storage_bucket = Some("tcfs".into());
        invite.storage_access_key = Some("access-key".into());
        invite.storage_secret_key = Some("secret-key".into());
        invite.remote_prefix = Some("tenant-a".into());
        invite.encryption_passphrase = Some("phrase".into());
        invite.encryption_salt = Some("salt".into());
        invite.refresh_signature(&signing_key);

        let provider = test_provider();
        let result = provider
            .process_enrollment_invite(&invite.encode_compact().expect("invite encodes"))
            .expect("well-formed invite should return a denial result");

        assert!(!result.success);
        assert!(result.error_message.contains("cannot be verified"));
        assert_eq!(result.storage_endpoint, "");
        assert_eq!(result.storage_bucket, "");
        assert_eq!(result.access_key, "");
        assert_eq!(result.s3_secret, "");
        assert_eq!(result.encryption_passphrase, "");
    }
}

impl TcfsProviderHandle {
    /// Internal async conflict check — used by both `check_conflict` and `list_items`.
    async fn check_conflict_async(&self, item_id: &str) -> Result<Option<String>, ProviderError> {
        let data = match self.operator.read(item_id).await {
            Ok(d) => d,
            Err(_) => return Ok(None),
        };
        let bytes = data.to_bytes();
        let manifest_hash = match parse_visible_index_entry(&bytes, item_id) {
            Ok(Some(entry)) => entry.manifest_hash,
            Ok(None) => return Ok(None),
            Err(_) => return Ok(None),
        };

        let manifest_path = format!(
            "{}/manifests/{}",
            self.remote_prefix.trim_end_matches('/'),
            manifest_hash
        );

        let manifest_bytes = match self.operator.read(&manifest_path).await {
            Ok(d) => d,
            Err(_) => return Ok(None),
        };

        let manifest =
            match tcfs_sync::manifest::SyncManifest::from_bytes(&manifest_bytes.to_bytes()) {
                Ok(m) => m,
                Err(_) => return Ok(None),
            };

        // Conflict: written by another device and our device hasn't merged yet
        if manifest.written_by != self.device_id
            && !manifest.vclock.clocks.contains_key(&self.device_id)
        {
            return Ok(Some(manifest.written_by.clone()));
        }

        Ok(None)
    }
}
