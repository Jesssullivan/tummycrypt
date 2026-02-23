//! tcfs-secrets: SOPS/age/KDBX credential management
//!
//! Identity discovery chain (in order of precedence):
//!   1. $CREDENTIALS_DIRECTORY/age-identity  (systemd LoadCredentialEncrypted)
//!   2. $SOPS_AGE_KEY_FILE env var (path to key file)
//!   3. $SOPS_AGE_KEY env var (literal key content)
//!   4. ~/.config/sops/age/keys.txt (default fallback)

pub mod age;
pub mod device;
pub mod identity;
pub mod kdbx;
pub mod keychain;
pub mod rotate;
pub mod sops;

pub use identity::{find_age_identity, IdentityProvider};
pub use kdbx::{KdbxCredential, KdbxStore};
pub use sops::{decrypt_sops_file, SopsCredentials, SopsFile};

use anyhow::Result;
use secrecy::SecretString;
use std::path::Path;
use zeroize::Zeroize;

/// Loaded S3 credentials, sourced from SOPS-encrypted file or environment.
///
/// `secret_access_key` is held in a `SecretString` that is zeroized on drop,
/// preventing secret material from lingering in process memory.
#[derive(Clone)]
pub struct S3Credentials {
    pub access_key_id: String,
    pub secret_access_key: SecretString,
    pub endpoint: String,
    pub region: String,
}

impl std::fmt::Debug for S3Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Credentials")
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"[REDACTED]")
            .field("endpoint", &self.endpoint)
            .field("region", &self.region)
            .finish()
    }
}

/// Credential store: loads and caches credentials, watches for file changes
pub struct CredStore {
    pub s3: Option<S3Credentials>,
    pub source: String,
}

impl CredStore {
    /// Load credentials using the full discovery chain:
    /// 1. SOPS-encrypted file (decrypted with age identity)
    /// 2. RemoteJuggler KDBX store (if $REMOTE_JUGGLER_IDENTITY is set)
    /// 3. Environment variables (AWS_ACCESS_KEY_ID etc.)
    pub async fn load(
        config: &tcfs_core::config::SecretsConfig,
        storage: &tcfs_core::config::StorageConfig,
    ) -> Result<Self> {
        // Try SOPS credential file first
        if let Some(cred_file) = &storage.credentials_file {
            if cred_file.exists() {
                match Self::load_from_sops(cred_file, config).await {
                    Ok(store) => return Ok(store),
                    Err(e) => {
                        tracing::warn!("SOPS credential load failed: {e}, falling back")
                    }
                }
            }
        }

        // Try RemoteJuggler KDBX store (if identity is configured)
        if std::env::var("REMOTE_JUGGLER_IDENTITY").is_ok() {
            match Self::load_from_remote_juggler(storage).await {
                Ok(store) => return Ok(store),
                Err(e) => {
                    tracing::debug!("RemoteJuggler credential load skipped: {e}")
                }
            }
        }

        // Fall back to environment variables
        Self::load_from_env(storage)
    }

    async fn load_from_sops(
        cred_file: &Path,
        secrets_config: &tcfs_core::config::SecretsConfig,
    ) -> Result<Self> {
        let identity = identity::find_age_identity(secrets_config).await?;
        let mut creds = sops::decrypt_sops_file(cred_file, &identity).await?;

        let s3 = S3Credentials {
            access_key_id: creds.access_key_id.clone(),
            secret_access_key: SecretString::from(std::mem::take(&mut creds.secret_access_key)),
            endpoint: creds.endpoint.clone().unwrap_or_default(),
            region: creds.region.clone().unwrap_or_else(|| "us-east-1".into()),
        };
        // Zeroize the plaintext copy
        creds.secret_access_key.zeroize();

        Ok(CredStore {
            s3: Some(s3),
            source: format!("sops:{}", cred_file.display()),
        })
    }

    /// Attempt to load S3 credentials from RemoteJuggler's KDBX store.
    ///
    /// Shells out to `remote-juggler kdbx get tcfs/s3-credentials --format json`
    /// and parses the JSON output. This is a best-effort fallback -- if
    /// RemoteJuggler is not installed or the entry doesn't exist, returns an error
    /// and the discovery chain continues.
    async fn load_from_remote_juggler(storage: &tcfs_core::config::StorageConfig) -> Result<Self> {
        let mut cmd = tokio::process::Command::new("remote-juggler");
        cmd.args(["kdbx", "get", "tcfs/s3-credentials", "--format", "json"]);

        // Use TCFS_KDBX_PATH if set (from Nix module)
        if let Ok(kdbx_path) = std::env::var("TCFS_KDBX_PATH") {
            cmd.args(["--database", &kdbx_path]);
        }

        let output = cmd
            .output()
            .await
            .map_err(|e| anyhow::anyhow!("remote-juggler not available: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("remote-juggler kdbx get failed: {stderr}");
        }

        let json: serde_json::Value = serde_json::from_slice(&output.stdout)
            .map_err(|e| anyhow::anyhow!("parsing remote-juggler output: {e}"))?;

        let access_key = json
            .get("access_key_id")
            .or_else(|| json.get("username"))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        let mut secret_key = json
            .get("secret_access_key")
            .or_else(|| json.get("password"))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        if access_key.is_empty() || secret_key.is_empty() {
            anyhow::bail!("remote-juggler returned empty credentials");
        }

        let endpoint = json
            .get("endpoint")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| storage.endpoint.clone());

        let region = json
            .get("region")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| storage.region.clone());

        let s3 = S3Credentials {
            access_key_id: access_key,
            secret_access_key: SecretString::from(std::mem::take(&mut secret_key)),
            endpoint,
            region,
        };
        secret_key.zeroize();

        tracing::info!("credentials loaded from RemoteJuggler KDBX store");

        Ok(CredStore {
            s3: Some(s3),
            source: "remote-juggler:kdbx".into(),
        })
    }

    fn load_from_env(storage: &tcfs_core::config::StorageConfig) -> Result<Self> {
        let access_key = std::env::var("TCFS_S3_ACCESS")
            .or_else(|_| std::env::var("AWS_ACCESS_KEY_ID"))
            .or_else(|_| std::env::var("SEAWEED_ACCESS_KEY"))
            .unwrap_or_default();
        let mut secret_key = std::env::var("TCFS_S3_SECRET")
            .or_else(|_| std::env::var("AWS_SECRET_ACCESS_KEY"))
            .or_else(|_| std::env::var("SEAWEED_SECRET_KEY"))
            .unwrap_or_default();

        let s3 = if !access_key.is_empty() {
            let creds = S3Credentials {
                access_key_id: access_key,
                secret_access_key: SecretString::from(std::mem::take(&mut secret_key)),
                endpoint: storage.endpoint.clone(),
                region: storage.region.clone(),
            };
            secret_key.zeroize();
            Some(creds)
        } else {
            None
        };

        Ok(CredStore {
            s3,
            source: "env".into(),
        })
    }
}
