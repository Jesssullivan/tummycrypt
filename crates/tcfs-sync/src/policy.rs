//! Per-folder sync policies — controls sync behavior, auto-download thresholds,
//! and auto-unsync exemptions on a per-directory basis.
//!
//! Policies are stored in a JSON file and queried by path with parent-chain
//! inheritance: a policy set on `/home/user/projects` applies to all files
//! under that directory unless overridden by a more specific policy.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Sync mode for a folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncMode {
    /// Sync on demand — user triggers push/pull explicitly.
    OnDemand,
    /// Always keep synced — auto-push changes, auto-pull remote updates.
    Always,
    /// Never sync — ignore all changes in this folder.
    Never,
}

impl Default for SyncMode {
    fn default() -> Self {
        SyncMode::OnDemand
    }
}

/// Sync behavior policy for a single folder.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FolderPolicy {
    /// Sync mode for this folder.
    pub sync_mode: SyncMode,
    /// Auto-download files smaller than this (bytes). None = use global default.
    pub download_threshold: Option<u64>,
    /// Exempt from auto-unsync (pinned — never automatically unsynced).
    pub auto_unsync_exempt: bool,
}

impl Default for FolderPolicy {
    fn default() -> Self {
        Self {
            sync_mode: SyncMode::OnDemand,
            download_threshold: None,
            auto_unsync_exempt: false,
        }
    }
}

/// Persistent store for per-folder sync policies.
///
/// Loaded from a JSON file, queryable by path with parent-chain inheritance.
/// Policies set on a directory apply to all descendants unless overridden.
pub struct PolicyStore {
    policies: HashMap<String, FolderPolicy>,
    store_path: PathBuf,
}

impl Default for PolicyStore {
    fn default() -> Self {
        Self {
            policies: HashMap::new(),
            store_path: PathBuf::new(),
        }
    }
}

impl PolicyStore {
    /// Load from JSON file, or create empty if file doesn't exist.
    pub fn open(path: &Path) -> Result<Self> {
        let policies = if path.exists() {
            let content = std::fs::read_to_string(path)
                .with_context(|| format!("reading policy store: {}", path.display()))?;
            serde_json::from_str(&content)
                .with_context(|| format!("parsing policy store: {}", path.display()))?
        } else {
            HashMap::new()
        };

        Ok(Self {
            policies,
            store_path: path.to_path_buf(),
        })
    }

    /// Get the effective policy for a path by walking up the parent chain.
    ///
    /// Returns the first matching policy found at or above the given path.
    /// Returns None if no policy covers this path.
    pub fn get(&self, path: &Path) -> Option<&FolderPolicy> {
        let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());

        let mut current = Some(canonical.as_path());
        while let Some(dir) = current {
            let key = dir.to_string_lossy();
            if let Some(policy) = self.policies.get(key.as_ref()) {
                return Some(policy);
            }
            current = dir.parent();
        }
        None
    }

    /// Set a policy for a folder path.
    pub fn set(&mut self, path: &Path, policy: FolderPolicy) {
        let key = std::fs::canonicalize(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .into_owned();
        self.policies.insert(key, policy);
    }

    /// Remove a policy for a folder path.
    pub fn remove(&mut self, path: &Path) -> bool {
        let key = std::fs::canonicalize(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .into_owned();
        self.policies.remove(&key).is_some()
    }

    /// List all policies.
    pub fn all(&self) -> &HashMap<String, FolderPolicy> {
        &self.policies
    }

    /// Check if a path is exempt from auto-unsync (walks parent chain).
    pub fn is_auto_unsync_exempt(&self, path: &Path) -> bool {
        self.get(path)
            .map(|p| p.auto_unsync_exempt)
            .unwrap_or(false)
    }

    /// Flush policies to disk using atomic write (temp file + rename).
    pub fn flush(&self) -> Result<()> {
        if let Some(parent) = self.store_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating policy dir: {}", parent.display()))?;
        }

        let json =
            serde_json::to_string_pretty(&self.policies).context("serializing policy store")?;

        let tmp_path = self.store_path.with_extension("tmp");
        std::fs::write(&tmp_path, &json)
            .with_context(|| format!("writing policy temp: {}", tmp_path.display()))?;
        std::fs::rename(&tmp_path, &self.store_path)
            .with_context(|| format!("renaming policy store: {}", self.store_path.display()))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sync_mode_serde() {
        let json = serde_json::to_string(&SyncMode::Always).unwrap();
        assert_eq!(json, "\"always\"");
        let parsed: SyncMode = serde_json::from_str("\"never\"").unwrap();
        assert_eq!(parsed, SyncMode::Never);
    }

    #[test]
    fn test_folder_policy_defaults() {
        let policy: FolderPolicy = serde_json::from_str("{}").unwrap();
        assert_eq!(policy.sync_mode, SyncMode::OnDemand);
        assert!(policy.download_threshold.is_none());
        assert!(!policy.auto_unsync_exempt);
    }

    #[test]
    fn test_policy_store_open_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("policies.json");
        let store = PolicyStore::open(&path).unwrap();
        assert!(store.all().is_empty());
    }

    #[test]
    fn test_policy_set_get() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("policies.json");
        let mut store = PolicyStore::open(&path).unwrap();

        let folder = dir.path().join("project");
        std::fs::create_dir_all(&folder).unwrap();

        store.set(
            &folder,
            FolderPolicy {
                sync_mode: SyncMode::Always,
                download_threshold: Some(10_000_000),
                auto_unsync_exempt: true,
            },
        );

        let policy = store.get(&folder).unwrap();
        assert_eq!(policy.sync_mode, SyncMode::Always);
        assert_eq!(policy.download_threshold, Some(10_000_000));
        assert!(policy.auto_unsync_exempt);
    }

    #[test]
    fn test_policy_parent_walk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("policies.json");
        let mut store = PolicyStore::open(&path).unwrap();

        // Set policy on parent
        let parent = dir.path().join("project");
        let child = parent.join("subdir");
        let file = child.join("file.txt");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::write(&file, b"data").unwrap();

        store.set(
            &parent,
            FolderPolicy {
                sync_mode: SyncMode::Never,
                auto_unsync_exempt: true,
                ..Default::default()
            },
        );

        // Child inherits parent policy
        let policy = store.get(&file).unwrap();
        assert_eq!(policy.sync_mode, SyncMode::Never);
        assert!(policy.auto_unsync_exempt);
    }

    #[test]
    fn test_policy_exempt_check() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("policies.json");
        let mut store = PolicyStore::open(&path).unwrap();

        let important = dir.path().join("important");
        let scratch = dir.path().join("scratch");
        std::fs::create_dir_all(&important).unwrap();
        std::fs::create_dir_all(&scratch).unwrap();

        store.set(
            &important,
            FolderPolicy {
                auto_unsync_exempt: true,
                ..Default::default()
            },
        );

        let file_in_important = important.join("data.bin");
        std::fs::write(&file_in_important, b"keep").unwrap();

        assert!(store.is_auto_unsync_exempt(&file_in_important));
        assert!(!store.is_auto_unsync_exempt(&scratch.join("temp.txt")));
    }

    #[test]
    fn test_policy_flush_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("policies.json");

        {
            let mut store = PolicyStore::open(&path).unwrap();
            store.set(
                Path::new("/test/folder"),
                FolderPolicy {
                    sync_mode: SyncMode::Always,
                    download_threshold: Some(5_000_000),
                    auto_unsync_exempt: true,
                },
            );
            store.flush().unwrap();
        }

        // Reload and verify
        let store2 = PolicyStore::open(&path).unwrap();
        let policy = store2.get(Path::new("/test/folder")).unwrap();
        assert_eq!(policy.sync_mode, SyncMode::Always);
        assert!(policy.auto_unsync_exempt);
    }

    #[test]
    fn test_policy_remove() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("policies.json");
        let mut store = PolicyStore::open(&path).unwrap();

        store.set(Path::new("/test"), FolderPolicy::default());
        assert_eq!(store.all().len(), 1);

        assert!(store.remove(Path::new("/test")));
        assert_eq!(store.all().len(), 0);
        assert!(!store.remove(Path::new("/nonexistent")));
    }
}
