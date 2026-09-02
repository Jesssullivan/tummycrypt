//! The scanned-row schema.
//!
//! One [`RowSchema`] is one filesystem seat observed by the agent's walker.
//! The identity fields `(dev, ino, size, mtime_ns, ctime_ns)` are exactly the
//! `StatIdentity` tuple the agent's `FreshnessCache` keys on, so a row is
//! self-sufficient for a resume decision without a second `stat`.

use serde::{Deserialize, Serialize};

/// The kind of seat a row describes.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileKind {
    /// A regular file.
    Regular,
    /// A directory.
    Directory,
    /// A symbolic link; the row's `link_target` carries the raw target bytes.
    Symlink,
    /// Anything else (fifo, socket, device). Bulkload refuses to copy these.
    Other,
}

/// One scanned filesystem row.
///
/// Paths travel as raw bytes, not `String`: darwin hands out non-UTF-8 names
/// and the Python engine's `PATH_NOT_PORTABLE` refusal is a policy decision
/// made *after* the bytes are captured, not a decoding accident.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowSchema {
    /// Path relative to the corpus root, as raw OS bytes.
    pub rel_path: Vec<u8>,
    /// What kind of seat this is.
    pub kind: FileKind,
    /// Containing device id.
    pub dev: u64,
    /// Inode number.
    pub ino: u64,
    /// Apparent size in bytes.
    pub size: u64,
    /// Modification time, nanoseconds since the unix epoch.
    pub mtime_ns: i128,
    /// Inode change time, nanoseconds since the unix epoch.
    pub ctime_ns: i128,
    /// Unix mode bits.
    pub mode: u32,
    /// Number of hard links to this inode.
    pub nlink: u64,
    /// Symlink target as raw OS bytes, when `kind` is [`FileKind::Symlink`].
    pub link_target: Option<Vec<u8>>,
    /// blake3 of the file contents, when the walker was asked to hash.
    pub blake3: Option<[u8; 32]>,
}

impl RowSchema {
    /// The `StatIdentity` tuple this row asserts.
    ///
    /// A resume that finds the same tuple may skip the seat; anything else is
    /// re-read. This is the input to the R25 `bytes_reread_on_resume` metric.
    #[must_use]
    pub const fn stat_identity(&self) -> (u64, u64, u64, i128, i128) {
        (self.dev, self.ino, self.size, self.mtime_ns, self.ctime_ns)
    }
}
