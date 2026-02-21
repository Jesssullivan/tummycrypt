//! age decryption helpers (age 0.11 API)

use crate::identity::IdentityProvider;
use anyhow::{Context, Result};

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
    #[test]
    fn placeholder() {
        // Round-trip test added in Phase 5 (requires age key generation)
    }
}
