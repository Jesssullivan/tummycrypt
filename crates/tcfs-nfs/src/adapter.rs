//! NFS protocol adapter: implements `nfsserve::vfs::NFSFileSystem` by
//! delegating to `tcfs_vfs::VirtualFilesystem`.
//!
//! This adapter handles the translation between:
//! - NFS fileid3 (u64) ↔ VFS path strings (via InodeTable)
//! - nfsserve types (fattr3, nfsstat3) ↔ tcfs-vfs types (VfsAttr, VfsFileType)

use std::ffi::OsStr;
use std::sync::Arc;
use std::time::Duration;

use nfsserve::nfs::{fattr3, fileid3, filename3, ftype3, nfspath3, nfsstat3, nfsstring, sattr3};
use nfsserve::vfs::{DirEntry, NFSFileSystem, ReadDirResult, VFSCapabilities};
use tracing::{debug, warn};

/// Timeout for VFS/S3 operations.  mount.nfs sends GETATTR/READDIR during
/// the mount handshake and hangs until the NFS server responds.  If the S3
/// backend is slow or unreachable the whole mount blocks indefinitely.
/// Returning NFS3ERR_IO on timeout lets the mount fail fast instead of hanging.
const VFS_TIMEOUT: Duration = Duration::from_secs(10);

use tcfs_vfs::types::{VfsAttr, VfsFileType};
use tcfs_vfs::VirtualFilesystem;

use crate::inode::{InodeTable, ROOT_FILEID};

/// NFS adapter wrapping a `VirtualFilesystem` implementation.
pub struct NfsAdapter<V: VirtualFilesystem> {
    vfs: Arc<V>,
    inodes: InodeTable,
}

impl<V: VirtualFilesystem> NfsAdapter<V> {
    /// Create a new NFS adapter for the given VFS.
    pub fn new(vfs: Arc<V>) -> Self {
        NfsAdapter {
            vfs,
            inodes: InodeTable::new(),
        }
    }

    /// Convert VfsAttr to NFS fattr3.
    fn to_fattr3(&self, id: fileid3, attr: &VfsAttr) -> fattr3 {
        let ftype = match attr.kind {
            VfsFileType::RegularFile => ftype3::NF3REG,
            VfsFileType::Directory => ftype3::NF3DIR,
        };

        fattr3 {
            ftype,
            mode: attr.perm as u32,
            nlink: attr.nlink,
            uid: attr.uid,
            gid: attr.gid,
            size: attr.size,
            used: attr.blocks * 512,
            rdev: nfsserve::nfs::specdata3 {
                specdata1: 0,
                specdata2: 0,
            },
            fsid: 0,
            fileid: id,
            atime: system_time_to_nfstime(&attr.atime),
            mtime: system_time_to_nfstime(&attr.mtime),
            ctime: system_time_to_nfstime(&attr.ctime),
        }
    }
}

fn system_time_to_nfstime(t: &std::time::SystemTime) -> nfsserve::nfs::nfstime3 {
    let d = t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    nfsserve::nfs::nfstime3 {
        seconds: d.as_secs() as u32,
        nseconds: d.subsec_nanos(),
    }
}

#[async_trait::async_trait]
impl<V: VirtualFilesystem + 'static> NFSFileSystem for NfsAdapter<V> {
    fn capabilities(&self) -> VFSCapabilities {
        VFSCapabilities::ReadOnly
    }

    fn root_dir(&self) -> fileid3 {
        ROOT_FILEID
    }

    async fn lookup(&self, dirid: fileid3, filename: &filename3) -> Result<fileid3, nfsstat3> {
        let name_str = std::str::from_utf8(filename).map_err(|_| nfsstat3::NFS3ERR_INVAL)?;

        // Handle . and ..
        if name_str == "." {
            return Ok(dirid);
        }
        if name_str == ".." {
            // For simplicity, look up parent from path
            if let Some(path) = self.inodes.get_path(dirid) {
                if path == "/" {
                    return Ok(ROOT_FILEID);
                }
                if let Some(parent) = path
                    .rsplit_once('/')
                    .map(|(p, _)| if p.is_empty() { "/" } else { p })
                {
                    return Ok(self.inodes.get_or_insert(parent));
                }
            }
            return Ok(ROOT_FILEID);
        }

        let child_path = self
            .inodes
            .child_path(dirid, name_str)
            .ok_or(nfsstat3::NFS3ERR_STALE)?;

        let parent_path = self.inodes.get_path(dirid).ok_or(nfsstat3::NFS3ERR_STALE)?;

        // Verify the entry exists via VFS (with timeout to avoid mount hangs)
        tokio::time::timeout(VFS_TIMEOUT, self.vfs.lookup(&parent_path, OsStr::new(name_str)))
            .await
            .map_err(|_| {
                warn!(path = %parent_path, name = %name_str, "NFS LOOKUP timed out");
                nfsstat3::NFS3ERR_IO
            })?
            .map_err(|_| nfsstat3::NFS3ERR_NOENT)?;

        let id = self.inodes.get_or_insert(&child_path);
        debug!(parent = %parent_path, name = %name_str, fileid = id, "NFS LOOKUP");
        Ok(id)
    }

    async fn getattr(&self, id: fileid3) -> Result<fattr3, nfsstat3> {
        let path = self.inodes.get_path(id).ok_or(nfsstat3::NFS3ERR_STALE)?;

        let attr = tokio::time::timeout(VFS_TIMEOUT, self.vfs.getattr(&path))
            .await
            .map_err(|_| {
                warn!(path = %path, "NFS GETATTR timed out");
                nfsstat3::NFS3ERR_IO
            })?
            .map_err(|_| nfsstat3::NFS3ERR_NOENT)?;

        Ok(self.to_fattr3(id, &attr))
    }

    async fn setattr(&self, _id: fileid3, _setattr: sattr3) -> Result<fattr3, nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS) // read-only
    }

    async fn read(
        &self,
        id: fileid3,
        offset: u64,
        count: u32,
    ) -> Result<(Vec<u8>, bool), nfsstat3> {
        let path = self.inodes.get_path(id).ok_or(nfsstat3::NFS3ERR_STALE)?;

        // Open + read pattern: open the file, read the requested range, release
        let (fh, data) = tokio::time::timeout(VFS_TIMEOUT, self.vfs.open(&path))
            .await
            .map_err(|_| {
                warn!(path = %path, "NFS READ open timed out");
                nfsstat3::NFS3ERR_IO
            })?
            .map_err(|e| {
                warn!(path = %path, error = %e, "NFS READ open failed");
                nfsstat3::NFS3ERR_IO
            })?;

        let start = offset as usize;
        let result = if start >= data.len() {
            (Vec::new(), true)
        } else {
            let end = (start + count as usize).min(data.len());
            let eof = end >= data.len();
            (data[start..end].to_vec(), eof)
        };

        // Release the file handle
        let _ = self.vfs.release(fh).await;

        Ok(result)
    }

    async fn write(&self, _id: fileid3, _offset: u64, _data: &[u8]) -> Result<fattr3, nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS) // read-only
    }

    async fn create(
        &self,
        _dirid: fileid3,
        _filename: &filename3,
        _attr: sattr3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn create_exclusive(
        &self,
        _dirid: fileid3,
        _filename: &filename3,
    ) -> Result<fileid3, nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn mkdir(
        &self,
        _dirid: fileid3,
        _dirname: &filename3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn remove(&self, _dirid: fileid3, _filename: &filename3) -> Result<(), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn rename(
        &self,
        _from_dirid: fileid3,
        _from_filename: &filename3,
        _to_dirid: fileid3,
        _to_filename: &filename3,
    ) -> Result<(), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn readdir(
        &self,
        dirid: fileid3,
        start_after: fileid3,
        max_entries: usize,
    ) -> Result<ReadDirResult, nfsstat3> {
        let path = self.inodes.get_path(dirid).ok_or(nfsstat3::NFS3ERR_STALE)?;

        let vfs_entries = tokio::time::timeout(VFS_TIMEOUT, self.vfs.readdirplus(&path))
            .await
            .map_err(|_| {
                warn!(path = %path, "NFS READDIR timed out");
                nfsstat3::NFS3ERR_IO
            })?
            .map_err(|e| {
                warn!(path = %path, error = %e, "NFS READDIR failed");
                nfsstat3::NFS3ERR_IO
            })?;

        // Assign inodes to all entries and convert
        let mut entries: Vec<DirEntry> = Vec::new();
        let mut past_start = start_after == 0;

        for vfs_entry in &vfs_entries {
            let child_path = if path == "/" {
                format!("/{}", vfs_entry.name)
            } else {
                format!("{}/{}", path.trim_end_matches('/'), vfs_entry.name)
            };
            let child_id = self.inodes.get_or_insert(&child_path);

            if !past_start {
                if child_id == start_after {
                    past_start = true;
                }
                continue;
            }

            if entries.len() >= max_entries {
                return Ok(ReadDirResult {
                    entries,
                    end: false,
                });
            }

            let attr = match &vfs_entry.attr {
                Some(a) => self.to_fattr3(child_id, a),
                None => {
                    // Fallback: getattr
                    match self.vfs.getattr(&child_path).await {
                        Ok(a) => self.to_fattr3(child_id, &a),
                        Err(_) => continue,
                    }
                }
            };

            entries.push(DirEntry {
                fileid: child_id,
                name: nfsstring(vfs_entry.name.as_bytes().to_vec()),
                attr,
            });
        }

        Ok(ReadDirResult { entries, end: true })
    }

    async fn symlink(
        &self,
        _dirid: fileid3,
        _linkname: &filename3,
        _symlink: &nfspath3,
        _attr: &sattr3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn readlink(&self, _id: fileid3) -> Result<nfspath3, nfsstat3> {
        Err(nfsstat3::NFS3ERR_INVAL) // no symlinks
    }
}
