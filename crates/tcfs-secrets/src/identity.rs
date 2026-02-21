//! Age identity discovery chain

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tcfs_core::config::SecretsConfig;

/// A loaded age identity (private key)
pub struct IdentityProvider {
    pub key_data: String,
    pub source: String,
}

/// Discover and load the age identity using the priority chain:
///   1. $CREDENTIALS_DIRECTORY/age-identity  (systemd credential injection)
///   2. $SOPS_AGE_KEY_FILE  (explicit path env var)
///   3. $SOPS_AGE_KEY  (literal key in env var, may be multi-line)
///   4. config.age_identity path (from tcfs.toml)
///   5. ~/.config/sops/age/keys.txt  (default XDG location)
pub async fn find_age_identity(config: &SecretsConfig) -> Result<IdentityProvider> {
    // 1. systemd credentials directory
    if let Ok(cred_dir) = std::env::var("CREDENTIALS_DIRECTORY") {
        let path = PathBuf::from(&cred_dir).join("age-identity");
        if path.exists() {
            let key_data = tokio::fs::read_to_string(&path)
                .await
                .with_context(|| format!("reading systemd credential: {}", path.display()))?;
            return Ok(IdentityProvider {
                key_data,
                source: format!("systemd:{}", path.display()),
            });
        }
    }

    // 2. SOPS_AGE_KEY_FILE env var
    if let Ok(key_file) = std::env::var("SOPS_AGE_KEY_FILE") {
        let path = PathBuf::from(&key_file);
        if path.exists() {
            let key_data = tokio::fs::read_to_string(&path)
                .await
                .with_context(|| format!("reading SOPS_AGE_KEY_FILE: {}", path.display()))?;
            return Ok(IdentityProvider {
                key_data,
                source: format!("SOPS_AGE_KEY_FILE:{}", path.display()),
            });
        }
    }

    // 3. SOPS_AGE_KEY env var (literal key content)
    if let Ok(key_content) = std::env::var("SOPS_AGE_KEY") {
        if !key_content.is_empty() {
            return Ok(IdentityProvider {
                key_data: key_content,
                source: "SOPS_AGE_KEY (env)".into(),
            });
        }
    }

    // 4. Explicit config path
    if let Some(identity_path) = &config.age_identity {
        let expanded = expand_tilde(identity_path);
        if expanded.exists() {
            let key_data = tokio::fs::read_to_string(&expanded)
                .await
                .with_context(|| format!("reading age identity: {}", expanded.display()))?;
            return Ok(IdentityProvider {
                key_data,
                source: format!("config:{}", expanded.display()),
            });
        }
    }

    // 5. Default XDG location
    let default_path = default_age_key_path();
    if default_path.exists() {
        let key_data = tokio::fs::read_to_string(&default_path)
            .await
            .with_context(|| format!("reading default age key: {}", default_path.display()))?;
        return Ok(IdentityProvider {
            key_data,
            source: format!("default:{}", default_path.display()),
        });
    }

    anyhow::bail!(
        "no age identity found. Tried: $CREDENTIALS_DIRECTORY/age-identity, \
         $SOPS_AGE_KEY_FILE, $SOPS_AGE_KEY, config path, and {}. \
         Run: task sops:init",
        default_path.display()
    )
}

fn default_age_key_path() -> PathBuf {
    let home = dirs_path();
    home.join(".config/sops/age/keys.txt")
}

fn dirs_path() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}

fn expand_tilde(path: &Path) -> PathBuf {
    if let Some(s) = path.to_str() {
        if let Some(rest) = s.strip_prefix("~/") {
            return dirs_path().join(rest);
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_tilde() {
        let expanded = expand_tilde(&PathBuf::from("~/.config/sops/age/keys.txt"));
        assert!(!expanded.to_str().unwrap().starts_with("~/"));
    }
}
