//! Key derivation: Argon2id passphrase → master key

use argon2::{Algorithm, Argon2, Params, Version};
use secrecy::{ExposeSecret, SecretString};
use zeroize::Zeroize;

use crate::KEY_SIZE;

/// A 256-bit master key derived from a passphrase via Argon2id.
///
/// Zeroized on drop to prevent secrets lingering in memory.
#[derive(Clone)]
pub struct MasterKey {
    bytes: [u8; KEY_SIZE],
}

impl MasterKey {
    pub fn from_bytes(bytes: [u8; KEY_SIZE]) -> Self {
        Self { bytes }
    }

    pub fn as_bytes(&self) -> &[u8; KEY_SIZE] {
        &self.bytes
    }
}

impl Drop for MasterKey {
    fn drop(&mut self) {
        self.bytes.zeroize();
    }
}

impl std::fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MasterKey")
            .field("bytes", &"[REDACTED]")
            .finish()
    }
}

/// Argon2id parameters for KDF
#[derive(Debug, Clone)]
pub struct KdfParams {
    /// Memory cost in KiB (default: 65536 = 64 MiB)
    pub mem_cost_kib: u32,
    /// Time cost / iterations (default: 3)
    pub time_cost: u32,
    /// Parallelism (default: 4)
    pub parallelism: u32,
}

impl Default for KdfParams {
    fn default() -> Self {
        Self {
            mem_cost_kib: 65536,
            time_cost: 3,
            parallelism: 4,
        }
    }
}

/// Derive a 256-bit master key from a passphrase and salt using Argon2id.
///
/// The salt should be 16 bytes, randomly generated and stored alongside the
/// encrypted data (it does not need to be secret).
pub fn derive_master_key(
    passphrase: &SecretString,
    salt: &[u8; 16],
    params: &KdfParams,
) -> anyhow::Result<MasterKey> {
    let argon2_params = Params::new(
        params.mem_cost_kib,
        params.time_cost,
        params.parallelism,
        Some(KEY_SIZE),
    )
    .map_err(|e| anyhow::anyhow!("invalid Argon2id params: {e}"))?;

    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, argon2_params);

    let mut key = [0u8; KEY_SIZE];
    argon2
        .hash_password_into(passphrase.expose_secret().as_bytes(), salt, &mut key)
        .map_err(|e| anyhow::anyhow!("Argon2id KDF failed: {e}"))?;

    Ok(MasterKey::from_bytes(key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::SecretString;

    #[test]
    fn test_kdf_deterministic() {
        let passphrase = SecretString::from("test-passphrase-123");
        let salt = [1u8; 16];
        // Use fast params for testing
        let params = KdfParams {
            mem_cost_kib: 1024,
            time_cost: 1,
            parallelism: 1,
        };

        let key1 = derive_master_key(&passphrase, &salt, &params).unwrap();
        let key2 = derive_master_key(&passphrase, &salt, &params).unwrap();

        assert_eq!(
            key1.as_bytes(),
            key2.as_bytes(),
            "KDF must be deterministic"
        );
    }

    #[test]
    fn test_kdf_different_passphrases() {
        let salt = [1u8; 16];
        let params = KdfParams {
            mem_cost_kib: 1024,
            time_cost: 1,
            parallelism: 1,
        };

        let key1 = derive_master_key(&SecretString::from("passphrase-a"), &salt, &params).unwrap();
        let key2 = derive_master_key(&SecretString::from("passphrase-b"), &salt, &params).unwrap();

        assert_ne!(
            key1.as_bytes(),
            key2.as_bytes(),
            "different passphrases must produce different keys"
        );
    }

    #[test]
    fn test_kdf_different_salts() {
        let passphrase = SecretString::from("same-passphrase");
        let params = KdfParams {
            mem_cost_kib: 1024,
            time_cost: 1,
            parallelism: 1,
        };

        let key1 = derive_master_key(&passphrase, &[1u8; 16], &params).unwrap();
        let key2 = derive_master_key(&passphrase, &[2u8; 16], &params).unwrap();

        assert_ne!(
            key1.as_bytes(),
            key2.as_bytes(),
            "different salts must produce different keys"
        );
    }
}
