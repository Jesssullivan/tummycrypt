use std::path::Path;

use anyhow::{Context, Result};
use tracing::info;

/// Create a timestamped backup of a file before modifying it.
///
/// Backups are stored as: `{backup_dir}/{timestamp}/{relative_path}`
pub fn backup_file(
    source: &Path,
    backup_dir: &Path,
    relative_path: &str,
) -> Result<Option<std::path::PathBuf>> {
    if !source.exists() {
        return Ok(None);
    }

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let backup_path = backup_dir.join(format!("{timestamp}")).join(relative_path);

    if let Some(parent) = backup_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating backup dir: {}", parent.display()))?;
    }

    std::fs::copy(source, &backup_path).with_context(|| {
        format!(
            "backing up {} -> {}",
            source.display(),
            backup_path.display()
        )
    })?;

    info!(
        source = %source.display(),
        backup = %backup_path.display(),
        "created backup"
    );

    Ok(Some(backup_path))
}

/// Write content to a local file, creating parent dirs if needed.
/// Always backs up the existing file first.
pub fn write_with_backup(
    target: &Path,
    content: &[u8],
    backup_dir: &Path,
    relative_path: &str,
) -> Result<()> {
    // Backup existing file if present
    if target.exists() {
        backup_file(target, backup_dir, relative_path)?;
    }

    // Ensure parent directory exists
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }

    std::fs::write(target, content).with_context(|| format!("writing {}", target.display()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backup_nonexistent_file() {
        let dir = tempfile::tempdir().unwrap();
        let result =
            backup_file(Path::new("/nonexistent/file.yaml"), dir.path(), "file.yaml").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_backup_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("original.yaml");
        std::fs::write(&source, "secret: value").unwrap();

        let backup_dir = dir.path().join("backups");
        let result = backup_file(&source, &backup_dir, "original.yaml").unwrap();

        assert!(result.is_some());
        let backup_path = result.unwrap();
        assert!(backup_path.exists());
        assert_eq!(
            std::fs::read_to_string(&backup_path).unwrap(),
            "secret: value"
        );
    }

    #[test]
    fn test_write_with_backup() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("file.yaml");
        let backup_dir = dir.path().join("backups");

        // Write initial content
        std::fs::write(&target, "original").unwrap();

        // Write new content with backup
        write_with_backup(&target, b"updated", &backup_dir, "file.yaml").unwrap();

        // Verify new content
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "updated");

        // Verify backup was created
        let backups: Vec<_> = std::fs::read_dir(&backup_dir)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(backups.len(), 1);
    }

    #[test]
    fn test_write_new_file_no_backup() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("subdir").join("new.yaml");
        let backup_dir = dir.path().join("backups");

        write_with_backup(&target, b"new content", &backup_dir, "subdir/new.yaml").unwrap();

        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new content");
        // No backup dir should be created since there was no existing file
        assert!(!backup_dir.exists());
    }
}
