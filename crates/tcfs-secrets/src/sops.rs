//! SOPS YAML decryption
//!
//! SOPS-encrypted YAML files have the structure:
//! ```yaml
//! plaintext_key: plain_value
//! secret_key: ENC[AES256_GCM,data:BASE64,iv:IV_BASE64,tag:TAG_BASE64,type:str]
//! sops:
//!   age:
//!     - recipient: age1...
//!       enc: |
//!         -----BEGIN AGE ENCRYPTED FILE-----
//!         ...
//!         -----END AGE ENCRYPTED FILE-----
//!   mac: ENC[AES256_GCM,data:...,iv:...,tag:...,type:str]
//!   version: 3.8.1
//! ```
//!
//! Decryption steps:
//!   1. Parse the `sops.age[0].enc` age-encrypted data key
//!   2. Decrypt with age identity → data key (32 bytes)
//!   3. For each ENC[...] value: AES-256-GCM decrypt with data key

use crate::identity::IdentityProvider;
use anyhow::{bail, Context, Result};
use std::path::Path;

/// Decrypted credentials extracted from a SOPS file
#[derive(Debug, Clone, Default)]
pub struct SopsCredentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub endpoint: Option<String>,
    pub region: Option<String>,
    pub jwt_signing_key: Option<String>,
    pub rclone_password1: Option<String>,
    pub rclone_password2: Option<String>,
    /// All other decrypted string fields
    pub extra: std::collections::HashMap<String, String>,
}

/// Parsed but not-yet-decrypted SOPS file
#[derive(Debug)]
pub struct SopsFile {
    /// Raw YAML parsed structure
    data: serde_yml::Value,
    /// The encrypted data key for age recipients
    age_enc: String,
}

impl SopsFile {
    pub fn parse(yaml_str: &str) -> Result<Self> {
        let data: serde_yml::Value = serde_yml::from_str(yaml_str).context("parsing SOPS YAML")?;

        let age_enc = extract_age_enc(&data)?;

        Ok(SopsFile { data, age_enc })
    }
}

/// Decrypt a SOPS-encrypted YAML file using the provided age identity
pub async fn decrypt_sops_file(
    path: &Path,
    identity: &IdentityProvider,
) -> Result<SopsCredentials> {
    let yaml_str = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("reading SOPS file: {}", path.display()))?;

    let sops_file = SopsFile::parse(&yaml_str)?;

    // Step 1: Decrypt the data key with age
    let data_key = crate::age::decrypt_with_identity(identity, sops_file.age_enc.as_bytes())
        .context("decrypting SOPS data key with age")?;

    if data_key.len() < 32 {
        bail!("data key is too short ({} bytes, need 32)", data_key.len());
    }
    let data_key: [u8; 32] = data_key[..32].try_into().unwrap();

    // Step 2: Walk the YAML and decrypt ENC[...] values
    let mut creds = SopsCredentials::default();
    decrypt_yaml_value(&sops_file.data, &data_key, &mut creds, "")?;

    Ok(creds)
}

/// Extract the age-encrypted data key from the sops block
fn extract_age_enc(data: &serde_yml::Value) -> Result<String> {
    let sops = data
        .get("sops")
        .ok_or_else(|| anyhow::anyhow!("no 'sops' block in file (is this a SOPS file?)"))?;

    let age_arr = sops
        .get("age")
        .and_then(|a| a.as_sequence())
        .ok_or_else(|| anyhow::anyhow!("sops.age is missing or not a list"))?;

    let first = age_arr
        .first()
        .ok_or_else(|| anyhow::anyhow!("sops.age is empty"))?;

    let enc = first
        .get("enc")
        .and_then(|e| e.as_str())
        .ok_or_else(|| anyhow::anyhow!("sops.age[0].enc is missing"))?;

    Ok(enc.to_string())
}

/// Walk the YAML value tree, decrypting ENC[...] strings and populating creds
fn decrypt_yaml_value(
    value: &serde_yml::Value,
    data_key: &[u8; 32],
    creds: &mut SopsCredentials,
    key_path: &str,
) -> Result<()> {
    match value {
        serde_yml::Value::Mapping(map) => {
            for (k, v) in map {
                let key = k.as_str().unwrap_or("");
                // Skip the sops metadata block
                if key == "sops" {
                    continue;
                }
                let path = if key_path.is_empty() {
                    key.to_string()
                } else {
                    format!("{key_path}.{key}")
                };
                decrypt_yaml_value(v, data_key, creds, &path)?;
            }
        }
        serde_yml::Value::String(s) => {
            let decrypted = if s.starts_with("ENC[AES256_GCM,") {
                decrypt_enc_value(s, data_key)
                    .with_context(|| format!("decrypting field '{key_path}'"))?
            } else {
                s.clone()
            };

            // Map known field names to SopsCredentials fields
            let leaf_key = key_path.split('.').next_back().unwrap_or(key_path);
            match leaf_key {
                "access_key_id" => creds.access_key_id = decrypted,
                "secret_access_key" => creds.secret_access_key = decrypted,
                "endpoint" => creds.endpoint = Some(decrypted),
                "region" => creds.region = Some(decrypted),
                "jwt_signing_key" => creds.jwt_signing_key = Some(decrypted),
                "rclone_password1" => creds.rclone_password1 = Some(decrypted),
                "rclone_password2" => creds.rclone_password2 = Some(decrypted),
                _ => {
                    creds.extra.insert(key_path.to_string(), decrypted);
                }
            }
        }
        _ => {} // numbers, booleans, null — not encrypted
    }
    Ok(())
}

/// Decrypt a single `ENC[AES256_GCM,data:...,iv:...,tag:...,type:str]` token
fn decrypt_enc_value(enc: &str, data_key: &[u8; 32]) -> Result<String> {
    use aes_gcm::{aead::Aead, Aes256Gcm, KeyInit, Nonce};
    use base64::{engine::general_purpose::STANDARD as B64, Engine};

    // Parse ENC[AES256_GCM,data:B64,iv:B64,tag:B64,type:TYPE]
    let inner = enc
        .strip_prefix("ENC[AES256_GCM,")
        .and_then(|s| s.strip_suffix(']'))
        .ok_or_else(|| anyhow::anyhow!("malformed ENC value: {enc}"))?;

    let mut data_b64 = "";
    let mut iv_b64 = "";
    let mut tag_b64 = "";
    let mut _enc_type = "";

    for part in inner.split(',') {
        if let Some(v) = part.strip_prefix("data:") {
            data_b64 = v;
        } else if let Some(v) = part.strip_prefix("iv:") {
            iv_b64 = v;
        } else if let Some(v) = part.strip_prefix("tag:") {
            tag_b64 = v;
        } else if let Some(v) = part.strip_prefix("type:") {
            _enc_type = v;
        }
    }

    if data_b64.is_empty() || iv_b64.is_empty() || tag_b64.is_empty() {
        bail!("ENC value missing required fields: data={data_b64:?} iv={iv_b64:?} tag={tag_b64:?}");
    }

    let ciphertext = B64.decode(data_b64).context("base64 decode data")?;
    let iv_bytes = B64.decode(iv_b64).context("base64 decode iv")?;
    let tag_bytes = B64.decode(tag_b64).context("base64 decode tag")?;

    if iv_bytes.len() != 12 {
        bail!("IV must be 12 bytes, got {}", iv_bytes.len());
    }
    if tag_bytes.len() != 16 {
        bail!("tag must be 16 bytes, got {}", tag_bytes.len());
    }

    // SOPS AES-GCM: ciphertext || tag
    let mut ct_with_tag = ciphertext;
    ct_with_tag.extend_from_slice(&tag_bytes);

    let cipher = Aes256Gcm::new_from_slice(data_key).context("creating AES-256-GCM cipher")?;
    let nonce = Nonce::from_slice(&iv_bytes);

    let plaintext = cipher
        .decrypt(nonce, ct_with_tag.as_ref())
        .map_err(|_| anyhow::anyhow!("AES-256-GCM decryption failed (wrong key?)"))?;

    String::from_utf8(plaintext).context("decrypted value is not valid UTF-8")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_non_sops_yaml_fails_gracefully() {
        let result = SopsFile::parse("key: value\nother: 123\n");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("sops"));
    }
}
