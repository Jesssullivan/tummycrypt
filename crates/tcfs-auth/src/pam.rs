//! PAM (Pluggable Authentication Modules) provider for Linux.
//!
//! Delegates authentication to the system PAM stack, allowing TCFS to
//! use the same credentials as SSH/sudo/login. Only available on Linux.
//!
//! # Security Model
//!
//! PAM authentication runs locally on the daemon's host. The client sends
//! a username + password (or empty password for passwordless PAM configs),
//! and the daemon verifies via `pam_authenticate()`.
//!
//! This is appropriate for:
//! - Multi-user Linux servers where each user has a system account
//! - Integration with LDAP/Kerberos via PAM modules
//! - Passwordless auth via `pam_ssh_agent` or `pam_fprintd`
//!
//! # Configuration
//!
//! Requires a PAM service file at `/etc/pam.d/tcfs`:
//! ```text
//! auth required pam_unix.so
//! account required pam_unix.so
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::provider::{AuthChallenge, AuthProvider, AuthResponse, RegistrationData, VerifyResult};

/// PAM provider configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PamConfig {
    /// PAM service name (default: "tcfs"). Maps to /etc/pam.d/<service>.
    pub service: String,
    /// Allowed usernames (empty = allow all system users).
    pub allowed_users: Vec<String>,
}

impl Default for PamConfig {
    fn default() -> Self {
        Self {
            service: "tcfs".into(),
            allowed_users: Vec::new(),
        }
    }
}

/// PAM authentication provider.
pub struct PamProvider {
    config: PamConfig,
    /// Tracks which devices have registered (username binding).
    device_users: Arc<RwLock<HashMap<String, String>>>,
}

impl PamProvider {
    pub fn new(config: PamConfig) -> Self {
        Self {
            config,
            device_users: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Bind a device to a system username.
    pub async fn bind_device(&self, device_id: &str, username: &str) {
        self.device_users
            .write()
            .await
            .insert(device_id.to_string(), username.to_string());
        info!(device_id, username, "PAM device bound to user");
    }

    /// Check if a username is allowed by the config.
    fn is_user_allowed(&self, username: &str) -> bool {
        self.config.allowed_users.is_empty()
            || self.config.allowed_users.iter().any(|u| u == username)
    }

    /// Authenticate via PAM.
    ///
    /// On Linux, this would call `pam_authenticate()` via the `pam` crate.
    /// Currently returns a stub result since the `pam` dependency is not
    /// yet wired (requires linking against libpam).
    fn pam_authenticate(&self, username: &str, password: &str) -> Result<bool, String> {
        // Validate inputs
        if username.is_empty() {
            return Err("username is required".into());
        }

        if !self.is_user_allowed(username) {
            warn!(username, "PAM auth denied: user not in allowed list");
            return Ok(false);
        }

        // TODO: Wire actual PAM authentication when `pam` crate is added.
        // For now, this is a compile-time stub that documents the interface.
        //
        // Real implementation:
        // ```
        // let mut auth = pam::Authenticator::with_password(&self.config.service)?;
        // auth.get_handler().set_credentials(username, password);
        // match auth.authenticate() {
        //     Ok(()) => Ok(true),
        //     Err(e) => {
        //         warn!(username, error = %e, "PAM authentication failed");
        //         Ok(false)
        //     }
        // }
        // ```
        let _ = password;
        debug!(
            username,
            service = %self.config.service,
            "PAM auth stub called (not yet linked to libpam)"
        );
        Err("PAM provider not yet linked to libpam — enable with 'pam' feature".into())
    }
}

#[async_trait]
impl AuthProvider for PamProvider {
    fn name(&self) -> &str {
        "pam"
    }

    async fn challenge(&self, device_id: &str) -> anyhow::Result<AuthChallenge> {
        let users = self.device_users.read().await;
        let username = users.get(device_id).cloned().unwrap_or_default();

        Ok(AuthChallenge {
            challenge_id: uuid::Uuid::new_v4().to_string(),
            data: Vec::new(),
            prompt: if username.is_empty() {
                "Enter your system username and password".into()
            } else {
                format!("Enter password for {username}")
            },
            expires_at: 0, // No expiry for PAM challenges
        })
    }

    async fn verify(&self, response: &AuthResponse) -> anyhow::Result<VerifyResult> {
        // Response data format: "username:password" (colon-separated)
        let credentials = String::from_utf8_lossy(&response.data);
        let (username, password) = match credentials.split_once(':') {
            Some((u, p)) => (u, p),
            None => {
                return Ok(VerifyResult::Failure {
                    reason: "credentials must be in 'username:password' format".into(),
                });
            }
        };

        match self.pam_authenticate(username, password) {
            Ok(true) => {
                // Bind device to this user for future challenges
                self.device_users
                    .write()
                    .await
                    .insert(response.device_id.clone(), username.to_string());

                info!(
                    device_id = %response.device_id,
                    username,
                    "PAM authentication succeeded"
                );
                Ok(VerifyResult::Success {
                    session_token: uuid::Uuid::new_v4().to_string(),
                    device_id: response.device_id.clone(),
                })
            }
            Ok(false) => Ok(VerifyResult::Failure {
                reason: "PAM authentication denied".into(),
            }),
            Err(e) => Ok(VerifyResult::Failure {
                reason: format!("PAM error: {e}"),
            }),
        }
    }

    fn is_available(&self) -> bool {
        // Only available on Linux
        cfg!(target_os = "linux")
    }

    async fn register(&self, _device_id: &str) -> anyhow::Result<RegistrationData> {
        Ok(RegistrationData {
            data: Vec::new(),
            instructions: format!(
                "PAM authentication uses your system login credentials.\n\
                 Service: {}\n\
                 To authenticate, run: tcfs auth verify <username>:<password>\n\
                 \n\
                 Ensure /etc/pam.d/{} exists with appropriate auth rules.",
                self.config.service, self.config.service
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_pam_provider_creation() {
        let provider = PamProvider::new(PamConfig::default());
        assert_eq!(provider.name(), "pam");
        // Only available on Linux
        assert_eq!(provider.is_available(), cfg!(target_os = "linux"));
    }

    #[tokio::test]
    async fn test_pam_challenge() {
        let provider = PamProvider::new(PamConfig::default());
        let challenge = provider.challenge("test-device").await.unwrap();
        assert!(!challenge.challenge_id.is_empty());
        assert!(challenge.prompt.contains("username"));
    }

    #[tokio::test]
    async fn test_pam_verify_stub() {
        let provider = PamProvider::new(PamConfig::default());
        let response = AuthResponse {
            challenge_id: "test".into(),
            data: b"testuser:testpass".to_vec(),
            device_id: "dev-1".into(),
        };
        let result = provider.verify(&response).await.unwrap();
        // Stub always returns failure (not linked to libpam)
        assert!(matches!(result, VerifyResult::Failure { .. }));
    }

    #[tokio::test]
    async fn test_pam_user_allowlist() {
        let config = PamConfig {
            service: "tcfs".into(),
            allowed_users: vec!["alice".into(), "bob".into()],
        };
        let provider = PamProvider::new(config);
        assert!(provider.is_user_allowed("alice"));
        assert!(provider.is_user_allowed("bob"));
        assert!(!provider.is_user_allowed("eve"));
    }

    #[tokio::test]
    async fn test_pam_device_binding() {
        let provider = PamProvider::new(PamConfig::default());
        provider.bind_device("dev-1", "jess").await;

        let challenge = provider.challenge("dev-1").await.unwrap();
        assert!(challenge.prompt.contains("jess"));
    }
}
