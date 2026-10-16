//! Single-read ingest discipline for the walker and the upload path.
//!
//! Ported from bulkload's `capture_file` / `StatIdentity` discipline
//! (bulkload `crates/bulkload-agent/src/transfer.rs` and `walk.rs`, read at
//! `99a026ae`), adapted to tcfs's manifest format:
//!
//! - **Stat identity around one read.** A capture opens the file, takes the
//!   [`StatIdentity`] of that descriptor, reads it once, and takes the identity
//!   again. If the identity moved, the byte count disagrees with the size, or
//!   the path now names a different inode, the capture is torn: it is retried
//!   (up to [`CAPTURE_ATTEMPTS`]) and then refused with [`SourceChanged`].
//! - **Later checks are stat checks.** Once a capture is settled, every later
//!   "did the source change?" check (before a skip, before a manifest
//!   publish) compares the path's stat identity with the captured one. It does
//!   not read the file again.
//! - **Racy captures keep the content check.** A file whose mtime or ctime is
//!   within [`RACY_GRANULARITY_NS`] of the capture start can be rewritten at the
//!   same size within one timestamp tick without moving its identity — exactly
//!   Git's "racy" index entries. For those, and on platforms without POSIX
//!   inode metadata, the later checks also re-hash the content, as tcfs always
//!   did.
//!
//! Read budget per regular file (settled files):
//!
//! | Path | Before | After |
//! | --- | --- | --- |
//! | In-memory (< `STREAMING_THRESHOLD`) | 3 reads | 1 read |
//! | Streaming (>= `STREAMING_THRESHOLD`) | 4 reads | 2 reads |
//!
//! The streaming path still reads twice: the file hash is the encryption
//! `file_id` and the conflict check needs it before any chunk is uploaded, and
//! a large file cannot be held in memory between the two. The second pass
//! re-verifies every chunk hash against the first, so it is also the content
//! check. Removing that pass would change the key scheme, which is a separate
//! security decision (see `docs/ops/tcfs-bulkload-boundary.md`).
//!
//! The walker half of the discipline is [`CollectRefusal`]: an entry the
//! walker cannot carry (a name that is not UTF-8, a directory it cannot list,
//! an entry whose type it cannot read) is recorded as a typed refusal and the
//! walk continues, instead of being dropped silently or aborting the root.

use std::fs::File;
use std::path::{Path, PathBuf};

use crate::freshness::{stat_identity, StatIdentity};

/// A capture whose source has an mtime or ctime within this window of the
/// capture start (or later than the capture end) is racy. Two seconds covers
/// the coarsest timestamp granularity tcfs meets in practice (HFS+, FAT, SMB)
/// and matches bulkload's `RACY_GRANULARITY_NS`.
pub const RACY_GRANULARITY_NS: i128 = 2_000_000_000;

/// Captures attempted before a source that keeps changing is refused.
pub const CAPTURE_ATTEMPTS: u32 = 3;

/// The source file changed while tcfs was capturing or uploading it.
///
/// Returned (inside `anyhow::Error`) instead of uploading bytes that no longer
/// match the file. Callers can `downcast_ref::<SourceChanged>()` to tell this
/// apart from an I/O or storage failure: it is retryable on the next cycle.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("file changed during {stage}: {detail} for {}", path.display())]
pub struct SourceChanged {
    pub path: PathBuf,
    pub stage: String,
    pub detail: String,
}

impl SourceChanged {
    pub fn new(path: &Path, stage: &str, detail: impl Into<String>) -> Self {
        Self {
            path: path.to_path_buf(),
            stage: stage.to_string(),
            detail: detail.into(),
        }
    }
}

/// Why the walker refused an entry. Refusals are values: the walk continues.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CollectRefusalReason {
    /// The entry's name is not valid UTF-8, so it has no remote index key.
    /// A directory refused this way is refused with its whole subtree.
    NonUtf8Name,
    /// The directory could not be listed (for example `EACCES`), or listing
    /// failed part way. Its whole subtree is refused; nothing under it is
    /// carried, and nothing under it may be inferred deleted.
    UnreadableDirectory {
        kind: std::io::ErrorKind,
        message: String,
    },
    /// The entry's file type could not be read.
    UnreadableEntry {
        kind: std::io::ErrorKind,
        message: String,
    },
}

impl std::fmt::Display for CollectRefusalReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NonUtf8Name => f.write_str("name is not valid UTF-8"),
            Self::UnreadableDirectory { kind, message } => {
                write!(f, "directory unreadable ({kind:?}): {message}")
            }
            Self::UnreadableEntry { kind, message } => {
                write!(f, "entry type unreadable ({kind:?}): {message}")
            }
        }
    }
}

/// One entry the walker refused. `path` keeps the name's exact bytes; it is
/// never converted lossily.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectRefusal {
    pub path: PathBuf,
    pub reason: CollectRefusalReason,
}

impl CollectRefusal {
    pub(crate) fn io(path: &Path, error: &std::io::Error, directory: bool) -> Self {
        let kind = error.kind();
        let message = error.to_string();
        Self {
            path: path.to_path_buf(),
            reason: if directory {
                CollectRefusalReason::UnreadableDirectory { kind, message }
            } else {
                CollectRefusalReason::UnreadableEntry { kind, message }
            },
        }
    }

    /// The refused path relative to `root` as remote index text, when it has
    /// one. A non-UTF-8 path has none: nothing under it was ever tracked.
    pub fn rel_path_text(&self, root: &Path) -> Option<String> {
        let rel = self.path.strip_prefix(root).ok()?;
        let text = rel.to_str()?;
        Some(crate::engine::normalize_rel_path_text(text))
    }
}

/// Wall-clock nanoseconds since the Unix epoch, for racy classification.
pub(crate) fn now_ns() -> i128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos() as i128)
        .unwrap_or(0)
}

/// Stat identity of an open descriptor (`fstat`). `None` for a non-regular
/// file or a platform without POSIX inode metadata.
pub(crate) fn fd_identity(file: &File) -> std::io::Result<Option<StatIdentity>> {
    Ok(stat_identity(&file.metadata()?))
}

/// Stat identity of whatever `path` names now (following symlinks, as the
/// upload path always has).
pub(crate) fn path_identity(path: &Path) -> std::io::Result<Option<StatIdentity>> {
    Ok(stat_identity(&std::fs::metadata(path)?))
}

/// Whether a capture cannot be vouched for by its stat identity alone.
pub(crate) fn is_racy(identity: Option<&StatIdentity>, started_ns: i128, ended_ns: i128) -> bool {
    let Some(identity) = identity else {
        // No identity at all: only the content can vouch for the bytes.
        return true;
    };
    let window = started_ns.saturating_sub(racy_granularity_ns());
    let mtime = identity.mtime_ns();
    let ctime = identity.ctime_ns();
    mtime >= window || ctime >= window || mtime > ended_ns || ctime > ended_ns
}

/// Files at or above this size take the streaming (bounded-memory) capture.
pub(crate) fn streaming_threshold() -> u64 {
    #[cfg(test)]
    if let Some(threshold) = test_support::streaming_threshold_override() {
        return threshold;
    }
    tcfs_chunks::STREAMING_THRESHOLD
}

fn racy_granularity_ns() -> i128 {
    #[cfg(test)]
    if let Some(window) = test_support::racy_override() {
        return window;
    }
    RACY_GRANULARITY_NS
}

/// Record bytes read from an upload source. Test builds keep a per-path
/// ledger so the read budget above is enforced by tests; release builds
/// compile this to nothing.
#[inline]
pub(crate) fn note_source_read(path: &Path, bytes: u64) {
    #[cfg(test)]
    test_support::note(path, bytes);
    #[cfg(not(test))]
    let _ = (path, bytes);
}

/// Test hook run after each capture's opening `fstat`, before its read.
#[inline]
pub(crate) fn before_capture_read(path: &Path, attempt: u32) {
    #[cfg(test)]
    test_support::run_before_capture_read(path, attempt);
    #[cfg(not(test))]
    let _ = (path, attempt);
}

/// Test hook run after each capture read, before the closing stat check.
#[inline]
pub(crate) fn after_capture_read(path: &Path, attempt: u32) {
    #[cfg(test)]
    test_support::run_after_capture_read(path, attempt);
    #[cfg(not(test))]
    let _ = (path, attempt);
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;
    use std::sync::Mutex;

    static READS: Mutex<Option<HashMap<PathBuf, u64>>> = Mutex::new(None);

    pub(crate) fn note(path: &Path, bytes: u64) {
        let mut guard = READS.lock().unwrap_or_else(|e| e.into_inner());
        *guard
            .get_or_insert_with(HashMap::new)
            .entry(path.to_path_buf())
            .or_default() += bytes;
    }

    /// Total bytes the upload path read from `path` so far.
    pub(crate) fn source_bytes_read(path: &Path) -> u64 {
        let guard = READS.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .as_ref()
            .and_then(|reads| reads.get(path).copied())
            .unwrap_or(0)
    }

    thread_local! {
        static STREAMING_THRESHOLD_OVERRIDE: Cell<Option<u64>> = const { Cell::new(None) };
        static RACY_OVERRIDE: Cell<Option<i128>> = const { Cell::new(None) };
        #[allow(clippy::type_complexity)]
        static AFTER_READ: RefCell<Option<Box<dyn FnMut(&Path, u32)>>> = const { RefCell::new(None) };
        #[allow(clippy::type_complexity)]
        static BEFORE_READ: RefCell<Option<Box<dyn FnMut(&Path, u32)>>> = const { RefCell::new(None) };
    }

    /// Install a hook that runs after each capture's opening stat, before
    /// its read, on this thread.
    pub(crate) struct BeforeCaptureRead;

    impl BeforeCaptureRead {
        pub(crate) fn install(hook: impl FnMut(&Path, u32) + 'static) -> Self {
            BEFORE_READ.with(|cell| *cell.borrow_mut() = Some(Box::new(hook)));
            Self
        }
    }

    impl Drop for BeforeCaptureRead {
        fn drop(&mut self) {
            BEFORE_READ.with(|cell| *cell.borrow_mut() = None);
        }
    }

    pub(crate) fn run_before_capture_read(path: &Path, attempt: u32) {
        BEFORE_READ.with(|cell| {
            if let Some(hook) = cell.borrow_mut().as_mut() {
                hook(path, attempt);
            }
        });
    }

    pub(crate) fn streaming_threshold_override() -> Option<u64> {
        STREAMING_THRESHOLD_OVERRIDE.with(Cell::get)
    }

    /// Route files of at least `threshold` bytes through the streaming
    /// capture on this thread until dropped.
    pub(crate) struct StreamingThreshold;

    impl StreamingThreshold {
        pub(crate) fn set(threshold: u64) -> Self {
            STREAMING_THRESHOLD_OVERRIDE.with(|cell| cell.set(Some(threshold)));
            Self
        }
    }

    impl Drop for StreamingThreshold {
        fn drop(&mut self) {
            STREAMING_THRESHOLD_OVERRIDE.with(|cell| cell.set(None));
        }
    }

    pub(crate) fn racy_override() -> Option<i128> {
        RACY_OVERRIDE.with(Cell::get)
    }

    /// Treat every capture on this thread as settled (the racy window is
    /// disabled; a stamp later than the capture end is still racy) until
    /// dropped. Freshly written test files are otherwise always racy.
    pub(crate) struct SettledCaptures;

    impl SettledCaptures {
        pub(crate) fn enable() -> Self {
            RACY_OVERRIDE.with(|cell| cell.set(Some(i128::MIN / 4)));
            Self
        }
    }

    impl Drop for SettledCaptures {
        fn drop(&mut self) {
            RACY_OVERRIDE.with(|cell| cell.set(None));
        }
    }

    /// Install a hook that runs after each capture read on this thread.
    pub(crate) struct AfterCaptureRead;

    impl AfterCaptureRead {
        pub(crate) fn install(hook: impl FnMut(&Path, u32) + 'static) -> Self {
            AFTER_READ.with(|cell| *cell.borrow_mut() = Some(Box::new(hook)));
            Self
        }
    }

    impl Drop for AfterCaptureRead {
        fn drop(&mut self) {
            AFTER_READ.with(|cell| *cell.borrow_mut() = None);
        }
    }

    pub(crate) fn run_after_capture_read(path: &Path, attempt: u32) {
        AFTER_READ.with(|cell| {
            if let Some(hook) = cell.borrow_mut().as_mut() {
                hook(path, attempt);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(mtime_ns: i128, ctime_ns: i128) -> StatIdentity {
        StatIdentity {
            dev: 1,
            ino: 2,
            size: 3,
            mtime_sec: (mtime_ns / 1_000_000_000) as i64,
            mtime_nsec: (mtime_ns % 1_000_000_000) as i64,
            ctime_sec: (ctime_ns / 1_000_000_000) as i64,
            ctime_nsec: (ctime_ns % 1_000_000_000) as i64,
        }
    }

    #[test]
    fn racy_classification_matches_git_and_bulkload() {
        let start = 100 * 1_000_000_000i128;
        let end = start + 1_000_000_000;
        // Settled: both stamps well before the window.
        assert!(!is_racy(
            Some(&identity(start - 10_000_000_000, start - 9_000_000_000)),
            start,
            end
        ));
        // mtime inside the window.
        assert!(is_racy(
            Some(&identity(start - 1_000_000_000, start - 9_000_000_000)),
            start,
            end
        ));
        // ctime inside the window (a forged mtime cannot hide a fresh write).
        assert!(is_racy(
            Some(&identity(start - 10_000_000_000, start - 1)),
            start,
            end
        ));
        // Stamped after the capture ended: a clock we cannot order against.
        assert!(is_racy(
            Some(&identity(end + 1, start - 9_000_000_000)),
            start,
            end
        ));
        // No identity at all is always racy.
        assert!(is_racy(None, start, end));
    }

    #[test]
    fn source_changed_is_typed_and_names_stage() {
        let error = anyhow::Error::new(SourceChanged::new(
            Path::new("/r/a.txt"),
            "manifest publish",
            "stat identity moved",
        ));
        let typed = error.downcast_ref::<SourceChanged>().unwrap();
        assert_eq!(typed.stage, "manifest publish");
        assert!(error
            .to_string()
            .contains("file changed during manifest publish: stat identity moved"));
    }
}
