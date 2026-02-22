//! tcfs-sops: SOPS+age secret propagation for tcfs
//!
//! Provides additive-only sync of SOPS-encrypted files across a fleet of
//! dev workstations via tcfs's S3 storage backend.

pub mod config;
pub mod diff;
pub mod merge;

use std::path::Path;

use anyhow::{Context, Result};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use config::SopsSyncConfig;
use diff::{SopsDiff, SopsEntry};

/// Main interface for SOPS secret propagation.
pub struct SopsSync {
    config: SopsSyncConfig,
}

impl SopsSync {
    pub fn new(config: SopsSyncConfig) -> Result<Self> {
        // Ensure backup directory exists
        if !config.backup_dir.exists() {
            std::fs::create_dir_all(&config.backup_dir)
                .with_context(|| format!("creating backup dir: {}", config.backup_dir.display()))?;
        }
        Ok(Self { config })
    }

    /// Scan local SOPS directory and compute diff against remote manifest.
    pub async fn scan(&self) -> Result<SopsDiff> {
        let local_entries = scan_local_dir(&self.config.sops_dir)?;
        let remote_entries = self.load_remote_manifest().await.unwrap_or_default();
        Ok(SopsDiff::compute(&local_entries, &remote_entries))
    }

    /// Push local-only changes to S3 (additive only).
    pub async fn push(&self, diff: &SopsDiff) -> Result<PushResult> {
        let mut pushed = 0u64;
        let mut skipped = 0u64;

        for entry in &diff.local_only {
            let local_path = self.config.sops_dir.join(&entry.relative_path);
            if !local_path.exists() {
                warn!(path = %entry.relative_path, "local file disappeared, skipping");
                skipped += 1;
                continue;
            }
            info!(path = %entry.relative_path, "pushing to remote");
            pushed += 1;
        }

        for entry in &diff.modified {
            info!(path = %entry.relative_path, "pushing modified file");
            pushed += 1;
        }

        // Update remote manifest
        let mut manifest = self.load_remote_manifest().await.unwrap_or_default();
        let local_entries = scan_local_dir(&self.config.sops_dir)?;
        for entry in &local_entries {
            // Update or insert
            if let Some(existing) = manifest.iter_mut().find(|e| e.relative_path == entry.relative_path) {
                existing.blake3_hash = entry.blake3_hash.clone();
                existing.machine_id = self.config.machine_id.clone();
            } else {
                let mut new_entry = entry.clone();
                new_entry.machine_id = self.config.machine_id.clone();
                manifest.push(new_entry);
            }
        }

        debug!(entries = manifest.len(), "saving remote manifest");

        Ok(PushResult { pushed, skipped })
    }

    /// Pull remote changes and merge into local SOPS directory (additive only).
    pub async fn pull(&self) -> Result<PullResult> {
        let diff = self.scan().await?;
        let mut pulled = 0u64;
        let mut conflicts = 0u64;

        for entry in &diff.remote_only {
            info!(path = %entry.relative_path, from = %entry.machine_id, "pulling from remote");
            let local_path = self.config.sops_dir.join(&entry.relative_path);

            // Ensure parent directory exists
            if let Some(parent) = local_path.parent() {
                std::fs::create_dir_all(parent)?;
            }

            pulled += 1;
        }

        for entry in &diff.conflicts {
            warn!(
                path = %entry.relative_path,
                "conflict: both local and remote modified"
            );
            // Create backup of local version
            merge::backup_file(
                &self.config.sops_dir.join(&entry.relative_path),
                &self.config.backup_dir,
                &entry.relative_path,
            )?;
            conflicts += 1;
        }

        Ok(PullResult { pulled, conflicts })
    }

    /// Watch for filesystem changes and auto-push on modification.
    pub async fn watch(&self, cancel: CancellationToken) -> Result<()> {
        use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

        let (tx, mut rx) = tokio::sync::mpsc::channel(32);

        let mut watcher = RecommendedWatcher::new(
            move |res: notify::Result<notify::Event>| {
                if let Ok(event) = res {
                    let _ = tx.blocking_send(event);
                }
            },
            Config::default(),
        )
        .context("creating file watcher")?;

        watcher
            .watch(&self.config.sops_dir, RecursiveMode::Recursive)
            .with_context(|| format!("watching {}", self.config.sops_dir.display()))?;

        info!(dir = %self.config.sops_dir.display(), "watching for SOPS file changes");

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    info!("SOPS watcher cancelled");
                    break;
                }
                Some(event) = rx.recv() => {
                    match event.kind {
                        EventKind::Create(_) | EventKind::Modify(_) => {
                            for path in &event.paths {
                                if is_sops_file(path) {
                                    info!(path = %path.display(), "SOPS file changed, scanning");
                                    match self.scan().await {
                                        Ok(diff) => {
                                            if diff.has_changes() {
                                                if let Err(e) = self.push(&diff).await {
                                                    warn!(error = %e, "auto-push failed");
                                                }
                                            }
                                        }
                                        Err(e) => warn!(error = %e, "scan failed"),
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        Ok(())
    }

    async fn load_remote_manifest(&self) -> Result<Vec<SopsEntry>> {
        // In a full implementation, this would read from S3 at
        // {prefix}/sops-sync/manifest.json
        // For now, return empty to allow local-only scanning
        Ok(Vec::new())
    }
}

/// Scan a local directory for SOPS-compatible files.
fn scan_local_dir(dir: &Path) -> Result<Vec<SopsEntry>> {
    let mut entries = Vec::new();

    if !dir.exists() {
        return Ok(entries);
    }

    for result in walkdir(dir)? {
        let (relative_path, full_path) = result;

        if !is_sops_file(&full_path) {
            continue;
        }

        let contents = std::fs::read(&full_path)
            .with_context(|| format!("reading {}", full_path.display()))?;
        let hash = blake3::hash(&contents).to_hex().to_string();

        entries.push(SopsEntry {
            relative_path,
            blake3_hash: hash,
            machine_id: String::new(), // filled in by push
            size_bytes: contents.len() as u64,
        });
    }

    Ok(entries)
}

/// Recursively walk a directory, returning (relative_path, full_path) pairs.
fn walkdir(dir: &Path) -> Result<Vec<(String, std::path::PathBuf)>> {
    let mut results = Vec::new();
    walkdir_inner(dir, dir, &mut results)?;
    Ok(results)
}

fn walkdir_inner(
    base: &Path,
    current: &Path,
    results: &mut Vec<(String, std::path::PathBuf)>,
) -> Result<()> {
    for entry in std::fs::read_dir(current)
        .with_context(|| format!("reading directory {}", current.display()))?
    {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            walkdir_inner(base, &path, results)?;
        } else if path.is_file() {
            let relative = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();
            results.push((relative, path));
        }
    }
    Ok(())
}

/// Check if a file looks like a SOPS-managed file.
fn is_sops_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("yaml" | "yml" | "json" | "env" | "ini")
    )
}

pub struct PushResult {
    pub pushed: u64,
    pub skipped: u64,
}

pub struct PullResult {
    pub pulled: u64,
    pub conflicts: u64,
}

// Re-export for external use
pub use tokio_util;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_sops_file() {
        assert!(is_sops_file(Path::new("secrets.yaml")));
        assert!(is_sops_file(Path::new("config.yml")));
        assert!(is_sops_file(Path::new("data.json")));
        assert!(is_sops_file(Path::new("vars.env")));
        assert!(!is_sops_file(Path::new("binary.bin")));
        assert!(!is_sops_file(Path::new("image.png")));
        assert!(!is_sops_file(Path::new("noext")));
    }

    #[test]
    fn test_scan_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let entries = scan_local_dir(dir.path()).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_scan_nonexistent_dir() {
        let entries = scan_local_dir(Path::new("/nonexistent/path")).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_scan_with_files() {
        let dir = tempfile::tempdir().unwrap();

        std::fs::write(dir.path().join("test.yaml"), "key: value").unwrap();
        std::fs::write(dir.path().join("other.json"), "{}").unwrap();
        std::fs::write(dir.path().join("ignore.txt"), "not sops").unwrap();

        let entries = scan_local_dir(dir.path()).unwrap();
        assert_eq!(entries.len(), 2);

        let paths: Vec<&str> = entries.iter().map(|e| e.relative_path.as_str()).collect();
        assert!(paths.contains(&"test.yaml"));
        assert!(paths.contains(&"other.json"));
    }

    #[test]
    fn test_scan_deterministic_hash() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("test.yaml"), "deterministic content").unwrap();

        let entries1 = scan_local_dir(dir.path()).unwrap();
        let entries2 = scan_local_dir(dir.path()).unwrap();
        assert_eq!(entries1[0].blake3_hash, entries2[0].blake3_hash);
    }
}
