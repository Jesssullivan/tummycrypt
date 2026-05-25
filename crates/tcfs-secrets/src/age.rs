//! age decryption helpers (age 0.11 API)

use crate::identity::IdentityProvider;
use anyhow::{Context, Result};

/// Encrypt plaintext to an age X25519 public recipient and return ASCII armor.
pub fn encrypt_for_recipient(public_key: &str, plaintext: &[u8]) -> Result<String> {
    let recipient: age::x25519::Recipient = public_key
        .parse()
        .map_err(|e| anyhow::anyhow!("parsing age recipient public key {public_key}: {e}"))?;

    age::encrypt_and_armor(&recipient, plaintext).context("encrypting age bootstrap payload")
}

/// Decrypt age-encrypted data using an identity
///
/// `encrypted_data` should be the armored age ciphertext (PEM-like format)
/// Returns the plaintext bytes
pub fn decrypt_with_identity(
    identity: &IdentityProvider,
    encrypted_data: &[u8],
) -> Result<Vec<u8>> {
    use age::armor::ArmoredReader;
    use std::io::Read;

    // Parse identities from key file
    let identities =
        age::IdentityFile::from_buffer(std::io::BufReader::new(identity.key_data.as_bytes()))
            .context("parsing age identity file")?
            .into_identities()
            .context("extracting age identities")?;

    // Create decryptor — age 0.11: Decryptor is a plain struct, not an enum
    let armored = ArmoredReader::new(encrypted_data);
    let decryptor = age::Decryptor::new(armored).context("creating age decryptor")?;

    // Reject passphrase-protected keys
    if decryptor.is_scrypt() {
        anyhow::bail!("passphrase-protected age keys are not supported (SOPS uses recipient keys)");
    }

    // Decrypt
    let mut reader = decryptor
        .decrypt(identities.iter().map(|i| i.as_ref() as &dyn age::Identity))
        .context("decrypting with age identity")?;

    let mut plaintext = Vec::new();
    reader
        .read_to_end(&mut plaintext)
        .context("reading decrypted data")?;

    Ok(plaintext)
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    #[test]
    fn encrypt_for_recipient_roundtrips_with_identity() {
        let identity = age::x25519::Identity::generate();
        let public_key = identity.to_public().to_string();
        let ciphertext = encrypt_for_recipient(&public_key, b"bootstrap-secret").unwrap();
        assert!(ciphertext.contains("BEGIN AGE ENCRYPTED FILE"));

        let provider = IdentityProvider {
            key_data: identity.to_string().expose_secret().to_string(),
            source: "test".into(),
        };
        let plaintext = decrypt_with_identity(&provider, ciphertext.as_bytes()).unwrap();
        assert_eq!(plaintext, b"bootstrap-secret");
    }
}
