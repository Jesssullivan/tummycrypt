//! Authentication provider trait — pluggable auth backend abstraction.
//!
//! Each provider implements challenge-response authentication:
//! 1. `challenge()` — generate a challenge for the client
//! 2. `verify()` — validate the client's response
//!
//! Providers: TOTP, WebAuthn/FIDO2, Certificate, PAM.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A challenge issued to the client during authentication.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthChallenge {
    /// Unique challenge ID (for matching response to challenge).
    pub challenge_id: String,
    /// Provider-specific challenge data (TOTP: empty, WebAuthn: JSON options).
    pub data: Vec<u8>,
    /// Human-readable prompt (e.g., "Enter your 6-digit code").
    pub prompt: String,
    /// Challenge expiry (Unix timestamp, 0 = no expiry).
    pub expires_at: u64,
}

/// Client's response to an authentication challenge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthResponse {
    /// Must match the challenge_id from the issued challenge.
    pub challenge_id: String,
    /// Provider-specific response data (TOTP: 6-digit code, WebAuthn: assertion JSON).
    pub data: Vec<u8>,
    /// Device identifier for session binding.
    pub device_id: String,
}

/// Result of verification.
#[derive(Debug, Clone)]
pub enum VerifyResult {
    /// Authentication succeeded — session token returned.
    Success {
        session_token: String,
        device_id: String,
    },
    /// Authentication failed — reason provided.
    Failure { reason: String },
    /// Challenge expired or not found.
    Expired,
}

/// Pluggable authentication provider.
///
/// Implementations provide challenge-response auth for different mechanisms:
/// - TOTP (RFC 6238): time-based one-time passwords
/// - WebAuthn: browser/native passkey authentication
/// - Certificate: X.509 client certificate verification
/// - PAM: Linux Pluggable Authentication Modules
#[async_trait]
pub trait AuthProvider: Send + Sync {
    /// Provider name (e.g., "totp", "webauthn", "certificate").
    fn name(&self) -> &str;

    /// Generate a challenge for the client.
    ///
    /// For TOTP: returns an empty challenge (client generates code locally).
    /// For WebAuthn: returns PublicKeyCredentialRequestOptions as JSON.
    async fn challenge(&self, device_id: &str) -> anyhow::Result<AuthChallenge>;

    /// Verify the client's response to a challenge.
    ///
    /// Returns a session token on success, or a failure reason.
    async fn verify(&self, response: &AuthResponse) -> anyhow::Result<VerifyResult>;

    /// Check if this provider is available on the current platform.
    fn is_available(&self) -> bool {
        true
    }

    /// Register a new credential for a device (enrollment step).
    ///
    /// For TOTP: generates shared secret + QR code URI.
    /// For WebAuthn: starts credential registration ceremony.
    async fn register(&self, device_id: &str) -> anyhow::Result<RegistrationData> {
        let _ = device_id;
        Err(anyhow::anyhow!(
            "registration not supported by {} provider",
            self.name()
        ))
    }
}

/// Data returned during credential registration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistrationData {
    /// Provider-specific registration data.
    /// TOTP: JSON with `{ secret, qr_uri, qr_svg }`.
    /// WebAuthn: PublicKeyCredentialCreationOptions as JSON.
    pub data: Vec<u8>,
    /// Human-readable instructions for the user.
    pub instructions: String,
}
