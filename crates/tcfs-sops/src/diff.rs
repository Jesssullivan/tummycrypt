use serde::{Deserialize, Serialize};

/// A tracked SOPS file entry with its content hash.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SopsEntry {
    /// Path relative to the SOPS directory root
    pub relative_path: String,

    /// BLAKE3 hash of the file contents
    pub blake3_hash: String,

    /// Machine that last modified this entry
    pub machine_id: String,

    /// File size in bytes
    pub size_bytes: u64,
}

/// Result of comparing local vs remote SOPS entries.
#[derive(Debug, Default)]
pub struct SopsDiff {
    /// Files present locally but not in remote manifest
    pub local_only: Vec<SopsEntry>,

    /// Files present in remote manifest but not locally
    pub remote_only: Vec<SopsEntry>,

    /// Files present in both with matching hashes
    pub unchanged: Vec<SopsEntry>,

    /// Files present in both with different hashes
    pub modified: Vec<SopsEntry>,

    /// Files modified on both sides (local and remote differ, but neither matches last known)
    pub conflicts: Vec<SopsEntry>,
}

impl SopsDiff {
    /// Compute the diff between local and remote entry lists.
    pub fn compute(local: &[SopsEntry], remote: &[SopsEntry]) -> Self {
        let mut diff = SopsDiff::default();

        for local_entry in local {
            match remote.iter().find(|r| r.relative_path == local_entry.relative_path) {
                Some(remote_entry) => {
                    if local_entry.blake3_hash == remote_entry.blake3_hash {
                        diff.unchanged.push(local_entry.clone());
                    } else {
                        diff.modified.push(local_entry.clone());
                    }
                }
                None => {
                    diff.local_only.push(local_entry.clone());
                }
            }
        }

        for remote_entry in remote {
            if !local.iter().any(|l| l.relative_path == remote_entry.relative_path) {
                diff.remote_only.push(remote_entry.clone());
            }
        }

        diff
    }

    /// Returns true if there are any changes to sync.
    pub fn has_changes(&self) -> bool {
        !self.local_only.is_empty()
            || !self.remote_only.is_empty()
            || !self.modified.is_empty()
            || !self.conflicts.is_empty()
    }

    /// Summary of the diff for display.
    pub fn summary(&self) -> String {
        format!(
            "local_only={}, remote_only={}, modified={}, unchanged={}, conflicts={}",
            self.local_only.len(),
            self.remote_only.len(),
            self.modified.len(),
            self.unchanged.len(),
            self.conflicts.len(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, hash: &str) -> SopsEntry {
        SopsEntry {
            relative_path: path.to_string(),
            blake3_hash: hash.to_string(),
            machine_id: "test".to_string(),
            size_bytes: 100,
        }
    }

    #[test]
    fn test_empty_diff() {
        let diff = SopsDiff::compute(&[], &[]);
        assert!(!diff.has_changes());
        assert!(diff.local_only.is_empty());
        assert!(diff.remote_only.is_empty());
    }

    #[test]
    fn test_local_only() {
        let local = vec![entry("a.yaml", "hash1")];
        let diff = SopsDiff::compute(&local, &[]);
        assert!(diff.has_changes());
        assert_eq!(diff.local_only.len(), 1);
        assert_eq!(diff.local_only[0].relative_path, "a.yaml");
    }

    #[test]
    fn test_remote_only() {
        let remote = vec![entry("b.yaml", "hash2")];
        let diff = SopsDiff::compute(&[], &remote);
        assert!(diff.has_changes());
        assert_eq!(diff.remote_only.len(), 1);
    }

    #[test]
    fn test_unchanged() {
        let local = vec![entry("a.yaml", "same")];
        let remote = vec![entry("a.yaml", "same")];
        let diff = SopsDiff::compute(&local, &remote);
        assert!(!diff.has_changes());
        assert_eq!(diff.unchanged.len(), 1);
    }

    #[test]
    fn test_modified() {
        let local = vec![entry("a.yaml", "new_hash")];
        let remote = vec![entry("a.yaml", "old_hash")];
        let diff = SopsDiff::compute(&local, &remote);
        assert!(diff.has_changes());
        assert_eq!(diff.modified.len(), 1);
    }

    #[test]
    fn test_mixed() {
        let local = vec![
            entry("shared.yaml", "same"),
            entry("local.yaml", "loc"),
            entry("changed.yaml", "new"),
        ];
        let remote = vec![
            entry("shared.yaml", "same"),
            entry("remote.yaml", "rem"),
            entry("changed.yaml", "old"),
        ];
        let diff = SopsDiff::compute(&local, &remote);

        assert_eq!(diff.unchanged.len(), 1);
        assert_eq!(diff.local_only.len(), 1);
        assert_eq!(diff.remote_only.len(), 1);
        assert_eq!(diff.modified.len(), 1);
    }

    #[test]
    fn test_summary() {
        let diff = SopsDiff::default();
        let summary = diff.summary();
        assert!(summary.contains("local_only=0"));
        assert!(summary.contains("unchanged=0"));
    }
}
