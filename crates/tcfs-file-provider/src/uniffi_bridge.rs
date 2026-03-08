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

/// Configuration for the TCFS provider.
///
/// On iOS, these values come from the Keychain via the Swift layer.
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
    pub content_hash: String,
}

/// Sync status summary.
#[derive(uniffi::Record)]
pub struct SyncStatus {
    pub connected: bool,
    pub files_synced: u64,
    pub files_pending: u64,
    pub last_error: Option<String>,
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
}

#[uniffi::export]
impl TcfsProviderHandle {
    /// Create a new provider from configuration.
    #[uniffi::constructor]
    pub fn new(config: ProviderConfig) -> Result<Arc<Self>, ProviderError> {
        let master_key = if config.encryption_passphrase.is_empty() {
            None
        } else {
            let mut salt = [0u8; 16];
            let salt_bytes = config.encryption_salt.as_bytes();
            let copy_len = salt_bytes.len().min(16);
            salt[..copy_len].copy_from_slice(&salt_bytes[..copy_len]);

            let params = tcfs_crypto::kdf::KdfParams {
                mem_cost_kib: 65536,
                time_cost: 3,
                parallelism: 4,
            };

            let key = tcfs_crypto::derive_master_key(
                &SecretString::from(config.encryption_passphrase),
                &salt,
                &params,
            )
            .map_err(|e| ProviderError::Decryption {
                message: e.to_string(),
            })?;

            Some(key)
        };

        let runtime = tokio::runtime::Runtime::new().map_err(|e| ProviderError::Storage {
            message: format!("failed to create tokio runtime: {e}"),
        })?;

        let operator =
            tcfs_storage::operator::build_operator(&tcfs_storage::operator::StorageConfig {
                endpoint: config.s3_endpoint,
                region: "us-east-1".to_string(),
                bucket: config.s3_bucket,
                access_key_id: config.access_key,
                secret_access_key: config.s3_secret,
            })
            .map_err(|e| ProviderError::Storage {
                message: e.to_string(),
            })?;

        Ok(Arc::new(Self {
            runtime,
            operator,
            remote_prefix: config.remote_prefix,
            device_id: config.device_id,
            master_key,
        }))
    }

    /// List files at a given relative path.
    pub fn list_items(&self, path: &str) -> Result<Vec<FileItem>, ProviderError> {
        let prefix = format!(
            "{}/index/{}",
            self.remote_prefix.trim_end_matches('/'),
            path.trim_start_matches('/')
        );

        let entries = self
            .runtime
            .block_on(self.operator.list(&prefix))
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

            items.push(FileItem {
                item_id: entry_path.to_string(),
                filename: display_name.to_string(),
                file_size: entry.metadata().content_length(),
                modified_timestamp: 0,
                is_directory: is_dir,
                content_hash: String::new(),
            });
        }

        Ok(items)
    }

    /// Hydrate (download + decrypt + reassemble) a file to a local path.
    pub fn hydrate_file(&self, item_id: &str, destination_path: &str) -> Result<(), ProviderError> {
        self.runtime.block_on(async {
            let data = self.operator.read(item_id).await?;
            let bytes = data.to_bytes();
            let text = String::from_utf8_lossy(&bytes);

            let mut manifest_hash = String::new();
            for line in text.lines() {
                if let Some(val) = line.strip_prefix("manifest_hash=") {
                    manifest_hash = val.to_string();
                }
            }

            if manifest_hash.is_empty() {
                return Err(ProviderError::NotFound {
                    path: item_id.to_string(),
                });
            }

            let manifest_path = format!(
                "{}/manifests/{}",
                self.remote_prefix.trim_end_matches('/'),
                manifest_hash
            );

            let manifest_bytes = self.operator.read(&manifest_path).await?;
            let manifest = tcfs_sync::manifest::SyncManifest::from_bytes(
                &manifest_bytes.to_bytes(),
            )
            .map_err(|e| ProviderError::Storage {
                message: e.to_string(),
            })?;

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
        self.runtime.block_on(async {
            let data = tokio::fs::read(local_path)
                .await
                .map_err(|e| ProviderError::Storage {
                    message: format!("read {}: {}", local_path, e),
                })?;

            let file_hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(&data));

            let file_key = self
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
                    tcfs_crypto::encrypt_chunk(fk, idx as u64, &file_id_bytes, chunk_bytes)
                        .map_err(|e| ProviderError::Decryption {
                            message: e.to_string(),
                        })?
                } else {
                    chunk_bytes.to_vec()
                };

                let hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(&upload_bytes));
                let chunk_key = format!(
                    "{}/chunks/{}",
                    self.remote_prefix.trim_end_matches('/'),
                    hash
                );
                self.operator.write(&chunk_key, upload_bytes).await?;
                chunk_hashes.push(hash);
            }

            // Build vclock
            let mut vclock = tcfs_sync::conflict::VectorClock::new();
            let existing_index_key = format!(
                "{}/index/{}",
                self.remote_prefix.trim_end_matches('/'),
                remote_path.trim_start_matches('/')
            );
            if let Ok(existing_data) = self.operator.read(&existing_index_key).await {
                let existing_bytes = existing_data.to_bytes();
                let existing_text = String::from_utf8_lossy(&existing_bytes);
                for line in existing_text.lines() {
                    if let Some(hash) = line.strip_prefix("manifest_hash=") {
                        let manifest_path = format!(
                            "{}/manifests/{}",
                            self.remote_prefix.trim_end_matches('/'),
                            hash
                        );
                        if let Ok(mb) = self.operator.read(&manifest_path).await {
                            if let Ok(existing_manifest) =
                                tcfs_sync::manifest::SyncManifest::from_bytes(&mb.to_bytes())
                            {
                                vclock.merge(&existing_manifest.vclock);
                            }
                        }
                    }
                }
            }
            vclock.tick(&self.device_id);

            let written_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            let encrypted_file_key = match (&self.master_key, &file_key) {
                (Some(mk), Some(fk)) => {
                    let wrapped =
                        tcfs_crypto::wrap_key(mk, fk).map_err(|e| ProviderError::Decryption {
                            message: e.to_string(),
                        })?;
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
                written_by: self.device_id.clone(),
                written_at,
                rel_path: Some(remote_path.to_string()),
                encrypted_file_key,
            };

            let manifest_json =
                serde_json::to_vec_pretty(&manifest).map_err(|e| ProviderError::Storage {
                    message: e.to_string(),
                })?;
            let manifest_key = format!(
                "{}/manifests/{}",
                self.remote_prefix.trim_end_matches('/'),
                file_hash
            );
            self.operator.write(&manifest_key, manifest_json).await?;

            let index_key = format!(
                "{}/index/{}",
                self.remote_prefix.trim_end_matches('/'),
                remote_path.trim_start_matches('/')
            );
            let index_entry = format!(
                "manifest_hash={}\nsize={}\nchunks={}\n",
                file_hash,
                data.len(),
                chunks.len()
            );
            self.operator.write(&index_key, index_entry).await?;

            Ok(())
        })
    }

    /// Delete a file or directory by its item ID.
    pub fn delete_item(&self, item_id: &str) -> Result<(), ProviderError> {
        self.runtime.block_on(async {
            if let Ok(data) = self.operator.read(item_id).await {
                let bytes = data.to_bytes();
                let text = String::from_utf8_lossy(&bytes);
                for line in text.lines() {
                    if let Some(hash) = line.strip_prefix("manifest_hash=") {
                        let manifest_path = format!(
                            "{}/manifests/{}",
                            self.remote_prefix.trim_end_matches('/'),
                            hash
                        );
                        let _ = self.operator.delete(&manifest_path).await;
                    }
                }
            }

            self.operator.delete(item_id).await?;
            Ok(())
        })
    }

    /// Create a directory under the given parent path.
    pub fn create_directory(&self, parent_path: &str, dir_name: &str) -> Result<(), ProviderError> {
        let dir_path = format!(
            "{}/index/{}{}/",
            self.remote_prefix.trim_end_matches('/'),
            if parent_path.is_empty() {
                String::new()
            } else {
                format!("{}/", parent_path.trim_matches('/'))
            },
            dir_name.trim_matches('/')
        );

        self.runtime
            .block_on(self.operator.write(&dir_path, Vec::<u8>::new()))
            .map(|_| ())
            .map_err(ProviderError::from)
    }

    /// Get sync status (connected check via health probe).
    pub fn get_sync_status(&self) -> Result<SyncStatus, ProviderError> {
        let connected = self.runtime.block_on(async {
            let prefix = format!("{}/", self.remote_prefix.trim_end_matches('/'));
            self.operator.list(&prefix).await.is_ok()
        });

        Ok(SyncStatus {
            connected,
            files_synced: 0,
            files_pending: 0,
            last_error: None,
        })
    }
}
