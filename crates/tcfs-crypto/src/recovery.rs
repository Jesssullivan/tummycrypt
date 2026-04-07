//! BIP-39 mnemonic recovery key generation
//!
//! When a user initializes tcfs, a 24-word BIP-39 mnemonic is generated.
//! This mnemonic can recover the master key if all devices are lost.
//! The mnemonic is never stored digitally — the user writes it down.

use bip39::Mnemonic;
use rand::RngCore;
use secrecy::SecretString;

use crate::kdf::{derive_master_key, KdfParams, MasterKey};

/// Generate a new BIP-39 24-word mnemonic and derive a recovery key.
///
/// Returns the mnemonic (for display to user) and the derived master key.
/// The mnemonic should be displayed once and never stored digitally.
pub fn generate_mnemonic() -> anyhow::Result<(String, MasterKey)> {
    // 24 words = 256 bits of entropy
    let mut entropy = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut entropy);

    let mnemonic = Mnemonic::from_entropy(&entropy)
        .map_err(|e| anyhow::anyhow!("BIP-39 mnemonic generation failed: {e}"))?;

    let words = mnemonic.to_string();
    let master = mnemonic_to_master_key(&words)?;

    Ok((words, master))
}

/// Recover a master key from a BIP-39 24-word mnemonic.
///
/// Uses the mnemonic as a passphrase with a fixed, well-known salt.
/// The salt is fixed because the mnemonic itself provides sufficient entropy
/// (256 bits from 24 words).
pub fn mnemonic_to_master_key(words: &str) -> anyhow::Result<MasterKey> {
    // Validate the mnemonic
    let _mnemonic: Mnemonic = words
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid BIP-39 mnemonic: {e}"))?;

    // Fixed salt for recovery — the mnemonic provides the entropy
    let salt: [u8; 16] = *b"tcfs-recovery-v1";

    // Use lighter KDF params since the mnemonic has 256 bits of entropy
    let params = KdfParams {
        mem_cost_kib: 16384, // 16 MiB (lighter, since input has high entropy)
        time_cost: 2,
        parallelism: 1,
    };

    derive_master_key(&SecretString::from(words.to_string()), &salt, &params)
}

/// Derive master key from a passphrase using Argon2id.
///
/// If the passphrase looks like a BIP-39 mnemonic (>= 12 words), it is
/// validated and derived via the mnemonic path (fixed salt is acceptable
/// because 256-bit mnemonic entropy prevents brute-force).
///
/// For shorter passphrases, a per-vault random salt MUST be provided to
/// prevent identical passphrases from producing identical keys. Generate
/// the salt once with `generate_passphrase_salt()` and persist it alongside
/// the config.
pub fn derive_from_passphrase(passphrase: &str, salt: &[u8; 16]) -> anyhow::Result<MasterKey> {
    if passphrase.split_whitespace().count() >= 12 {
        mnemonic_to_master_key(passphrase)
    } else {
        let params = KdfParams::default();
        derive_master_key(&SecretString::from(passphrase.to_string()), salt, &params)
    }
}

/// Generate a random 16-byte salt for passphrase-based key derivation.
/// This salt should be persisted in the config and reused for the same vault.
pub fn generate_passphrase_salt() -> [u8; 16] {
    let mut salt = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut salt);
    salt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_mnemonic() {
        let (words, key) = generate_mnemonic().unwrap();

        let word_count = words.split_whitespace().count();
        assert_eq!(word_count, 24, "BIP-39 mnemonic must have 24 words");
        assert_ne!(key.as_bytes(), &[0u8; 32], "key must not be all zeros");
    }

    #[test]
    fn test_mnemonic_recovery_roundtrip() {
        let (words, original_key) = generate_mnemonic().unwrap();

        let recovered_key = mnemonic_to_master_key(&words).unwrap();
        assert_eq!(
            original_key.as_bytes(),
            recovered_key.as_bytes(),
            "recovered key must match original"
        );
    }

    #[test]
    fn test_invalid_mnemonic() {
        let result = mnemonic_to_master_key("not a valid mnemonic at all");
        assert!(result.is_err());
    }

    #[test]
    fn test_short_passphrase_uses_salt() {
        let salt_a: [u8; 16] = *b"salt-aaaaaaaaaa\0";
        let salt_b: [u8; 16] = *b"salt-bbbbbbbbbb\0";
        let key_a = derive_from_passphrase("same passphrase", &salt_a).unwrap();
        let key_b = derive_from_passphrase("same passphrase", &salt_b).unwrap();
        assert_ne!(
            key_a.as_bytes(),
            key_b.as_bytes(),
            "different salts must produce different keys"
        );
    }

    #[test]
    fn test_generate_passphrase_salt() {
        let s1 = generate_passphrase_salt();
        let s2 = generate_passphrase_salt();
        assert_ne!(s1, s2, "salts must be random");
    }

    #[test]
    fn test_different_mnemonics_different_keys() {
        let (_, key1) = generate_mnemonic().unwrap();
        let (_, key2) = generate_mnemonic().unwrap();

        assert_ne!(
            key1.as_bytes(),
            key2.as_bytes(),
            "different mnemonics must produce different keys"
        );
    }
}
