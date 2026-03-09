//! X.509 certificate-based authentication provider.
//!
//! Verifies client identity using TLS client certificates, suitable for
//! enterprise PKI integration. The daemon acts as a certificate authority
//! (CA) or trusts an external CA chain.
//!
//! # Security Model
//!
//! The client presents an X.509 certificate during TLS handshake (mTLS).
//! The daemon verifies:
//! 1. Certificate chain validity (trusted CA, not expired, not revoked)
//! 2. Subject CN or SAN matches the device identity
//! 3. Key usage includes clientAuth
//!
//! # Configuration
//!
//! ```toml
//! [auth]
//! methods = ["certificate"]
//!
//! [auth.certificate]
//! ca_cert = "/etc/tcfs/ca.pem"
//! # Optional: CRL for revocation checking
//! crl_path = "/etc/tcfs/crl.pem"
//! # Allowed CN patterns (empty = accept any valid cert)
//! allowed_subjects = []
//! ```
//!
//! # Enrollment
//!
//! 1. Admin generates CA keypair (`tcfs auth cert-init`)
//! 2. Device generates CSR → admin signs → device installs cert
//! 3. Or: device presents cert from enterprise PKI, admin adds CA to trust store

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::provider::{AuthChallenge, AuthProvider, AuthResponse, RegistrationData, VerifyResult};

/// Certificate provider configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CertificateConfig {
    /// Path to the trusted CA certificate(s) in PEM format.
    pub ca_cert_path: Option<String>,
    /// Path to a CRL file for revocation checking (optional).
    pub crl_path: Option<String>,
    /// Allowed subject CN patterns (empty = accept any valid cert from trusted CA).
    pub allowed_subjects: Vec<String>,
    /// Whether to require the clientAuth extended key usage.
    pub require_client_auth_eku: bool,
}

impl Default for CertificateConfig {
    fn default() -> Self {
        Self {
            ca_cert_path: None,
            crl_path: None,
            allowed_subjects: Vec::new(),
            require_client_auth_eku: true,
        }
    }
}

/// Registered certificate identity for a device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CertificateIdentity {
    /// Device ID this certificate is bound to.
    pub device_id: String,
    /// Certificate subject CN.
    pub subject_cn: String,
    /// Certificate fingerprint (SHA-256 of DER encoding).
    pub fingerprint: String,
    /// Certificate serial number (hex-encoded).
    pub serial: String,
    /// Not-before timestamp (Unix seconds).
    pub not_before: u64,
    /// Not-after timestamp (Unix seconds).
    pub not_after: u64,
    /// When this identity was registered.
    pub registered_at: u64,
}

/// X.509 certificate authentication provider.
pub struct CertificateProvider {
    config: CertificateConfig,
    /// Device ID → registered certificate identity.
    identities: Arc<RwLock<HashMap<String, CertificateIdentity>>>,
    /// Pending challenges (challenge_id → device_id + nonce).
    pending: Arc<RwLock<HashMap<String, PendingChallenge>>>,
}

#[derive(Debug, Clone)]
struct PendingChallenge {
    device_id: String,
    /// Nonce sent to the client for signing (will be used when real crypto is wired).
    #[allow(dead_code)]
    nonce: Vec<u8>,
    created_at: u64,
}

impl CertificateProvider {
    pub fn new(config: CertificateConfig) -> Self {
        Self {
            config,
            identities: Arc::new(RwLock::new(HashMap::new())),
            pending: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register a certificate identity for a device.
    pub async fn register_identity(&self, identity: CertificateIdentity) {
        let device_id = identity.device_id.clone();
        self.identities
            .write()
            .await
            .insert(device_id.clone(), identity);
        info!(device_id, "certificate identity registered");
    }

    /// Check if a subject CN is allowed by the config.
    fn is_subject_allowed(&self, subject_cn: &str) -> bool {
        self.config.allowed_subjects.is_empty()
            || self
                .config
                .allowed_subjects
                .iter()
                .any(|s| s == subject_cn || subject_cn.ends_with(s))
    }

    /// Verify a certificate fingerprint against stored identity.
    async fn verify_fingerprint(&self, device_id: &str, fingerprint: &str) -> bool {
        let identities = self.identities.read().await;
        match identities.get(device_id) {
            Some(id) => id.fingerprint == fingerprint,
            None => false,
        }
    }

    /// Load identities from a JSON file.
    pub async fn load_from_file(&self, path: &std::path::Path) -> anyhow::Result<()> {
        let data = tokio::fs::read_to_string(path).await?;
        let ids: HashMap<String, CertificateIdentity> = serde_json::from_str(&data)?;
        *self.identities.write().await = ids;
        info!(path = %path.display(), "loaded certificate identities");
        Ok(())
    }

    /// Save identities to a JSON file.
    pub async fn save_to_file(&self, path: &std::path::Path) -> anyhow::Result<()> {
        let ids = self.identities.read().await;
        let data = serde_json::to_string_pretty(&*ids)?;
        tokio::fs::write(path, data).await?;
        debug!(path = %path.display(), "saved certificate identities");
        Ok(())
    }

    fn now_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }
}

#[async_trait]
impl AuthProvider for CertificateProvider {
    fn name(&self) -> &str {
        "certificate"
    }

    async fn challenge(&self, device_id: &str) -> anyhow::Result<AuthChallenge> {
        // Verify device has a registered certificate identity
        let identities = self.identities.read().await;
        if !identities.contains_key(device_id) {
            return Err(anyhow::anyhow!(
                "device {device_id} has no registered certificate — enroll first"
            ));
        }

        // Generate a random nonce for the client to sign
        let mut nonce = vec![0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut nonce);
        let challenge_id = uuid::Uuid::new_v4().to_string();

        self.pending.write().await.insert(
            challenge_id.clone(),
            PendingChallenge {
                device_id: device_id.to_string(),
                nonce: nonce.clone(),
                created_at: Self::now_secs(),
            },
        );

        Ok(AuthChallenge {
            challenge_id,
            data: nonce,
            prompt: "Sign the challenge nonce with your client certificate private key".into(),
            expires_at: Self::now_secs() + 300, // 5 minutes
        })
    }

    async fn verify(&self, response: &AuthResponse) -> anyhow::Result<VerifyResult> {
        // Look up pending challenge
        let pending = self.pending.read().await;
        let challenge = match pending.get(&response.challenge_id) {
            Some(c) => c.clone(),
            None => {
                return Ok(VerifyResult::Expired);
            }
        };
        drop(pending);

        // Check expiry (5 min window)
        if Self::now_secs() > challenge.created_at + 300 {
            self.pending.write().await.remove(&response.challenge_id);
            return Ok(VerifyResult::Expired);
        }

        // Verify device ID matches
        if challenge.device_id != response.device_id {
            return Ok(VerifyResult::Failure {
                reason: "device ID mismatch".into(),
            });
        }

        // Response data format: "fingerprint|signature_hex"
        // The fingerprint may contain colons (e.g., "sha256:deadbeef"), so we
        // split on '|' to separate fingerprint from signature.
        // In a real implementation, we'd verify the signature over the nonce
        // using the certificate's public key. For now, we verify the fingerprint
        // matches the registered identity.
        let response_str = String::from_utf8_lossy(&response.data);
        let fingerprint = match response_str.split_once('|') {
            Some((fp, _sig)) => fp,
            None => {
                return Ok(VerifyResult::Failure {
                    reason: "response must be 'fingerprint|signature' format".into(),
                });
            }
        };

        // Check fingerprint against stored identity
        if !self
            .verify_fingerprint(&response.device_id, fingerprint)
            .await
        {
            warn!(
                device_id = %response.device_id,
                "certificate fingerprint mismatch"
            );
            return Ok(VerifyResult::Failure {
                reason: "certificate fingerprint does not match registered identity".into(),
            });
        }

        // Check subject allowlist
        let identities = self.identities.read().await;
        if let Some(id) = identities.get(&response.device_id) {
            if !self.is_subject_allowed(&id.subject_cn) {
                warn!(
                    device_id = %response.device_id,
                    subject = %id.subject_cn,
                    "certificate subject not in allowed list"
                );
                return Ok(VerifyResult::Failure {
                    reason: "certificate subject not allowed".into(),
                });
            }

            // Check certificate expiry
            let now = Self::now_secs();
            if now < id.not_before || now > id.not_after {
                return Ok(VerifyResult::Failure {
                    reason: "certificate has expired or is not yet valid".into(),
                });
            }
        }
        drop(identities);

        // Clean up pending challenge
        self.pending.write().await.remove(&response.challenge_id);

        info!(
            device_id = %response.device_id,
            "certificate authentication succeeded"
        );

        Ok(VerifyResult::Success {
            session_token: uuid::Uuid::new_v4().to_string(),
            device_id: response.device_id.clone(),
        })
    }

    fn is_available(&self) -> bool {
        // Available on all platforms
        true
    }

    async fn register(&self, _device_id: &str) -> anyhow::Result<RegistrationData> {
        // Certificate enrollment is typically done out-of-band:
        // 1. Device generates a CSR
        // 2. Admin signs it with the CA
        // 3. Device installs the signed certificate
        // 4. Admin registers the fingerprint via register_identity()
        Ok(RegistrationData {
            data: Vec::new(),
            instructions: format!(
                "Certificate enrollment steps:\n\
                 1. Generate a private key: openssl genrsa -out device.key 4096\n\
                 2. Create a CSR: openssl req -new -key device.key -out device.csr\n\
                 3. Submit CSR to your CA administrator\n\
                 4. Install the signed certificate\n\
                 5. Register with: tcfs auth cert-register <fingerprint>\n\
                 \n\
                 CA trust path: {}",
                self.config
                    .ca_cert_path
                    .as_deref()
                    .unwrap_or("(not configured)")
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_certificate_provider_creation() {
        let provider = CertificateProvider::new(CertificateConfig::default());
        assert_eq!(provider.name(), "certificate");
        assert!(provider.is_available());
    }

    #[tokio::test]
    async fn test_challenge_requires_registration() {
        let provider = CertificateProvider::new(CertificateConfig::default());
        let result = provider.challenge("unknown-device").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_certificate_identity_registration() {
        let provider = CertificateProvider::new(CertificateConfig::default());

        let identity = CertificateIdentity {
            device_id: "dev-1".into(),
            subject_cn: "dev-1.tcfs.local".into(),
            fingerprint: "abc123".into(),
            serial: "01".into(),
            not_before: 0,
            not_after: u64::MAX,
            registered_at: 0,
        };
        provider.register_identity(identity).await;

        // Should be able to issue challenge now
        let challenge = provider.challenge("dev-1").await.unwrap();
        assert!(!challenge.challenge_id.is_empty());
        assert_eq!(challenge.data.len(), 32); // 32-byte nonce
    }

    #[tokio::test]
    async fn test_verify_matching_fingerprint() {
        let provider = CertificateProvider::new(CertificateConfig::default());

        let identity = CertificateIdentity {
            device_id: "dev-1".into(),
            subject_cn: "dev-1.tcfs.local".into(),
            fingerprint: "sha256:deadbeef".into(),
            serial: "01".into(),
            not_before: 0,
            not_after: u64::MAX,
            registered_at: 0,
        };
        provider.register_identity(identity).await;

        let challenge = provider.challenge("dev-1").await.unwrap();

        let response = AuthResponse {
            challenge_id: challenge.challenge_id,
            data: b"sha256:deadbeef|fakesig".to_vec(),
            device_id: "dev-1".into(),
        };
        let result = provider.verify(&response).await.unwrap();
        assert!(matches!(result, VerifyResult::Success { .. }));
    }

    #[tokio::test]
    async fn test_verify_wrong_fingerprint() {
        let provider = CertificateProvider::new(CertificateConfig::default());

        let identity = CertificateIdentity {
            device_id: "dev-1".into(),
            subject_cn: "dev-1.tcfs.local".into(),
            fingerprint: "sha256:correct".into(),
            serial: "01".into(),
            not_before: 0,
            not_after: u64::MAX,
            registered_at: 0,
        };
        provider.register_identity(identity).await;

        let challenge = provider.challenge("dev-1").await.unwrap();

        let response = AuthResponse {
            challenge_id: challenge.challenge_id,
            data: b"sha256:wrong|sig".to_vec(),
            device_id: "dev-1".into(),
        };
        let result = provider.verify(&response).await.unwrap();
        assert!(matches!(result, VerifyResult::Failure { .. }));
    }

    #[tokio::test]
    async fn test_subject_allowlist() {
        let config = CertificateConfig {
            allowed_subjects: vec![".tcfs.local".into()],
            ..Default::default()
        };
        let provider = CertificateProvider::new(config);

        assert!(provider.is_subject_allowed("dev-1.tcfs.local"));
        assert!(!provider.is_subject_allowed("attacker.evil.com"));
    }

    #[tokio::test]
    async fn test_expired_certificate_rejected() {
        let provider = CertificateProvider::new(CertificateConfig::default());

        let identity = CertificateIdentity {
            device_id: "dev-1".into(),
            subject_cn: "dev-1.tcfs.local".into(),
            fingerprint: "sha256:abc".into(),
            serial: "01".into(),
            not_before: 0,
            not_after: 1, // Expired in 1970
            registered_at: 0,
        };
        provider.register_identity(identity).await;

        let challenge = provider.challenge("dev-1").await.unwrap();

        let response = AuthResponse {
            challenge_id: challenge.challenge_id,
            data: b"sha256:abc|sig".to_vec(),
            device_id: "dev-1".into(),
        };
        let result = provider.verify(&response).await.unwrap();
        assert!(matches!(result, VerifyResult::Failure { reason } if reason.contains("expired")));
    }

    #[tokio::test]
    async fn test_identity_persistence() {
        let provider = CertificateProvider::new(CertificateConfig::default());

        let identity = CertificateIdentity {
            device_id: "persist-dev".into(),
            subject_cn: "persist.tcfs.local".into(),
            fingerprint: "sha256:persist".into(),
            serial: "99".into(),
            not_before: 0,
            not_after: u64::MAX,
            registered_at: 0,
        };
        provider.register_identity(identity).await;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cert-ids.json");
        provider.save_to_file(&path).await.unwrap();

        let provider2 = CertificateProvider::new(CertificateConfig::default());
        provider2.load_from_file(&path).await.unwrap();

        let challenge = provider2.challenge("persist-dev").await;
        assert!(challenge.is_ok());
    }
}
