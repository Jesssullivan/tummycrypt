//! Sync Trash — staged deletes as a safety net.
//!
//! Instead of permanently deleting index entries on `unlink`, the VFS can
//! move them to a `.tcfs-trash/` prefix. This allows:
//! - Accidental delete recovery via `tcfs trash restore`
//! - Auto-purge after a configurable retention period
//! - List trashed items with deletion timestamp
//!
//! Trash entry key format: `{prefix}/.tcfs-trash/{timestamp}/{rel_path}`
//! The original index entry content is preserved verbatim.

use anyhow::{Context, Result};
use opendal::Operator;
use std::path::{Component, Path};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::debug;

/// Validate that a relative path does not contain traversal components.
///
/// Rejects paths with `..`, absolute prefixes, or other components that
/// could escape the intended prefix when used in S3 key construction.
fn validate_rel_path(rel_path: &str) -> Result<()> {
    if rel_path.is_empty() {
        anyhow::bail!("rel_path must not be empty");
    }
    for component in Path::new(rel_path).components() {
        match component {
            Component::ParentDir => {
                anyhow::bail!(
                    "path traversal rejected: rel_path contains '..' component: {rel_path}"
                );
            }
            Component::RootDir | Component::Prefix(_) => {
                anyhow::bail!("absolute path rejected: rel_path must be relative: {rel_path}");
            }
            _ => {}
        }
    }
    Ok(())
}

/// A single trashed item.
#[derive(Debug, Clone)]
pub struct TrashEntry {
    /// Original relative path (e.g., "docs/file.txt")
    pub original_path: String,
    /// Unix timestamp when the file was trashed.
    pub trashed_at: u64,
    /// Key in the S3 trash prefix (for restore/purge).
    pub trash_key: String,
    /// Original index entry content (for restore).
    pub index_content: String,
}

/// Move an index entry to the trash prefix instead of deleting it.
///
/// Returns the trash key where the entry was stored.
pub async fn trash_index_entry(
    op: &Operator,
    prefix: &str,
    index_key: &str,
    rel_path: &str,
) -> Result<String> {
    validate_rel_path(rel_path)?;

    // Read the original index entry
    let content = op
        .read(index_key)
        .await
        .with_context(|| format!("reading index for trash: {index_key}"))?;
    let content_bytes = content.to_bytes();

    // Generate trash key with timestamp
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let trash_key = if prefix.is_empty() {
        format!(".tcfs-trash/{now}/{rel_path}")
    } else {
        format!(
            "{}/.tcfs-trash/{now}/{rel_path}",
            prefix.trim_end_matches('/')
        )
    };

    // Write to trash location
    op.write(&trash_key, content_bytes.to_vec())
        .await
        .with_context(|| format!("writing trash entry: {trash_key}"))?;

    // Delete the original index entry
    op.delete(index_key)
        .await
        .with_context(|| format!("deleting original index: {index_key}"))?;

    debug!(original = %rel_path, trash = %trash_key, "moved to trash");
    Ok(trash_key)
}

/// List all trashed items under the given prefix.
pub async fn list_trash(op: &Operator, prefix: &str) -> Result<Vec<TrashEntry>> {
    let trash_prefix = if prefix.is_empty() {
        ".tcfs-trash/".to_string()
    } else {
        format!("{}/.tcfs-trash/", prefix.trim_end_matches('/'))
    };

    let mut entries = Vec::new();

    let lister = op
        .list_with(&trash_prefix)
        .recursive(true)
        .await
        .with_context(|| format!("listing trash: {trash_prefix}"))?;

    for entry in lister {
        let path = entry.path().to_string();
        // Skip directory markers
        if path.ends_with('/') {
            continue;
        }

        // Parse timestamp and original path from key
        // Format: {prefix}/.tcfs-trash/{timestamp}/{rel_path}
        let after_trash = path.strip_prefix(&trash_prefix).unwrap_or(&path);

        if let Some(slash_pos) = after_trash.find('/') {
            let timestamp_str = &after_trash[..slash_pos];
            let original_path = &after_trash[slash_pos + 1..];

            if let Ok(timestamp) = timestamp_str.parse::<u64>() {
                // Read content for metadata
                let content = match op.read(&path).await {
                    Ok(data) => String::from_utf8_lossy(&data.to_bytes()).to_string(),
                    Err(_) => String::new(),
                };

                entries.push(TrashEntry {
                    original_path: original_path.to_string(),
                    trashed_at: timestamp,
                    trash_key: path,
                    index_content: content,
                });
            }
        }
    }

    // Sort by trash time (newest first)
    entries.sort_by(|a, b| b.trashed_at.cmp(&a.trashed_at));
    Ok(entries)
}

/// Restore a trashed item back to its original index location.
pub async fn restore_trash_entry(op: &Operator, prefix: &str, entry: &TrashEntry) -> Result<()> {
    let clean = entry.original_path.trim_start_matches('/');
    let index_key = if prefix.is_empty() {
        format!("index/{clean}")
    } else {
        format!("{}/index/{clean}", prefix.trim_end_matches('/'))
    };

    // Write content back to original index location
    op.write(&index_key, entry.index_content.as_bytes().to_vec())
        .await
        .with_context(|| format!("restoring index entry: {index_key}"))?;

    // Remove trash entry
    op.delete(&entry.trash_key)
        .await
        .with_context(|| format!("removing trash entry: {}", entry.trash_key))?;

    debug!(
        path = %entry.original_path,
        "restored from trash"
    );
    Ok(())
}

/// Purge trashed items older than `max_age_secs`.
///
/// Returns the number of entries purged.
pub async fn purge_old_trash(op: &Operator, prefix: &str, max_age_secs: u64) -> Result<usize> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let entries = list_trash(op, prefix).await?;
    let mut purged = 0;

    for entry in &entries {
        let age = now.saturating_sub(entry.trashed_at);
        if age > max_age_secs {
            if let Err(e) = op.delete(&entry.trash_key).await {
                tracing::warn!(
                    path = %entry.original_path,
                    error = %e,
                    "failed to purge trash entry"
                );
            } else {
                purged += 1;
            }
        }
    }

    if purged > 0 {
        debug!(purged, "purged old trash entries");
    }
    Ok(purged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use opendal::services::Memory;

    fn memory_op() -> Operator {
        let builder = Memory::default();
        Operator::new(builder).unwrap().finish()
    }

    #[tokio::test]
    async fn trash_and_restore_roundtrip() {
        let op = memory_op();
        let prefix = "test";

        // Create an index entry
        let index_key = "test/index/doc.txt";
        op.write(
            index_key,
            b"manifest_hash=abc123\nsize=100\nchunks=1".to_vec(),
        )
        .await
        .unwrap();

        // Trash it
        let _trash_key = trash_index_entry(&op, prefix, index_key, "doc.txt")
            .await
            .unwrap();

        // Original should be gone
        assert!(op.read(index_key).await.is_err());

        // Should appear in trash list
        let entries = list_trash(&op, prefix).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].original_path, "doc.txt");
        assert!(entries[0].index_content.contains("manifest_hash=abc123"));

        // Restore it
        restore_trash_entry(&op, prefix, &entries[0]).await.unwrap();

        // Original should be back
        assert!(op.read(index_key).await.is_ok());

        // Trash should be empty
        let entries = list_trash(&op, prefix).await.unwrap();
        assert_eq!(entries.len(), 0);
    }

    #[tokio::test]
    async fn purge_removes_old_entries() {
        let op = memory_op();
        let prefix = "test";

        // Manually write a trash entry with timestamp 0 (very old)
        let old_key = "test/.tcfs-trash/0/old_file.txt";
        op.write(old_key, b"manifest_hash=old\nsize=50\nchunks=1".to_vec())
            .await
            .unwrap();

        // Purge anything older than 1 second
        let purged = purge_old_trash(&op, prefix, 1).await.unwrap();
        assert_eq!(purged, 1);

        // Should be gone
        assert!(op.read(old_key).await.is_err());
    }

    #[tokio::test]
    async fn empty_trash_list() {
        let op = memory_op();
        let entries = list_trash(&op, "prefix").await.unwrap();
        assert!(entries.is_empty());
    }

    // ── Path traversal tests ────────────────────────────────────────────

    #[test]
    fn validate_rel_path_normal() {
        assert!(validate_rel_path("doc.txt").is_ok());
        assert!(validate_rel_path("dir/file.txt").is_ok());
        assert!(validate_rel_path("a/b/c/deep.md").is_ok());
        assert!(validate_rel_path(".hidden").is_ok());
    }

    #[test]
    fn validate_rel_path_rejects_parent_dir() {
        assert!(validate_rel_path("../escape.txt").is_err());
        assert!(validate_rel_path("dir/../../etc/passwd").is_err());
        assert!(validate_rel_path("ok/../sneaky").is_err());
    }

    #[test]
    fn validate_rel_path_rejects_absolute() {
        assert!(validate_rel_path("/etc/passwd").is_err());
        assert!(validate_rel_path("/root/.ssh/id_rsa").is_err());
    }

    #[test]
    fn validate_rel_path_rejects_empty() {
        assert!(validate_rel_path("").is_err());
    }

    #[tokio::test]
    async fn trash_rejects_traversal() {
        let op = memory_op();
        let index_key = "test/index/doc.txt";
        op.write(index_key, b"content".to_vec()).await.unwrap();

        let result = trash_index_entry(&op, "test", index_key, "../escape").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("path traversal"));

        // Original should still exist (not deleted)
        assert!(op.read(index_key).await.is_ok());
    }
}
