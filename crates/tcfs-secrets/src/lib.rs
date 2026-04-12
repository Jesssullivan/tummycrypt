//! tcfs-secrets: SOPS/age/KDBX credential management
//!
//! S3 credential discovery chain (in order of precedence):
//!   1. SOPS-encrypted file (`storage.credentials_file`)
//!   2. RemoteJuggler KDBX store (`$REMOTE_JUGGLER_IDENTITY`)
//!   3. TCFS-specific env: `TCFS_S3_ACCESS` / `TCFS_S3_SECRET`
//!   4. AWS env: `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` (warns)
//!   5. Legacy env: `SEAWEED_ACCESS_KEY` / `SEAWEED_SECRET_KEY`
//!   6. File variants: `TCFS_S3_ACCESS_FILE`, `AWS_ACCESS_KEY_ID_FILE`
//!   7. AWS shared credentials file: `~/.aws/credentials`
//!
//! Age identity discovery chain (in order of precedence):
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
        // Track which source provided the credentials for logging
        let mut source = "env";

        // Try TCFS-specific env first (preferred — namespaced, intentional)
        let access_key = if let Ok(v) = std::env::var("TCFS_S3_ACCESS") {
            source = "env:TCFS_S3_ACCESS";
            Some(v)
        } else if let Ok(v) = std::env::var("AWS_ACCESS_KEY_ID") {
            // AWS env vars are visible in `ps aux` and crash dumps — warn
            tracing::warn!(
                "S3 credentials loaded from AWS_ACCESS_KEY_ID env var. \
                 Prefer SOPS file, TCFS_S3_ACCESS, or ~/.aws/credentials for production."
            );
            source = "env:AWS_ACCESS_KEY_ID";
            Some(v)
        } else if let Ok(v) = std::env::var("SEAWEED_ACCESS_KEY") {
            source = "env:SEAWEED_ACCESS_KEY";
            Some(v)
        } else if let Ok(v) = read_env_file("TCFS_S3_ACCESS_FILE") {
            source = "file:TCFS_S3_ACCESS_FILE";
            Some(v)
        } else if let Ok(v) = read_env_file("AWS_ACCESS_KEY_ID_FILE") {
            source = "file:AWS_ACCESS_KEY_ID_FILE";
            Some(v)
        } else {
            None
        };

        let secret_key = std::env::var("TCFS_S3_SECRET")
            .or_else(|_| std::env::var("AWS_SECRET_ACCESS_KEY"))
            .or_else(|_| std::env::var("SEAWEED_SECRET_KEY"))
            .or_else(|_| read_env_file("TCFS_S3_SECRET_FILE"))
            .or_else(|_| read_env_file("AWS_SECRET_ACCESS_KEY_FILE"))
            .ok();

        // If env vars didn't work, try AWS shared credentials file
        if access_key.is_none() || secret_key.is_none() {
            if let Some((ak, mut sk)) = parse_aws_credentials_file() {
                source = "aws-credentials-file";
                let s3 = S3Credentials {
                    access_key_id: ak,
                    secret_access_key: SecretString::from(std::mem::take(&mut sk)),
                    endpoint: storage.endpoint.clone(),
                    region: storage.region.clone(),
                };
                sk.zeroize();
                return Ok(CredStore {
                    s3: Some(s3),
                    source: source.into(),
                });
            }
        }

        let s3 = if let (Some(ak), Some(mut sk)) = (access_key, secret_key) {
            let creds = S3Credentials {
                access_key_id: ak,
                secret_access_key: SecretString::from(std::mem::take(&mut sk)),
                endpoint: storage.endpoint.clone(),
                region: storage.region.clone(),
            };
            sk.zeroize();
            Some(creds)
        } else {
            None
        };

        Ok(CredStore {
            s3,
            source: source.into(),
        })
    }
}

/// Read a credential value by resolving a `*_FILE` env var to a file path, then reading the file.
fn read_env_file(env_var: &str) -> Result<String, std::env::VarError> {
    let path = std::env::var(env_var)?;
    std::fs::read_to_string(path.trim())
        .map(|s| s.trim().to_string())
        .map_err(|_| std::env::VarError::NotPresent)
}

/// Parse the AWS shared credentials file (`~/.aws/credentials`).
///
/// Supports the standard INI format:
/// ```ini
/// [default]
/// aws_access_key_id = AKIA...
/// aws_secret_access_key = wJal...
/// ```
///
/// Respects `$AWS_SHARED_CREDENTIALS_FILE` for custom path and
/// `$AWS_PROFILE` for non-default profile selection.
fn parse_aws_credentials_file() -> Option<(String, String)> {
    let cred_path = std::env::var("AWS_SHARED_CREDENTIALS_FILE").ok().map(
        std::path::PathBuf::from,
    ).or_else(|| {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .ok()?;
        Some(std::path::PathBuf::from(home).join(".aws").join("credentials"))
    })?;

    let content = std::fs::read_to_string(&cred_path).ok()?;
    let profile = std::env::var("AWS_PROFILE").unwrap_or_else(|_| "default".into());
    let section_header = format!("[{}]", profile);

    let mut in_section = false;
    let mut access_key = None;
    let mut secret_key = None;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_section = trimmed == section_header;
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some((key, value)) = trimmed.split_once('=') {
            let key = key.trim();
            let value = value.trim();
            match key {
                "aws_access_key_id" => access_key = Some(value.to_string()),
                "aws_secret_access_key" => secret_key = Some(value.to_string()),
                _ => {}
            }
        }
    }

    match (access_key, secret_key) {
        (Some(ak), Some(sk)) => {
            tracing::info!("credentials loaded from AWS shared credentials file");
            Some((ak, sk))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::Mutex;

    /// Serialize env-var tests to avoid races (env vars are process-global).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Helper: clear all credential env vars to isolate tests.
    fn clear_cred_env() {
        for var in &[
            "TCFS_S3_ACCESS",
            "TCFS_S3_SECRET",
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "SEAWEED_ACCESS_KEY",
            "SEAWEED_SECRET_KEY",
            "TCFS_S3_ACCESS_FILE",
            "TCFS_S3_SECRET_FILE",
            "AWS_ACCESS_KEY_ID_FILE",
            "AWS_SECRET_ACCESS_KEY_FILE",
            "AWS_SHARED_CREDENTIALS_FILE",
            "AWS_PROFILE",
            "REMOTE_JUGGLER_IDENTITY",
        ] {
            std::env::remove_var(var);
        }
    }

    fn default_storage() -> tcfs_core::config::StorageConfig {
        tcfs_core::config::StorageConfig {
            endpoint: "http://localhost:8333".into(),
            region: "us-east-1".into(),
            ..Default::default()
        }
    }

    #[test]
    fn tcfs_env_takes_precedence_over_aws() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_cred_env();
        std::env::set_var("TCFS_S3_ACCESS", "tcfs-key");
        std::env::set_var("TCFS_S3_SECRET", "tcfs-secret");
        std::env::set_var("AWS_ACCESS_KEY_ID", "aws-key");
        std::env::set_var("AWS_SECRET_ACCESS_KEY", "aws-secret");

        let store = CredStore::load_from_env(&default_storage()).unwrap();
        let s3 = store.s3.unwrap();
        assert_eq!(s3.access_key_id, "tcfs-key");
        assert!(store.source.contains("TCFS_S3_ACCESS"));

        clear_cred_env();
    }

    #[test]
    fn file_variant_reads_content() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_cred_env();
        let dir = tempfile::tempdir().unwrap();

        let ak_path = dir.path().join("access_key");
        let sk_path = dir.path().join("secret_key");
        std::fs::write(&ak_path, "  file-access-key  \n").unwrap();
        std::fs::write(&sk_path, "file-secret-key\n").unwrap();

        std::env::set_var("TCFS_S3_ACCESS_FILE", ak_path.to_str().unwrap());
        std::env::set_var("TCFS_S3_SECRET_FILE", sk_path.to_str().unwrap());

        let store = CredStore::load_from_env(&default_storage()).unwrap();
        let s3 = store.s3.unwrap();
        assert_eq!(s3.access_key_id, "file-access-key"); // trimmed
        assert!(store.source.contains("FILE"));

        clear_cred_env();
    }

    #[test]
    fn aws_credentials_file_parsing() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_cred_env();
        let dir = tempfile::tempdir().unwrap();
        let cred_path = dir.path().join("credentials");

        let mut f = std::fs::File::create(&cred_path).unwrap();
        writeln!(f, "[default]").unwrap();
        writeln!(f, "aws_access_key_id = AKIAIOSFODNN7EXAMPLE").unwrap();
        writeln!(f, "aws_secret_access_key = wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY").unwrap();
        drop(f);

        std::env::set_var("AWS_SHARED_CREDENTIALS_FILE", cred_path.to_str().unwrap());

        let result = parse_aws_credentials_file();
        assert!(result.is_some());
        let (ak, sk) = result.unwrap();
        assert_eq!(ak, "AKIAIOSFODNN7EXAMPLE");
        assert_eq!(sk, "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY");

        clear_cred_env();
    }

    #[test]
    fn aws_credentials_file_with_profile() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_cred_env();
        let dir = tempfile::tempdir().unwrap();
        let cred_path = dir.path().join("credentials");

        let mut f = std::fs::File::create(&cred_path).unwrap();
        writeln!(f, "[default]").unwrap();
        writeln!(f, "aws_access_key_id = default-key").unwrap();
        writeln!(f, "aws_secret_access_key = default-secret").unwrap();
        writeln!(f).unwrap();
        writeln!(f, "[staging]").unwrap();
        writeln!(f, "aws_access_key_id = staging-key").unwrap();
        writeln!(f, "aws_secret_access_key = staging-secret").unwrap();
        drop(f);

        std::env::set_var("AWS_SHARED_CREDENTIALS_FILE", cred_path.to_str().unwrap());
        std::env::set_var("AWS_PROFILE", "staging");

        let result = parse_aws_credentials_file();
        assert!(result.is_some());
        let (ak, _sk) = result.unwrap();
        assert_eq!(ak, "staging-key");

        clear_cred_env();
    }

    #[test]
    fn missing_credentials_returns_none() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_cred_env();
        // Point to a nonexistent file to prevent ~/.aws/credentials from being found
        std::env::set_var("AWS_SHARED_CREDENTIALS_FILE", "/nonexistent/path");

        let store = CredStore::load_from_env(&default_storage()).unwrap();
        assert!(store.s3.is_none());

        clear_cred_env();
    }

    #[test]
    fn aws_credentials_file_used_as_fallback() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_cred_env();
        let dir = tempfile::tempdir().unwrap();
        let cred_path = dir.path().join("credentials");

        let mut f = std::fs::File::create(&cred_path).unwrap();
        writeln!(f, "[default]").unwrap();
        writeln!(f, "aws_access_key_id = file-ak").unwrap();
        writeln!(f, "aws_secret_access_key = file-sk").unwrap();
        drop(f);

        std::env::set_var("AWS_SHARED_CREDENTIALS_FILE", cred_path.to_str().unwrap());

        let store = CredStore::load_from_env(&default_storage()).unwrap();
        let s3 = store.s3.unwrap();
        assert_eq!(s3.access_key_id, "file-ak");
        assert_eq!(store.source, "aws-credentials-file");

        clear_cred_env();
    }

    #[test]
    fn seaweed_legacy_vars_work() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_cred_env();
        std::env::set_var("AWS_SHARED_CREDENTIALS_FILE", "/nonexistent");
        std::env::set_var("SEAWEED_ACCESS_KEY", "sw-ak");
        std::env::set_var("SEAWEED_SECRET_KEY", "sw-sk");

        let store = CredStore::load_from_env(&default_storage()).unwrap();
        let s3 = store.s3.unwrap();
        assert_eq!(s3.access_key_id, "sw-ak");
        assert!(store.source.contains("SEAWEED"));

        clear_cred_env();
    }
}
