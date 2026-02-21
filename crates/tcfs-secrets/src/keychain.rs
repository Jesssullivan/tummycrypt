//! Platform keychain integration for storing master keys and device identities.
//!
//! Uses the `keyring` crate for cross-platform access:
//! - macOS: Keychain Services
//! - Linux: GNOME Keyring / Secret Service (D-Bus)
//! - Windows: Credential Manager (DPAPI)
//!
//! Fallback to age-encrypted file if no platform keychain is available.

use anyhow::Result;
use secrecy::{ExposeSecret, SecretString};
use zeroize::Zeroize;

const SERVICE_NAME: &str = "tcfs";

/// Store a secret in the platform keychain.
pub fn store_secret(key_name: &str, secret: &SecretString) -> Result<()> {
    let entry = keyring::Entry::new(SERVICE_NAME, key_name)
        .map_err(|e| anyhow::anyhow!("keychain entry creation: {e}"))?;
    entry
        .set_password(secret.expose_secret())
        .map_err(|e| anyhow::anyhow!("keychain store for '{key_name}': {e}"))?;
    tracing::debug!(key = key_name, "stored secret in platform keychain");
    Ok(())
}

/// Retrieve a secret from the platform keychain.
pub fn get_secret(key_name: &str) -> Result<Option<SecretString>> {
    let entry = keyring::Entry::new(SERVICE_NAME, key_name)
        .map_err(|e| anyhow::anyhow!("keychain entry creation: {e}"))?;
    match entry.get_password() {
        Ok(mut password) => {
            let secret = SecretString::from(password.clone());
            password.zeroize();
            Ok(Some(secret))
        }
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(anyhow::anyhow!("keychain get for '{key_name}': {e}")),
    }
}

/// Delete a secret from the platform keychain.
pub fn delete_secret(key_name: &str) -> Result<()> {
    let entry = keyring::Entry::new(SERVICE_NAME, key_name)
        .map_err(|e| anyhow::anyhow!("keychain entry creation: {e}"))?;
    match entry.delete_credential() {
        Ok(()) => {
            tracing::debug!(key = key_name, "deleted secret from platform keychain");
            Ok(())
        }
        Err(keyring::Error::NoEntry) => Ok(()), // already deleted
        Err(e) => Err(anyhow::anyhow!("keychain delete for '{key_name}': {e}")),
    }
}

/// Check if the platform keychain is available.
pub fn is_available() -> bool {
    keyring::Entry::new(SERVICE_NAME, "__tcfs_probe__").is_ok()
}

/// Well-known keychain key names
pub mod keys {
    /// The wrapped master key (base64)
    pub const MASTER_KEY: &str = "master-key";
    /// The device identity private key (age secret key)
    pub const DEVICE_IDENTITY: &str = "device-identity";
    /// Session unlock token (ephemeral)
    pub const SESSION_TOKEN: &str = "session-token";
}
