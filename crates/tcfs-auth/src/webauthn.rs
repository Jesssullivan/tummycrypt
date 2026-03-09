//! WebAuthn / FIDO2 authentication provider.
//!
//! Provides passkey-based authentication using platform authenticators
//! (Touch ID, Windows Hello, security keys). Uses `webauthn-rs` for
//! server-side credential management.
//!
//! # Registration Flow
//!
//! 1. `register(device_id)` → PublicKeyCredentialCreationOptions (JSON)
//! 2. Client calls navigator.credentials.create() or platform authenticator
//! 3. Client sends attestation → `complete_registration(device_id, attestation)`
//!
//! # Authentication Flow
//!
//! 1. `challenge(device_id)` → PublicKeyCredentialRequestOptions (JSON)
//! 2. Client calls navigator.credentials.get() or platform authenticator
//! 3. `verify(response)` → validates assertion against stored credential

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use webauthn_rs::prelude::*;

use crate::provider::{AuthChallenge, AuthProvider, AuthResponse, RegistrationData, VerifyResult};

/// Configuration for the WebAuthn provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebAuthnConfig {
    /// Relying party name (shown to user during registration).
    pub rp_name: String,
    /// Relying party ID (domain or app identifier).
    pub rp_id: String,
    /// Relying party origin (URL for browser-based auth).
    pub rp_origin: String,
}

impl Default for WebAuthnConfig {
    fn default() -> Self {
        Self {
            rp_name: "TummyCrypt".to_string(),
            rp_id: "tcfs.local".to_string(),
            rp_origin: "https://tcfs.local".to_string(),
        }
    }
}

/// Stored credential for a registered device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebAuthnCredential {
    pub device_id: String,
    pub user_id: Vec<u8>,
    pub passkey: Passkey,
    pub enrolled_at: chrono::DateTime<chrono::Utc>,
}

/// Pending registration state (between create and complete).
#[derive(Debug)]
#[allow(dead_code)]
struct PendingRegistration {
    state: PasskeyRegistration,
    device_id: String,
}

/// Pending authentication state (between challenge and verify).
#[derive(Debug)]
struct PendingAuth {
    state: PasskeyAuthentication,
    device_id: String,
}

/// WebAuthn authentication provider.
pub struct WebAuthnProvider {
    webauthn: Arc<Webauthn>,
    credentials: Arc<RwLock<HashMap<String, WebAuthnCredential>>>,
    pending_registrations: Arc<RwLock<HashMap<String, PendingRegistration>>>,
    pending_auths: Arc<RwLock<HashMap<String, PendingAuth>>>,
}

impl WebAuthnProvider {
    /// Create a new WebAuthn provider with the given configuration.
    pub fn new(config: WebAuthnConfig) -> anyhow::Result<Self> {
        let rp_origin = Url::parse(&config.rp_origin)
            .map_err(|e| anyhow::anyhow!("invalid rp_origin URL: {e}"))?;

        let builder = WebauthnBuilder::new(&config.rp_id, &rp_origin)
            .map_err(|e| anyhow::anyhow!("WebAuthn builder error: {e}"))?
            .rp_name(&config.rp_name);

        let webauthn = builder
            .build()
            .map_err(|e| anyhow::anyhow!("WebAuthn build error: {e}"))?;

        Ok(Self {
            webauthn: Arc::new(webauthn),
            credentials: Arc::new(RwLock::new(HashMap::new())),
            pending_registrations: Arc::new(RwLock::new(HashMap::new())),
            pending_auths: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Complete a registration ceremony with the client's attestation response.
    pub async fn complete_registration(
        &self,
        device_id: &str,
        attestation: &RegisterPublicKeyCredential,
    ) -> anyhow::Result<()> {
        let pending = {
            let mut regs = self.pending_registrations.write().await;
            regs.remove(device_id)
                .ok_or_else(|| anyhow::anyhow!("no pending registration for device {device_id}"))?
        };

        let passkey = self
            .webauthn
            .finish_passkey_registration(attestation, &pending.state)
            .map_err(|e| anyhow::anyhow!("registration verification failed: {e}"))?;

        let credential = WebAuthnCredential {
            device_id: device_id.to_string(),
            user_id: uuid::Uuid::new_v4().as_bytes().to_vec(),
            passkey,
            enrolled_at: chrono::Utc::now(),
        };

        self.credentials
            .write()
            .await
            .insert(device_id.to_string(), credential);

        info!(device_id, "WebAuthn credential registered");
        Ok(())
    }

    /// Load credentials from a JSON file.
    pub async fn load_from_file(&self, path: &std::path::Path) -> anyhow::Result<()> {
        let data = tokio::fs::read_to_string(path).await?;
        let creds: HashMap<String, WebAuthnCredential> = serde_json::from_str(&data)?;
        *self.credentials.write().await = creds;
        info!(path = %path.display(), "loaded WebAuthn credentials");
        Ok(())
    }

    /// Save credentials to a JSON file.
    pub async fn save_to_file(&self, path: &std::path::Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let creds = self.credentials.read().await;
        let json = serde_json::to_string_pretty(&*creds)?;
        tokio::fs::write(path, json).await?;
        debug!(path = %path.display(), "saved WebAuthn credentials");
        Ok(())
    }
}

#[async_trait]
impl AuthProvider for WebAuthnProvider {
    fn name(&self) -> &str {
        "webauthn"
    }

    async fn challenge(&self, device_id: &str) -> anyhow::Result<AuthChallenge> {
        let creds = self.credentials.read().await;
        let credential = creds
            .get(device_id)
            .ok_or_else(|| anyhow::anyhow!("no WebAuthn credential for device {device_id}"))?;

        let (rcr, auth_state) = self
            .webauthn
            .start_passkey_authentication(&[credential.passkey.clone()])
            .map_err(|e| anyhow::anyhow!("failed to start authentication: {e}"))?;

        let challenge_id = uuid::Uuid::new_v4().to_string();

        self.pending_auths.write().await.insert(
            challenge_id.clone(),
            PendingAuth {
                state: auth_state,
                device_id: device_id.to_string(),
            },
        );

        let data = serde_json::to_vec(&rcr)?;

        debug!(device_id, challenge_id = %challenge_id, "WebAuthn challenge issued");
        Ok(AuthChallenge {
            challenge_id,
            data,
            prompt: "Complete the passkey authentication on your device".into(),
            expires_at: (chrono::Utc::now() + chrono::Duration::minutes(5)).timestamp() as u64,
        })
    }

    async fn verify(&self, response: &AuthResponse) -> anyhow::Result<VerifyResult> {
        let pending = {
            let mut auths = self.pending_auths.write().await;
            match auths.remove(&response.challenge_id) {
                Some(p) => p,
                None => {
                    return Ok(VerifyResult::Expired);
                }
            }
        };

        let assertion: PublicKeyCredential = serde_json::from_slice(&response.data)
            .map_err(|e| anyhow::anyhow!("invalid assertion JSON: {e}"))?;

        match self
            .webauthn
            .finish_passkey_authentication(&assertion, &pending.state)
        {
            Ok(result) => {
                // Update credential counter to prevent replay
                if let Some(cred) = self.credentials.write().await.get_mut(&pending.device_id) {
                    cred.passkey.update_credential(&result);
                }

                info!(device_id = %pending.device_id, "WebAuthn authentication succeeded");
                Ok(VerifyResult::Success {
                    session_token: uuid::Uuid::new_v4().to_string(),
                    device_id: pending.device_id,
                })
            }
            Err(e) => {
                warn!(device_id = %pending.device_id, error = %e, "WebAuthn authentication failed");
                Ok(VerifyResult::Failure {
                    reason: format!("assertion verification failed: {e}"),
                })
            }
        }
    }

    async fn register(&self, device_id: &str) -> anyhow::Result<RegistrationData> {
        let user_id = uuid::Uuid::new_v4();
        let user_name = format!("tcfs-{device_id}");

        let (ccr, reg_state) = self
            .webauthn
            .start_passkey_registration(
                user_id, &user_name, &user_name,
                // Exclude existing credentials for this device
                None,
            )
            .map_err(|e| anyhow::anyhow!("failed to start registration: {e}"))?;

        self.pending_registrations.write().await.insert(
            device_id.to_string(),
            PendingRegistration {
                state: reg_state,
                device_id: device_id.to_string(),
            },
        );

        let data = serde_json::to_vec(&ccr)?;

        info!(device_id, "WebAuthn registration started");
        Ok(RegistrationData {
            data,
            instructions: "Complete the passkey registration on your device. \
                          On iOS/macOS, Touch ID or Face ID will be prompted. \
                          On other platforms, use your security key."
                .into(),
        })
    }

    fn is_available(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_webauthn_provider_creation() {
        let config = WebAuthnConfig::default();
        let provider = WebAuthnProvider::new(config).unwrap();
        assert_eq!(provider.name(), "webauthn");
        assert!(provider.is_available());
    }

    #[tokio::test]
    async fn test_challenge_requires_enrollment() {
        let config = WebAuthnConfig::default();
        let provider = WebAuthnProvider::new(config).unwrap();
        let result = provider.challenge("unknown-device").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_verify_expired_challenge() {
        let config = WebAuthnConfig::default();
        let provider = WebAuthnProvider::new(config).unwrap();
        let response = AuthResponse {
            challenge_id: "nonexistent".into(),
            data: Vec::new(),
            device_id: "test".into(),
        };
        let result = provider.verify(&response).await.unwrap();
        assert!(matches!(result, VerifyResult::Expired));
    }

    #[tokio::test]
    async fn test_registration_starts() {
        let config = WebAuthnConfig::default();
        let provider = WebAuthnProvider::new(config).unwrap();
        let reg = provider.register("test-device").await.unwrap();
        assert!(!reg.data.is_empty());
        assert!(!reg.instructions.is_empty());

        // Should have a pending registration
        let pending = provider.pending_registrations.read().await;
        assert!(pending.contains_key("test-device"));
    }
}
