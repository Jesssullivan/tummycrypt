//! Inode table: bidirectional mapping between NFS file IDs and VFS paths.
//!
//! NFS uses `fileid3` (u64) to identify files. The VirtualFilesystem trait
//! uses string paths. This module bridges the two.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

/// Root inode — always 1 (NFS convention).
pub const ROOT_FILEID: u64 = 1;

/// Bidirectional path <-> fileid mapping.
pub struct InodeTable {
    /// path -> fileid
    path_to_id: RwLock<HashMap<String, u64>>,
    /// fileid -> path
    id_to_path: RwLock<HashMap<u64, String>>,
    /// Next available fileid
    next_id: AtomicU64,
}

impl InodeTable {
    /// Create a new inode table with root pre-registered.
    pub fn new() -> Self {
        let mut p2i = HashMap::new();
        let mut i2p = HashMap::new();
        p2i.insert("/".to_string(), ROOT_FILEID);
        i2p.insert(ROOT_FILEID, "/".to_string());

        InodeTable {
            path_to_id: RwLock::new(p2i),
            id_to_path: RwLock::new(i2p),
            next_id: AtomicU64::new(ROOT_FILEID + 1),
        }
    }

    /// Get or create a fileid for the given path.
    pub fn get_or_insert(&self, path: &str) -> u64 {
        // Fast path: read lock
        {
            let map = self.path_to_id.read().unwrap();
            if let Some(&id) = map.get(path) {
                return id;
            }
        }
        // Slow path: write lock + insert
        let mut p2i = self.path_to_id.write().unwrap();
        // Double-check after acquiring write lock
        if let Some(&id) = p2i.get(path) {
            return id;
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        p2i.insert(path.to_string(), id);
        self.id_to_path.write().unwrap().insert(id, path.to_string());
        id
    }

    /// Look up the path for a fileid. Returns None if not found.
    pub fn get_path(&self, id: u64) -> Option<String> {
        self.id_to_path.read().unwrap().get(&id).cloned()
    }

    /// Look up the fileid for a path. Returns None if not registered.
    pub fn get_id(&self, path: &str) -> Option<u64> {
        self.path_to_id.read().unwrap().get(path).copied()
    }

    /// Build the full child path from parent ID + filename.
    pub fn child_path(&self, parent_id: u64, name: &str) -> Option<String> {
        let parent = self.get_path(parent_id)?;
        let path = if parent == "/" {
            format!("/{}", name)
        } else {
            format!("{}/{}", parent.trim_end_matches('/'), name)
        };
        Some(path)
    }
}

impl Default for InodeTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_is_preregistered() {
        let table = InodeTable::new();
        assert_eq!(table.get_id("/"), Some(ROOT_FILEID));
        assert_eq!(table.get_path(ROOT_FILEID), Some("/".to_string()));
    }

    #[test]
    fn get_or_insert_allocates() {
        let table = InodeTable::new();
        let id = table.get_or_insert("/src/main.rs.tc");
        assert!(id > ROOT_FILEID);
        assert_eq!(table.get_path(id), Some("/src/main.rs.tc".to_string()));
        // Same path returns same id
        assert_eq!(table.get_or_insert("/src/main.rs.tc"), id);
    }

    #[test]
    fn child_path_from_root() {
        let table = InodeTable::new();
        assert_eq!(
            table.child_path(ROOT_FILEID, "src"),
            Some("/src".to_string())
        );
    }

    #[test]
    fn child_path_from_subdir() {
        let table = InodeTable::new();
        let src_id = table.get_or_insert("/src");
        assert_eq!(
            table.child_path(src_id, "main.rs.tc"),
            Some("/src/main.rs.tc".to_string())
        );
    }
}
