//! The corpus walker.
//!
//! One pass over a corpus root producing one [`RowSchema`] per seat. Two
//! properties matter and both are measured, not asserted:
//!
//! * Cache lookup uses the walk's metadata. A stale content read additionally
//!   checks the opened descriptor before and after hashing to detect writers.
//!   `files_statted_twice` counts these seats; warm resumes need no such checks.
//! * **Fresh seats are not re-read.** A seat the cache calls
//!   [`Freshness::Fresh`] contributes zero bytes to `bytes_reread_on_resume`.
//!
//! Refusals are forward-progressing: an unreadable or unportable seat is
//! recorded and the walk continues, matching the Python engine's
//! "every refusal is silent and forward-progressing" contract.

use std::path::{Path, PathBuf};

use rayon::prelude::{IntoParallelRefIterator, ParallelIterator};
use tcfs_bulkload_proto::{BulkloadRefusal, FileKind, Result, RowSchema};

use crate::freshness::{Freshness, FreshnessCache, StatIdentity};
use crate::hash;

/// How the walker should treat file contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashPolicy {
    /// Stat only. The fastest arm of the M0 bench.
    Never,
    /// Hash every regular file the cache calls stale.
    StaleOnly,
    /// Hash every regular file regardless of freshness. The cold baseline.
    Always,
}

/// Walker configuration.
#[derive(Debug, Clone)]
pub struct WalkOptions {
    /// Corpus root. Must be absolute.
    pub root: PathBuf,
    /// Whether and when to hash file contents.
    pub hash_policy: HashPolicy,
    /// Whether to descend into other filesystems.
    pub cross_device: bool,
}

impl WalkOptions {
    /// A stat-only walk of `root`, staying on one device.
    #[must_use]
    pub const fn new(root: PathBuf) -> Self {
        Self {
            root,
            hash_policy: HashPolicy::Never,
            cross_device: false,
        }
    }
}

/// A seat the walker declined, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedSeat {
    /// Path relative to the corpus root, as raw OS bytes.
    pub rel_path: Vec<u8>,
    /// Why the seat was declined.
    pub refusal: BulkloadRefusal,
}

/// Counters for one walk.
///
/// The first two fields are the R25 headline metrics; the M0 bench prints them
/// as its own columns. M1 wires the accounting; the numbers only become
/// meaningful once the M2 resume path exists.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct WalkStats {
    /// Seats visited.
    pub seats_seen: u64,
    /// Apparent bytes across all regular-file seats.
    pub bytes_seen: u64,
    /// Actual bytes read by hash workers, including reads later refused.
    pub bytes_read: u64,
    /// Seats the cache called [`Freshness::Fresh`].
    pub fresh_skipped: u64,
    /// Bytes read again despite the cache calling the seat fresh.
    ///
    /// R25 headline metric. The product bar is zero.
    pub bytes_reread_on_resume: u64,
    /// Seats with actual descriptor metadata calls beyond the census stat.
    /// Warm resumes should keep this at zero.
    pub files_statted_twice: u64,
}

/// The result of one walk.
#[derive(Debug, Default, Clone)]
pub struct WalkOutcome {
    /// One row per accepted seat.
    pub rows: Vec<RowSchema>,
    /// One entry per declined seat.
    pub refusals: Vec<RefusedSeat>,
    /// Counters for the pass.
    pub stats: WalkStats,
}

/// Walk `options.root`, consulting and updating `cache`.
///
/// # Errors
///
/// Refuses with [`BulkloadRefusal::PathNotAbsolute`] if the root is relative,
/// and [`BulkloadRefusal::Io`] if the root itself cannot be statted. Per-seat
/// problems are recorded in [`WalkOutcome::refusals`], not returned.
pub fn walk<C: FreshnessCache>(options: &WalkOptions, cache: &mut C) -> Result<WalkOutcome> {
    if !options.root.is_absolute() {
        return Err(BulkloadRefusal::PathNotAbsolute);
    }
    let root_meta = std::fs::metadata(&options.root)?;
    let root_dev = device_of(&root_meta);

    let mut outcome = WalkOutcome::default();
    let mut to_hash: Vec<(usize, PathBuf, StatIdentity, bool)> = Vec::new();

    let walker = ignore::WalkBuilder::new(&options.root)
        // Bulkload copies a corpus, not a source tree: gitignore, hidden-file
        // and parent-ignore filtering would silently drop payload.
        .standard_filters(false)
        .hidden(false)
        .follow_links(false)
        .same_file_system(!options.cross_device)
        .build();

    for entry in walker {
        let Ok(entry) = entry else {
            // An empty relative path marks an incomplete root traversal;
            // never report a successful census after an enumeration error.
            outcome.refusals.push(RefusedSeat {
                rel_path: Vec::new(),
                refusal: BulkloadRefusal::Io(None),
            });
            continue;
        };
        let path = entry.path();
        if path == options.root {
            continue;
        }
        let rel_path = match relative_bytes(&options.root, path) {
            Ok(bytes) => bytes,
            Err(refusal) => {
                outcome.refusals.push(RefusedSeat {
                    rel_path: os_bytes(path.as_os_str()),
                    refusal,
                });
                continue;
            }
        };

        let Ok(meta) = entry.metadata() else {
            outcome.refusals.push(RefusedSeat {
                rel_path,
                refusal: BulkloadRefusal::Io(None),
            });
            continue;
        };
        if !options.cross_device && device_of(&meta) != root_dev {
            continue;
        }

        let mut row = row_from_metadata(rel_path, &meta);
        if row.kind == FileKind::Symlink {
            row.link_target = std::fs::read_link(path)
                .ok()
                .map(|target| os_bytes(target.as_os_str()));
        }

        outcome.stats.seats_seen += 1;
        if row.kind == FileKind::Regular {
            outcome.stats.bytes_seen = outcome.stats.bytes_seen.saturating_add(row.size);
        }

        // Cache lookup uses the metadata already in the row. Only an actual
        // content read needs descriptor checks for concurrent source changes.
        let identity = StatIdentity::from_row(&row);
        let requires_digest =
            row.kind == FileKind::Regular && options.hash_policy != HashPolicy::Never;
        let cached_digest = if requires_digest {
            cache.digest(&identity)?
        } else {
            None
        };
        let freshness = if requires_digest {
            if cached_digest.is_some() {
                Freshness::Fresh
            } else {
                Freshness::Stale
            }
        } else {
            cache.lookup(&identity)?
        };
        if freshness == Freshness::Fresh {
            outcome.stats.fresh_skipped += 1;
        }

        let wants_hash = row.kind == FileKind::Regular
            && match options.hash_policy {
                HashPolicy::Never => false,
                HashPolicy::StaleOnly => freshness == Freshness::Stale,
                HashPolicy::Always => true,
            };
        if wants_hash {
            to_hash.push((
                outcome.rows.len(),
                path.to_path_buf(),
                identity,
                freshness == Freshness::Fresh,
            ));
        } else if requires_digest {
            row.blake3 = cached_digest;
        } else {
            cache.record(&identity)?;
        }
        outcome.rows.push(row);
    }

    complete_hashes(&mut outcome, &to_hash, cache)?;
    Ok(outcome)
}

fn complete_hashes<C: FreshnessCache>(
    outcome: &mut WalkOutcome,
    to_hash: &[(usize, PathBuf, StatIdentity, bool)],
    cache: &mut C,
) -> Result<()> {
    // Bounded handoff: workers never accumulate an O(N) result vector. The
    // owning thread commits each successful read while other workers continue.
    let (sender, receiver) = std::sync::mpsc::sync_channel(rayon::current_num_threads());
    std::thread::scope(|scope| -> Result<()> {
        let producer = std::thread::Builder::new().spawn_scoped(scope, move || {
            let _ = to_hash
                .par_iter()
                .try_for_each(|(index, path, identity, reread)| {
                    let read = hash::hash_file_observed(path, identity);
                    sender.send((*index, *reread, read)).map_err(|_| ())
                });
        })?;
        let mut result = Ok(());
        for (index, reread, read) in &receiver {
            if let Err(refusal) = finish_read(outcome, cache, index, reread, read) {
                result = Err(refusal);
                break;
            }
        }
        // A failed cache write must unblock senders before joining them.
        drop(receiver);
        producer.join().map_err(|_| BulkloadRefusal::Io(None))?;
        result
    })
}

fn finish_read<C: FreshnessCache>(
    outcome: &mut WalkOutcome,
    cache: &mut C,
    index: usize,
    reread: bool,
    read: hash::HashRead,
) -> Result<()> {
    outcome.stats.bytes_read = outcome.stats.bytes_read.saturating_add(read.bytes_read);
    if reread {
        outcome.stats.bytes_reread_on_resume = outcome
            .stats
            .bytes_reread_on_resume
            .saturating_add(read.bytes_read);
    }
    if read.metadata_checks > 0 {
        outcome.stats.files_statted_twice += 1;
    }
    let row = outcome
        .rows
        .get_mut(index)
        .ok_or(BulkloadRefusal::ContractSelfInconsistent)?;
    match read.digest {
        Ok(bytes) => {
            cache.record_digest(&StatIdentity::from_row(row), &bytes)?;
            row.blake3 = Some(bytes);
        }
        Err(refusal) => outcome.refusals.push(RefusedSeat {
            rel_path: row.rel_path.clone(),
            refusal,
        }),
    }
    Ok(())
}

fn device_of(meta: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt as _;
    meta.dev()
}

fn os_bytes(value: &std::ffi::OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt as _;
    value.as_bytes().to_vec()
}

fn relative_bytes(root: &Path, path: &Path) -> std::result::Result<Vec<u8>, BulkloadRefusal> {
    path.strip_prefix(root)
        .map(|rel| os_bytes(rel.as_os_str()))
        .map_err(|_| BulkloadRefusal::PathEscapesRoot)
}

fn row_from_metadata(rel_path: Vec<u8>, meta: &std::fs::Metadata) -> RowSchema {
    use std::os::unix::fs::MetadataExt as _;

    let kind = if meta.is_file() {
        FileKind::Regular
    } else if meta.is_dir() {
        FileKind::Directory
    } else if meta.is_symlink() {
        FileKind::Symlink
    } else {
        FileKind::Other
    };

    RowSchema {
        rel_path,
        kind,
        dev: meta.dev(),
        ino: meta.ino(),
        size: meta.size(),
        mtime_ns: nanos(meta.mtime(), meta.mtime_nsec()),
        ctime_ns: nanos(meta.ctime(), meta.ctime_nsec()),
        mode: meta.mode(),
        nlink: meta.nlink(),
        link_target: None,
        blake3: None,
    }
}

fn nanos(seconds: i64, nanoseconds: i64) -> i128 {
    i128::from(seconds) * 1_000_000_000 + i128::from(nanoseconds)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use std::path::PathBuf;

    use tcfs_bulkload_proto::FileKind;

    use super::{walk, HashPolicy, WalkOptions};
    use crate::freshness::MemoryCache;

    struct Corpus {
        root: PathBuf,
    }

    impl Corpus {
        fn new(name: &str) -> Self {
            let mut root = std::env::temp_dir();
            root.push(format!("tcfs-bulkload-walk-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("nested")).unwrap();
            std::fs::write(root.join("a.txt"), b"alpha").unwrap();
            std::fs::write(root.join("nested/b.txt"), b"bravo!").unwrap();
            Self { root }
        }
    }

    impl Drop for Corpus {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn refuses_a_relative_root() {
        let mut cache = MemoryCache::new();
        let options = WalkOptions::new(PathBuf::from("relative/path"));
        assert!(walk(&options, &mut cache).is_err());
    }

    #[test]
    fn walks_every_seat_once() {
        let corpus = Corpus::new("once");
        let mut cache = MemoryCache::new();
        let outcome = walk(&WalkOptions::new(corpus.root.clone()), &mut cache).unwrap();

        // two files plus one directory
        assert_eq!(outcome.stats.seats_seen, 3);
        assert_eq!(outcome.stats.bytes_seen, 11);
        assert_eq!(outcome.stats.files_statted_twice, 0);
        assert_eq!(outcome.stats.bytes_reread_on_resume, 0);
        assert!(outcome.refusals.is_empty());

        let files = outcome
            .rows
            .iter()
            .filter(|row| row.kind == FileKind::Regular)
            .count();
        assert_eq!(files, 2);
        assert!(outcome.rows.iter().all(|row| row.blake3.is_none()));
    }

    #[test]
    fn hashes_regular_files_when_asked() {
        let corpus = Corpus::new("hash");
        let mut cache = MemoryCache::new();
        let options = WalkOptions {
            hash_policy: HashPolicy::Always,
            ..WalkOptions::new(corpus.root.clone())
        };
        let outcome = walk(&options, &mut cache).unwrap();

        let hashed = outcome
            .rows
            .iter()
            .filter(|row| row.blake3.is_some())
            .count();
        assert_eq!(hashed, 2);
        assert!(outcome.refusals.is_empty());
    }

    #[test]
    fn a_warm_stale_only_walk_rereads_nothing() {
        let corpus = Corpus::new("resume");
        let mut cache = MemoryCache::new();
        let options = WalkOptions {
            hash_policy: HashPolicy::StaleOnly,
            ..WalkOptions::new(corpus.root.clone())
        };

        let cold = walk(&options, &mut cache).unwrap();
        assert_eq!(cold.stats.fresh_skipped, 0);
        assert_eq!(cold.stats.bytes_reread_on_resume, 0);
        assert_eq!(cold.stats.bytes_read, 11);
        assert_eq!(cold.stats.files_statted_twice, 2);

        let warm = walk(&options, &mut cache).unwrap();
        assert_eq!(warm.stats.fresh_skipped, warm.stats.seats_seen);
        // R25 headline bar: a resume that changed nothing re-reads nothing.
        assert_eq!(warm.stats.bytes_reread_on_resume, 0);
        assert_eq!(warm.stats.bytes_read, 0);
        assert_eq!(warm.stats.files_statted_twice, 0);
        assert_eq!(cold.rows, warm.rows);
    }

    #[test]
    fn an_always_hash_walk_over_a_warm_cache_counts_the_rereads() {
        let corpus = Corpus::new("reread");
        let mut cache = MemoryCache::new();
        let always = WalkOptions {
            hash_policy: HashPolicy::Always,
            ..WalkOptions::new(corpus.root.clone())
        };
        walk(&always, &mut cache).unwrap();
        let warm = walk(&always, &mut cache).unwrap();
        assert_eq!(warm.stats.bytes_reread_on_resume, 11);
    }

    #[test]
    fn stat_only_census_does_not_poison_content_resume() {
        let corpus = Corpus::new("stat-then-hash");
        let mut cache = MemoryCache::new();
        walk(&WalkOptions::new(corpus.root.clone()), &mut cache).unwrap();
        let options = WalkOptions {
            hash_policy: HashPolicy::StaleOnly,
            ..WalkOptions::new(corpus.root.clone())
        };
        let hashed = walk(&options, &mut cache).unwrap();
        assert_eq!(
            hashed
                .rows
                .iter()
                .filter(|row| row.blake3.is_some())
                .count(),
            2
        );
        assert_eq!(hashed.stats.bytes_reread_on_resume, 0);
    }

    #[test]
    fn refused_hash_never_records_completion() {
        use crate::freshness::{FreshnessCache, StatIdentity};
        use tcfs_bulkload_proto::Result;
        struct RemovedDuringLookup {
            path: PathBuf,
        }
        impl FreshnessCache for RemovedDuringLookup {
            fn lookup(&self, _: &StatIdentity) -> Result<crate::freshness::Freshness> {
                Ok(crate::freshness::Freshness::Stale)
            }
            fn record(&mut self, _: &StatIdentity) -> Result<()> {
                Ok(())
            }
            fn digest(&self, _: &StatIdentity) -> Result<Option<[u8; 32]>> {
                let _ = std::fs::remove_file(&self.path);
                Ok(None)
            }
            fn record_digest(&mut self, _: &StatIdentity, _: &[u8; 32]) -> Result<()> {
                panic!("failed hash must not become a completion")
            }
        }
        let corpus = Corpus::new("hash-failure");
        std::fs::remove_file(corpus.root.join("nested/b.txt")).unwrap();
        let mut cache = RemovedDuringLookup {
            path: corpus.root.join("a.txt"),
        };
        let options = WalkOptions {
            hash_policy: HashPolicy::StaleOnly,
            ..WalkOptions::new(corpus.root.clone())
        };
        let result = walk(&options, &mut cache).unwrap();
        assert_eq!(result.refusals.len(), 1);
        assert_eq!(result.stats.bytes_read, 0);
        assert_eq!(result.stats.files_statted_twice, 0);
        assert!(result.rows.iter().all(|row| row.blake3.is_none()));
    }
}
