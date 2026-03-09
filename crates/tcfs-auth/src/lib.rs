//! tcfs-auth: Authentication and authorization providers for TummyCrypt.
//!
//! Provides:
//! - `AuthProvider` trait: pluggable authentication backend abstraction
//! - `Session`: typed session with device identity, permissions, and expiry
//! - `Enrollment`: device enrollment protocol (QR code, invite link)
//! - `totp`: TOTP (RFC 6238) provider for 2FA
//! - `webauthn`: WebAuthn/FIDO2/passkey provider (future)
//!
//! # Architecture
//!
//! ```text
//! CLI / iOS App
//!   │
//!   ├─ AuthProvider::challenge() → Challenge
//!   ├─ AuthProvider::verify(response) → Session
//!   │
//!   └─ Session { device_id, permissions, expires_at }
//!       │
//!       └─ Daemon checks session on every RPC
//! ```

pub mod enrollment;
pub mod provider;
pub mod session;

#[cfg(feature = "totp")]
pub mod totp;

#[cfg(feature = "webauthn")]
pub mod webauthn;

// Re-exports
pub use enrollment::{EnrollmentInvite, EnrollmentRequest, EnrollmentResult};
pub use provider::{AuthChallenge, AuthProvider, AuthResponse, VerifyResult};
pub use session::{DevicePermissions, Session, SessionStore};
