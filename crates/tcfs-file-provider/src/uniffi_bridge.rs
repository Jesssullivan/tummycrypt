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

fn parse_manifest_hash_from_index(bytes: &[u8], path: &str) -> Result<String, ProviderError> {
    tcfs_sync::index_entry::parse_index_entry(bytes)
        .map(|entry| entry.manifest_hash)
        .map_err(|e| ProviderError::Storage {
            message: format!("parsing index entry {path}: {e}"),
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
        let master_key = if config.encryption_passphrase.is_empty() {
            None
        } else if config.encryption_passphrase.split_whitespace().count() >= 12 {
            let key = tcfs_crypto::mnemonic_to_master_key(&config.encryption_passphrase).map_err(
                |e| ProviderError::Decryption {
                    message: e.to_string(),
                },
            )?;
            Some(key)
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
            let prefix = format!(
                "{}/index/{}",
                self.remote_prefix.trim_end_matches('/'),
                path.trim_start_matches('/')
            );

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

                items.push(FileItem {
                    item_id: entry_path.to_string(),
                    filename: display_name.to_string(),
                    file_size: entry.metadata().content_length(),
                    modified_timestamp: modified_ts,
                    is_directory: is_dir,
                    content_hash: String::new(),
                    conflict_with,
                });
            }

            Ok(items)
        })
    }

    /// Hydrate (download + decrypt + reassemble) a file to a local path.
    pub fn hydrate_file(&self, item_id: &str, destination_path: &str) -> Result<(), ProviderError> {
        self.runtime.block_on(async {
            let data = self.operator.read(item_id).await?;
            let bytes = data.to_bytes();
            let manifest_hash = parse_manifest_hash_from_index(&bytes, item_id)?;

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

    /// Hydrate a file with progress reporting.
    ///
    /// Same as `hydrate_file` but calls the progress callback after each chunk.
    pub fn hydrate_file_with_progress(
        &self,
        item_id: &str,
        destination_path: &str,
        callback: Box<dyn ProgressCallback>,
    ) -> Result<(), ProviderError> {
        self.runtime.block_on(async {
            // Read index entry
            let data = self.operator.read(item_id).await?;
            let bytes = data.to_bytes();
            let manifest_hash = parse_manifest_hash_from_index(&bytes, item_id)?;

            // Fetch manifest
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
                if let Ok(hash) =
                    parse_manifest_hash_from_index(&existing_bytes, &existing_index_key)
                {
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
                mode: None,
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
            let index_entry = tcfs_sync::index_entry::RemoteIndexEntry::new(
                file_hash,
                data.len() as u64,
                chunks.len(),
            );
            tcfs_sync::index_entry::write_committed_index_entry(
                &self.operator,
                &index_key,
                &index_entry,
            )
            .await
            .map_err(ProviderError::from)?;

            Ok(())
        })
    }

    /// Delete a file or directory by its item ID.
    pub fn delete_item(&self, item_id: &str) -> Result<(), ProviderError> {
        self.runtime.block_on(async {
            if let Ok(data) = self.operator.read(item_id).await {
                let bytes = data.to_bytes();
                let manifest_prefix =
                    format!("{}/manifests", self.remote_prefix.trim_end_matches('/'));
                if let Ok(entry) = tcfs_sync::index_entry::parse_index_entry_record(&bytes) {
                    for manifest_path in entry.referenced_object_keys(&manifest_prefix) {
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

    /// Check if a file has a conflict (remote vclock diverged from ours).
    ///
    /// Returns the conflicting device ID if diverged, or None if clean.
    pub fn check_conflict(&self, item_id: &str) -> Result<Option<String>, ProviderError> {
        self.runtime.block_on(self.check_conflict_async(item_id))
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
    /// Returns credentials extracted from the invite for auto-configuration.
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

        // Create a session from the enrollment
        let session =
            tcfs_auth::Session::new(&self.device_id, &self.device_id, "enrollment").with_expiry(24);
        let token = session.token.clone();
        self.runtime.block_on(self.session_store.insert(session));

        Ok(EnrollmentResult {
            success: true,
            error_message: String::new(),
            device_id: invite.created_by.clone(),
            storage_endpoint: invite.storage_endpoint.unwrap_or_default(),
            storage_bucket: invite.storage_bucket.unwrap_or_default(),
            access_key: invite.storage_access_key.unwrap_or_default(),
            s3_secret: invite.storage_secret_key.unwrap_or_default(),
            remote_prefix: invite.remote_prefix.unwrap_or_else(|| "default".into()),
            encryption_passphrase: invite.encryption_passphrase.unwrap_or_default(),
            encryption_salt: invite.encryption_salt.unwrap_or_default(),
            session_token: token,
        })
    }

    /// Get sync status (connected check + file count from index).
    pub fn get_sync_status(&self) -> Result<SyncStatus, ProviderError> {
        let index_prefix = format!("{}/index/", self.remote_prefix.trim_end_matches('/'));

        self.runtime.block_on(async {
            match self.operator.list(&index_prefix).await {
                Ok(entries) => {
                    let file_count =
                        entries.iter().filter(|e| !e.path().ends_with('/')).count() as u64;
                    Ok(SyncStatus {
                        connected: true,
                        files_synced: file_count,
                        files_pending: 0,
                        last_error: None,
                    })
                }
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

impl TcfsProviderHandle {
    /// Internal async conflict check — used by both `check_conflict` and `list_items`.
    async fn check_conflict_async(&self, item_id: &str) -> Result<Option<String>, ProviderError> {
        let data = match self.operator.read(item_id).await {
            Ok(d) => d,
            Err(_) => return Ok(None),
        };
        let bytes = data.to_bytes();
        let manifest_hash = match parse_manifest_hash_from_index(&bytes, item_id) {
            Ok(hash) => hash,
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
