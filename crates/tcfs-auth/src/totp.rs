//! TOTP (RFC 6238) authentication provider.
//!
//! Provides time-based one-time password authentication for device unlock.
//! Each device registers a shared secret; the user enters the 6-digit code
//! from their authenticator app (Google Authenticator, Authy, 1Password, etc.).
//!
//! # Registration Flow
//!
//! 1. `register(device_id)` → generates secret + QR code URI + SVG
//! 2. User scans QR code with authenticator app
//! 3. User enters code to confirm enrollment → `verify(response)`
//!
//! # Authentication Flow
//!
//! 1. `challenge(device_id)` → empty challenge (TOTP is time-based)
//! 2. User enters 6-digit code from authenticator
//! 3. `verify(response)` → validates code against stored secret

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use totp_rs::{Algorithm, Secret, TOTP};
use tracing::{debug, info, warn};

use crate::provider::{AuthChallenge, AuthProvider, AuthResponse, RegistrationData, VerifyResult};
use crate::session::Session;

/// TOTP configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TotpConfig {
    /// Issuer name shown in authenticator apps.
    pub issuer: String,
    /// Number of digits (default: 6).
    pub digits: usize,
    /// Time step in seconds (default: 30).
    pub period: u64,
    /// Hash algorithm (default: SHA1 for compatibility).
    pub algorithm: TotpAlgorithm,
    /// Number of periods to allow for clock skew (default: 1).
    pub skew: u8,
}

impl Default for TotpConfig {
    fn default() -> Self {
        Self {
            issuer: "TummyCrypt".to_string(),
            digits: 6,
            period: 30,
            algorithm: TotpAlgorithm::Sha1,
            skew: 1,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum TotpAlgorithm {
    Sha1,
    Sha256,
    Sha512,
}

impl From<TotpAlgorithm> for Algorithm {
    fn from(alg: TotpAlgorithm) -> Self {
        match alg {
            TotpAlgorithm::Sha1 => Algorithm::SHA1,
            TotpAlgorithm::Sha256 => Algorithm::SHA256,
            TotpAlgorithm::Sha512 => Algorithm::SHA512,
        }
    }
}

/// Stored TOTP credential for a device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TotpCredential {
    pub device_id: String,
    pub secret: String, // Base32-encoded shared secret
    pub enrolled_at: u64,
    pub last_used_at: Option<u64>,
}

/// TOTP authentication provider.
pub struct TotpProvider {
    config: TotpConfig,
    /// Device ID → TOTP credential mapping.
    credentials: Arc<RwLock<HashMap<String, TotpCredential>>>,
}

impl TotpProvider {
    pub fn new(config: TotpConfig) -> Self {
        Self {
            config,
            credentials: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Load credentials from a JSON file.
    pub async fn load_from_file(&self, path: &std::path::Path) -> anyhow::Result<()> {
        let data = tokio::fs::read_to_string(path).await?;
        let creds: HashMap<String, TotpCredential> = serde_json::from_str(&data)?;
        *self.credentials.write().await = creds;
        info!(path = %path.display(), "loaded TOTP credentials");
        Ok(())
    }

    /// Save credentials to a JSON file.
    pub async fn save_to_file(&self, path: &std::path::Path) -> anyhow::Result<()> {
        let creds = self.credentials.read().await;
        let data = serde_json::to_string_pretty(&*creds)?;
        tokio::fs::write(path, data).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        debug!(path = %path.display(), "saved TOTP credentials (mode 0600)");
        Ok(())
    }

    /// Build a TOTP instance for a given secret.
    fn build_totp(&self, secret_base32: &str, account: &str) -> anyhow::Result<TOTP> {
        let secret = Secret::Encoded(secret_base32.to_string())
            .to_bytes()
            .map_err(|e| anyhow::anyhow!("invalid TOTP secret: {e}"))?;
        Ok(TOTP::new(
            self.config.algorithm.into(),
            self.config.digits,
            self.config.skew,
            self.config.period,
            secret,
            Some(self.config.issuer.clone()),
            account.to_string(),
        )?)
    }

    /// Get the number of enrolled devices.
    pub async fn enrolled_count(&self) -> usize {
        self.credentials.read().await.len()
    }
}

#[async_trait]
impl AuthProvider for TotpProvider {
    fn name(&self) -> &str {
        "totp"
    }

    async fn challenge(&self, device_id: &str) -> anyhow::Result<AuthChallenge> {
        // Verify the device has a registered TOTP credential
        let creds = self.credentials.read().await;
        if !creds.contains_key(device_id) {
            return Err(anyhow::anyhow!(
                "device {device_id} has no TOTP credential — run enrollment first"
            ));
        }

        // TOTP challenges are empty — the code is time-based, generated locally
        Ok(AuthChallenge {
            challenge_id: uuid::Uuid::new_v4().to_string(),
            data: Vec::new(),
            prompt: format!(
                "Enter {}-digit code from your authenticator app",
                self.config.digits
            ),
            expires_at: {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                now + self.config.period * 2 // Allow 2 periods for entry
            },
        })
    }

    async fn verify(&self, response: &AuthResponse) -> anyhow::Result<VerifyResult> {
        let code = String::from_utf8(response.data.clone())
            .map_err(|_| anyhow::anyhow!("TOTP code must be UTF-8"))?;
        let code = code.trim();

        let creds = self.credentials.read().await;
        let cred = match creds.get(&response.device_id) {
            Some(c) => c,
            None => {
                return Ok(VerifyResult::Failure {
                    reason: format!("no TOTP credential for device {}", response.device_id),
                });
            }
        };

        let totp = self.build_totp(&cred.secret, &response.device_id)?;

        if totp.check_current(code)? {
            info!(device_id = %response.device_id, "TOTP verification succeeded");

            // Update last_used timestamp
            drop(creds);
            let mut creds = self.credentials.write().await;
            if let Some(cred) = creds.get_mut(&response.device_id) {
                cred.last_used_at = Some(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                );
            }

            let session = Session::new(&response.device_id, &response.device_id, "totp");
            Ok(VerifyResult::Success {
                session_token: session.token,
                device_id: response.device_id.clone(),
            })
        } else {
            warn!(device_id = %response.device_id, "TOTP verification failed");
            Ok(VerifyResult::Failure {
                reason: "invalid TOTP code".to_string(),
            })
        }
    }

    async fn register(&self, device_id: &str) -> anyhow::Result<RegistrationData> {
        // Generate a random 160-bit secret (RFC 4226 recommends >= 128 bits)
        let secret = Secret::generate_secret();
        let secret_base32 = secret.to_encoded().to_string();

        let totp = self.build_totp(&secret_base32, device_id)?;
        let qr_uri = totp.get_url();

        // Generate QR code SVG
        #[cfg(feature = "totp")]
        let qr_svg = {
            use qrcode::QrCode;
            let code = QrCode::new(qr_uri.as_bytes())?;
            let svg = code
                .render::<qrcode::render::svg::Color>()
                .min_dimensions(200, 200)
                .build();
            svg
        };
        #[cfg(not(feature = "totp"))]
        let qr_svg = String::new();

        // Store the credential
        let cred = TotpCredential {
            device_id: device_id.to_string(),
            secret: secret_base32.clone(),
            enrolled_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            last_used_at: None,
        };
        self.credentials
            .write()
            .await
            .insert(device_id.to_string(), cred);

        info!(device_id, "TOTP credential registered");

        let reg_data = serde_json::json!({
            "secret": secret_base32,
            "qr_uri": qr_uri,
            "qr_svg": qr_svg,
            "algorithm": format!("{:?}", self.config.algorithm),
            "digits": self.config.digits,
            "period": self.config.period,
        });

        Ok(RegistrationData {
            data: serde_json::to_vec(&reg_data)?,
            instructions: format!(
                "Scan the QR code with your authenticator app, or manually enter the secret: {}",
                secret_base32
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_totp_registration() {
        let provider = TotpProvider::new(TotpConfig::default());

        let reg = provider.register("test-device").await.unwrap();
        assert!(!reg.data.is_empty());
        assert!(reg.instructions.contains("Scan the QR code"));

        let reg_json: serde_json::Value = serde_json::from_slice(&reg.data).unwrap();
        assert!(reg_json["secret"].is_string());
        assert!(reg_json["qr_uri"]
            .as_str()
            .unwrap()
            .starts_with("otpauth://totp/"));
        assert_eq!(reg_json["digits"], 6);
        assert_eq!(reg_json["period"], 30);

        assert_eq!(provider.enrolled_count().await, 1);
    }

    #[tokio::test]
    async fn test_totp_challenge_requires_enrollment() {
        let provider = TotpProvider::new(TotpConfig::default());

        // Challenge without enrollment should fail
        let result = provider.challenge("unknown-device").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_totp_verify_current_code() {
        let provider = TotpProvider::new(TotpConfig::default());

        // Register device
        let reg = provider.register("test-device").await.unwrap();
        let reg_json: serde_json::Value = serde_json::from_slice(&reg.data).unwrap();
        let secret = reg_json["secret"].as_str().unwrap();

        // Generate the current valid code
        let totp = TOTP::new(
            Algorithm::SHA1,
            6,
            1,
            30,
            Secret::Encoded(secret.to_string()).to_bytes().unwrap(),
            Some("TummyCrypt".to_string()),
            "test-device".to_string(),
        )
        .unwrap();
        let code = totp.generate_current().unwrap();

        // Verify it
        let response = AuthResponse {
            challenge_id: String::new(),
            data: code.as_bytes().to_vec(),
            device_id: "test-device".to_string(),
        };
        let result = provider.verify(&response).await.unwrap();
        assert!(matches!(result, VerifyResult::Success { .. }));
    }

    #[tokio::test]
    async fn test_totp_verify_bad_code() {
        let provider = TotpProvider::new(TotpConfig::default());
        provider.register("test-device").await.unwrap();

        let response = AuthResponse {
            challenge_id: String::new(),
            data: b"000000".to_vec(),
            device_id: "test-device".to_string(),
        };
        let result = provider.verify(&response).await.unwrap();
        assert!(matches!(result, VerifyResult::Failure { .. }));
    }
}
