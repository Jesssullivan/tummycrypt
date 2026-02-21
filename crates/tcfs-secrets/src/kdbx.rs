//! KeePass KDBX4 credential store (keepass 0.8 API)
//!
//! Entry convention: `tummycrypt/tcfs/seaweedfs/{username}/access-key`
//! The group path maps to KeePass groups, the entry title is the last component.

use anyhow::{Context, Result};
use std::path::Path;

/// Credential resolved from a KDBX database
#[derive(Debug, Clone)]
pub struct KdbxCredential {
    pub title: String,
    pub username: Option<String>,
    pub password: String,
    pub url: Option<String>,
}

/// KeePass database accessor
pub struct KdbxStore {
    db_path: std::path::PathBuf,
}

impl KdbxStore {
    pub fn open(path: &Path) -> Self {
        KdbxStore {
            db_path: path.to_path_buf(),
        }
    }

    /// Resolve a credential by group-path query.
    ///
    /// Path format: `group/subgroup/entry-title`
    /// Example: `tummycrypt/tcfs/seaweedfs/admin/access-key`
    pub fn resolve(&self, query: &str, master_password: &str) -> Result<KdbxCredential> {
        let mut file = std::fs::File::open(&self.db_path)
            .with_context(|| format!("opening KDBX: {}", self.db_path.display()))?;

        let key = keepass::DatabaseKey::new().with_password(master_password);

        let db = keepass::Database::open(&mut file, key).context("opening KDBX database")?;

        self.search_group(&db.root, query)
            .ok_or_else(|| anyhow::anyhow!("no entry found for path: {query}"))
    }

    fn search_group(&self, group: &keepass::db::Group, query: &str) -> Option<KdbxCredential> {
        let parts: Vec<&str> = query.splitn(2, '/').collect();

        match parts.as_slice() {
            [entry_title] => {
                // Leaf: search entries in current group
                for entry in &group.entries {
                    if let Some(title) = entry.get_title() {
                        if title.eq_ignore_ascii_case(entry_title) {
                            return Some(KdbxCredential {
                                title: title.to_string(),
                                username: entry.get_username().map(|s| s.to_string()),
                                password: entry.get_password().unwrap_or("").to_string(),
                                url: entry.get_url().map(|s| s.to_string()),
                            });
                        }
                    }
                }
                None
            }
            [group_name, rest] => {
                // Recurse into matching subgroup
                for subgroup in &group.groups {
                    if subgroup.name.eq_ignore_ascii_case(group_name) {
                        return self.search_group(subgroup, rest);
                    }
                }
                None
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn placeholder() {
        // KDBX integration tests require a test database — added in Phase 5
    }
}
