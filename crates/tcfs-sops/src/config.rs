use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Configuration for SOPS secret propagation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SopsSyncConfig {
    /// Local SOPS-managed directory to watch/sync
    pub sops_dir: PathBuf,

    /// Age identity file for re-encryption
    pub age_identity: PathBuf,

    /// S3 prefix for SOPS sync data (e.g. "sops-sync/{machine_id}/")
    pub s3_prefix: String,

    /// Machine identifier (defaults to hostname)
    pub machine_id: String,

    /// Local backup directory for pre-mutation snapshots
    pub backup_dir: PathBuf,

    /// When true, never delete remote entries (default: true)
    pub additive_only: bool,
}

impl Default for SopsSyncConfig {
    fn default() -> Self {
        let hostname = std::env::var("HOSTNAME")
            .or_else(|_| std::env::var("COMPUTERNAME"))
            .unwrap_or_else(|_| "unknown".to_string());

        Self {
            sops_dir: PathBuf::from("~/.config/sops/age"),
            age_identity: PathBuf::from("~/.config/sops/age/keys.txt"),
            s3_prefix: format!("sops-sync/{hostname}"),
            machine_id: hostname,
            backup_dir: PathBuf::from("~/.local/share/tcfs/sops-backups"),
            additive_only: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = SopsSyncConfig::default();
        assert!(config.additive_only);
        assert!(!config.machine_id.is_empty());
    }

    #[test]
    fn test_serde_roundtrip() {
        let config = SopsSyncConfig {
            sops_dir: PathBuf::from("/tmp/sops"),
            age_identity: PathBuf::from("/tmp/age.txt"),
            s3_prefix: "test-prefix".into(),
            machine_id: "test-host".into(),
            backup_dir: PathBuf::from("/tmp/backups"),
            additive_only: true,
        };

        let json = serde_json::to_string(&config).unwrap();
        let parsed: SopsSyncConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.machine_id, "test-host");
        assert_eq!(parsed.s3_prefix, "test-prefix");
        assert!(parsed.additive_only);
    }
}
