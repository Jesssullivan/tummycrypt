//! Credential rotation: atomic file replacement after re-encryption
//!
//! Rotation flow:
//!   1. Load current SOPS-encrypted credential file
//!   2. Decrypt with age identity to get plaintext
//!   3. Accept new credentials (provided by caller)
//!   4. Re-encrypt with SOPS (using same age recipient)
//!   5. Atomically replace the credential file
//!   6. Signal watchers to reload
//!
//! The caller is responsible for generating new credentials externally
//! (e.g., via `aws iam create-access-key` or SeaweedFS admin API) before
//! invoking the rotation.

use anyhow::{Context, Result};
use std::path::Path;
use tokio::sync::watch;

/// Result of a credential rotation operation.
#[derive(Debug)]
pub struct RotationResult {
    /// The credential file that was updated
    pub path: std::path::PathBuf,
    /// Timestamp of the rotation (Unix epoch seconds)
    pub rotated_at: u64,
    /// Whether a backup was created
    pub backup_created: bool,
}

/// Atomically replace a file with new content.
///
/// Writes to a temp file in the same directory, then renames to ensure
/// no partial reads by concurrent watchers.
pub async fn atomic_replace(path: &Path, new_content: &str) -> Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    let tmp_path = parent.join(format!(
        ".{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));

    tokio::fs::write(&tmp_path, new_content.as_bytes()).await?;
    tokio::fs::rename(&tmp_path, path).await?;

    tracing::info!("credential file rotated: {}", path.display());
    Ok(())
}

/// Rotate S3 credentials in a SOPS-encrypted file.
///
/// This function:
/// 1. Creates a timestamped backup of the current credential file
/// 2. Builds a new YAML document with the updated credentials
/// 3. Re-encrypts using `sops --encrypt` (shelling out to sops CLI)
/// 4. Atomically replaces the credential file
///
/// # Arguments
/// * `cred_file` — Path to the SOPS-encrypted YAML credential file
/// * `new_access_key` — The new S3 access key ID
/// * `new_secret_key` — The new S3 secret access key
/// * `reload_tx` — Optional channel sender to signal credential watchers to reload
pub async fn rotate_s3_credentials(
    cred_file: &Path,
    new_access_key: &str,
    new_secret_key: &str,
    reload_tx: Option<&watch::Sender<u64>>,
) -> Result<RotationResult> {
    if !cred_file.exists() {
        anyhow::bail!("credential file not found: {}", cred_file.display());
    }

    // Step 1: Create a timestamped backup
    let now = now_epoch();
    let backup_path = cred_file.with_extension(format!("yaml.bak.{now}"));
    let backup_created = match tokio::fs::copy(cred_file, &backup_path).await {
        Ok(_) => {
            tracing::info!("backup created: {}", backup_path.display());
            true
        }
        Err(e) => {
            tracing::warn!("backup creation failed (continuing): {e}");
            false
        }
    };

    // Step 2: Read the current file to preserve non-credential fields
    let current_content = tokio::fs::read_to_string(cred_file)
        .await
        .with_context(|| format!("reading credential file: {}", cred_file.display()))?;

    // Step 3: Build the plaintext YAML with updated credentials
    let plaintext = build_rotated_yaml(&current_content, new_access_key, new_secret_key)?;

    // Step 4: Re-encrypt with sops CLI
    let encrypted = sops_encrypt(&plaintext, cred_file)
        .await
        .context("re-encrypting credentials with sops")?;

    // Step 5: Atomic replace
    atomic_replace(cred_file, &encrypted)
        .await
        .context("atomic replacement of credential file")?;

    // Step 6: Signal watchers to reload
    if let Some(tx) = reload_tx {
        let _ = tx.send(now);
        tracing::info!("reload signal sent to credential watchers");
    }

    Ok(RotationResult {
        path: cred_file.to_path_buf(),
        rotated_at: now,
        backup_created,
    })
}

/// Build a new plaintext YAML with updated S3 credentials.
///
/// Preserves any existing fields (endpoint, region, etc.) from the
/// current content, only replacing access_key_id and secret_access_key.
fn build_rotated_yaml(
    current_content: &str,
    new_access_key: &str,
    new_secret_key: &str,
) -> Result<String> {
    // Parse the current YAML (may be SOPS-encrypted, but we only need the structure)
    let mut doc: serde_yml::Value =
        serde_yml::from_str(current_content).context("parsing current credential YAML")?;

    // Strip the sops metadata block (we'll re-encrypt from plaintext)
    if let serde_yml::Value::Mapping(ref mut map) = doc {
        map.remove(serde_yml::Value::String("sops".into()));
    }

    // Update the credential fields
    if let serde_yml::Value::Mapping(ref mut map) = doc {
        map.insert(
            serde_yml::Value::String("access_key_id".into()),
            serde_yml::Value::String(new_access_key.into()),
        );
        map.insert(
            serde_yml::Value::String("secret_access_key".into()),
            serde_yml::Value::String(new_secret_key.into()),
        );
    }

    serde_yml::to_string(&doc).context("serializing updated YAML")
}

/// Encrypt a plaintext YAML string using the `sops` CLI.
///
/// Uses `sops --encrypt --input-type yaml --output-type yaml /dev/stdin`
/// and pipes the plaintext through stdin. The sops CLI will use the
/// `.sops.yaml` configuration to determine the age recipient(s).
async fn sops_encrypt(plaintext: &str, cred_file: &Path) -> Result<String> {
    use tokio::io::AsyncWriteExt;
    use tokio::process::Command;

    // Determine the sops config file location (search upward from cred_file)
    let working_dir = cred_file.parent().unwrap_or(Path::new("."));

    let mut child = Command::new("sops")
        .args([
            "--encrypt",
            "--input-type",
            "yaml",
            "--output-type",
            "yaml",
            "/dev/stdin",
        ])
        .current_dir(working_dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("spawning sops")?;

    // Write plaintext to stdin asynchronously
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(plaintext.as_bytes())
            .await
            .context("writing to sops stdin")?;
        // Drop stdin to close it, signaling EOF to sops
    }

    let output = child.wait_with_output().await.context("waiting for sops")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("sops encrypt failed (exit {}): {}", output.status, stderr);
    }

    String::from_utf8(output.stdout).context("sops output is not valid UTF-8")
}

/// Clean up old backup files, keeping only the most recent `keep` backups.
pub async fn cleanup_backups(cred_file: &Path, keep: usize) -> Result<usize> {
    let parent = cred_file.parent().unwrap_or(Path::new("."));
    let stem = cred_file.file_name().unwrap_or_default().to_string_lossy();

    let mut backups: Vec<(String, std::path::PathBuf)> = Vec::new();

    let mut entries = tokio::fs::read_dir(parent).await?;
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with(&*stem) && name.contains(".bak.") {
            backups.push((name, entry.path()));
        }
    }

    if backups.len() <= keep {
        return Ok(0);
    }

    // Sort by name (which includes timestamp) — oldest first
    backups.sort_by(|a, b| a.0.cmp(&b.0));

    let to_remove = backups.len() - keep;
    let mut removed = 0;

    for (_, path) in backups.iter().take(to_remove) {
        match tokio::fs::remove_file(path).await {
            Ok(()) => {
                tracing::debug!("removed old backup: {}", path.display());
                removed += 1;
            }
            Err(e) => {
                tracing::warn!("failed to remove backup {}: {e}", path.display());
            }
        }
    }

    Ok(removed)
}

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_rotated_yaml_updates_credentials() {
        let input =
            "access_key_id: OLD_KEY\nsecret_access_key: OLD_SECRET\nendpoint: http://example.com\n";
        let result = build_rotated_yaml(input, "NEW_KEY", "NEW_SECRET").unwrap();

        assert!(result.contains("NEW_KEY"));
        assert!(result.contains("NEW_SECRET"));
        assert!(result.contains("example.com"));
        assert!(!result.contains("OLD_KEY"));
        assert!(!result.contains("OLD_SECRET"));
    }

    #[test]
    fn test_build_rotated_yaml_strips_sops_block() {
        let input = "access_key_id: test\nsecret_access_key: test\nsops:\n  version: 3.8.1\n";
        let result = build_rotated_yaml(input, "NEW_KEY", "NEW_SECRET").unwrap();

        assert!(!result.contains("sops"));
        assert!(!result.contains("3.8.1"));
        assert!(result.contains("NEW_KEY"));
    }

    #[tokio::test]
    async fn test_atomic_replace_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-creds.yaml");

        // Create initial file
        tokio::fs::write(&path, "initial content").await.unwrap();

        // Replace atomically
        atomic_replace(&path, "updated content").await.unwrap();

        let content = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(content, "updated content");

        // Verify no temp file remains
        let tmp_path = dir.path().join(".test-creds.yaml.tmp");
        assert!(!tmp_path.exists());
    }
}
