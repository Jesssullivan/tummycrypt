//! Bounded, additive hydration of retained compressed provider rollouts.
use std::fs::{self, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::fd::AsRawFd as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use rayon::prelude::*;
use rusqlite::{Connection, OpenFlags};

use super::{mapped_rollout_path, sql_refusal, PathMapping};
use crate::{BulkloadRefusal, Result};

/// Per-file receipt. The compressed original is never removed.
#[derive(Debug)]
pub struct Hydrated {
    pub path: std::path::PathBuf,
    pub source: std::path::PathBuf,
    pub source_identity: (u64, u64, u64, i64, i64, i64, i64),
    pub bytes: u64,
    pub blake3: String,
    pub published: bool,
}

/// Hydrate only absent raw rollouts named by a retained database snapshot.
///
/// Caller must choose `max_bytes` below available filesystem space with a reserve.
/// The shared actual-output budget is enforced before every write. Existing files
/// and symlinks are never replaced. Native decompressors validate their checksums.
/// Errors leave already published complete files in place for an idempotent retry.
/// # Errors
/// Refuses invalid limits, changed inputs, decompressor errors and budget exhaustion.
#[allow(clippy::too_many_arguments)] // Explicit native tools and receipt sink are caller-owned.
pub fn hydrate_state(
    snapshot: &Path,
    mapping: &PathMapping<'_>,
    max_bytes: u64,
    jobs: usize,
    gzip: &Path,
    zstd: &Path,
    receipt: &(impl Fn(&Hydrated) -> Result<()> + Sync),
) -> Result<Vec<Hydrated>> {
    if max_bytes == 0 || jobs == 0 || jobs > 4 {
        return Err(BulkloadRefusal::BudgetExceeded);
    }
    let connection = Connection::open_with_flags(snapshot, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(sql_refusal)?;
    let mut statement = connection
        .prepare("SELECT DISTINCT rollout_path FROM threads ORDER BY rollout_path")
        .map_err(sql_refusal)?;
    let paths = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(sql_refusal)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(sql_refusal)?;
    let budget = AtomicU64::new(max_bytes);
    let sequence = AtomicU64::new(0);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(jobs)
        .build()
        .map_err(|_| BulkloadRefusal::Io(None))?;
    pool.install(|| {
        paths
            .par_iter()
            .filter_map(|path| {
                let raw = mapped_rollout_path(Path::new(path), mapping);
                if raw
                    .components()
                    .any(|p| p == std::path::Component::ParentDir)
                {
                    return Some(Err(BulkloadRefusal::PathNotPortable));
                }
                // Unsupported historical roots remain unresolved in composition, not hydrated.
                if !raw.starts_with(mapping.destination_home) {
                    return None;
                }
                if fs::symlink_metadata(&raw).is_ok() {
                    return None;
                }
                let source =
                    [("gz", gzip), ("zst", zstd)]
                        .into_iter()
                        .find_map(|(extension, program)| {
                            let source = std::path::PathBuf::from(format!(
                                "{}.{}",
                                raw.display(),
                                extension
                            ));
                            source.is_file().then_some((source, program))
                        });
                source.map(|(source, program)| {
                    let hydrated = hydrate_one(
                        &source,
                        &raw,
                        program,
                        &budget,
                        sequence.fetch_add(1, Ordering::Relaxed),
                        100 * 1024 * 1024 * 1024,
                    )?;
                    receipt(&hydrated)?;
                    Ok(hydrated)
                })
            })
            .collect()
    })
}

fn hydrate_one(
    source: &Path,
    raw: &Path,
    program: &Path,
    budget: &AtomicU64,
    sequence: u64,
    reserve: u64,
) -> Result<Hydrated> {
    let identity = |m: &fs::Metadata| {
        (
            m.dev(),
            m.ino(),
            m.len(),
            m.mtime(),
            m.mtime_nsec(),
            m.ctime(),
            m.ctime_nsec(),
        )
    };
    let before = fs::metadata(source).map_err(|_| BulkloadRefusal::Io(None))?;
    let temporary = raw.with_extension(format!(
        "bulkload-hydrate-{}-{sequence}",
        std::process::id()
    ));
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|_| BulkloadRefusal::Io(None))?;
    let result = (|| {
        let mut child = Command::new(program)
            .args(["-dc", "--"])
            .arg(source)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| BulkloadRefusal::Io(None))?;
        let mut input = child.stdout.take().ok_or(BulkloadRefusal::Io(None))?;
        let mut hash = blake3::Hasher::new();
        let mut bytes = 0u64;
        let mut buffer = vec![0u8; 65536];
        let streamed: Result<()> = (|| {
            loop {
                if bytes % (64 * 1024 * 1024) < 65536 {
                    reserve_space(&output, reserve)?;
                }
                let count = input
                    .read(&mut buffer)
                    .map_err(|_| BulkloadRefusal::Io(None))?;
                if count == 0 {
                    break;
                }
                budget
                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
                        remaining.checked_sub(count as u64)
                    })
                    .map_err(|_| BulkloadRefusal::BudgetExceeded)?;
                let data = buffer.get(..count).ok_or(BulkloadRefusal::BudgetExceeded)?;
                output
                    .write_all(data)
                    .map_err(|_| BulkloadRefusal::Io(None))?;
                hash.update(data);
                bytes += count as u64;
            }
            Ok(())
        })();
        drop(input);
        let status = child.wait().map_err(|_| BulkloadRefusal::Io(None))?;
        streamed?;
        if !status.success()
            || identity(&before)
                != identity(&fs::metadata(source).map_err(|_| BulkloadRefusal::Io(None))?)
        {
            return Err(BulkloadRefusal::SqliteStateChanged);
        }
        output.sync_all().map_err(|_| BulkloadRefusal::Io(None))?;
        let published = match fs::hard_link(&temporary, raw) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
            Err(_) => return Err(BulkloadRefusal::Io(None)),
        };
        if published {
            fs::File::open(raw.parent().ok_or(BulkloadRefusal::Io(None))?)
                .and_then(|parent| parent.sync_all())
                .map_err(|_| BulkloadRefusal::Io(None))?;
        }
        Ok(Hydrated {
            path: raw.to_path_buf(),
            source: source.to_path_buf(),
            source_identity: identity(&before),
            bytes,
            blake3: hash.finalize().to_hex().to_string(),
            published,
        })
    })();
    drop(output);
    fs::remove_file(&temporary).map_err(|_| BulkloadRefusal::Io(None))?;
    result
}

fn reserve_space(file: &fs::File, reserve: u64) -> Result<()> {
    let mut metadata = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: descriptor remains open and metadata points to writable storage.
    if unsafe { libc::fstatvfs(file.as_raw_fd(), metadata.as_mut_ptr()) } != 0 {
        return Err(BulkloadRefusal::Io(None));
    }
    // SAFETY: successful fstatvfs initialized the structure.
    let metadata = unsafe { metadata.assume_init() };
    let available = u128::from(metadata.f_bavail) * u128::from(metadata.f_frsize);
    if available < (u128::from(reserve) + 4 * 64 * 1024 * 1024) {
        return Err(BulkloadRefusal::BudgetExceeded);
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::super::mapped_path;
    use super::*;

    #[test]
    fn mapping_alias_is_typed() {
        let mapping = PathMapping {
            source_home: Path::new("/Users/jess"),
            destination_home: Path::new("/home/jess"),
        };
        assert_eq!(
            mapped_rollout_path(
                Path::new("/Users/jess/.codex/sessions/x.jsonl.gz"),
                &mapping
            ),
            Path::new("/home/jess/.codex/sessions/x.jsonl")
        );
        assert_eq!(
            mapped_path(
                Path::new("/Volumes/TinylandState/tinyland-state/codex/sessions/x.jsonl"),
                &mapping
            ),
            Path::new("/home/jess/.codex/sessions/x.jsonl")
        );
        assert_eq!(
            mapped_path(
                Path::new("/Volumes/TinylandState/tinyland-state/codex-other/x"),
                &mapping
            ),
            Path::new("/Volumes/TinylandState/tinyland-state/codex-other/x")
        );
    }

    #[test]
    fn native_hydration_preserves_collision_and_refuses_budget() {
        let directory =
            std::env::temp_dir().join(format!("tcfs-hydrate-test-{}", std::process::id()));
        fs::create_dir(&directory).expect("owned test directory");
        let source = directory.join("original.gz");
        let compressed = Command::new("gzip")
            .arg("-c")
            .stdin(Stdio::null())
            .output()
            .expect("native gzip");
        assert!(compressed.status.success());
        fs::write(&source, &compressed.stdout).expect("compressed input");
        let raw = directory.join("raw.jsonl");
        fs::write(&raw, b"active destination").expect("destination");
        let receipt = hydrate_one(
            &source,
            &raw,
            Path::new("/usr/bin/gzip"),
            &AtomicU64::new(1024),
            0,
            0,
        )
        .expect("no-clobber");
        assert!(!receipt.published);
        assert_eq!(fs::read(&raw).expect("raw"), b"active destination");
        let original = directory.join("payload");
        fs::write(&original, b"nonempty").expect("source");
        let compressed = Command::new("gzip")
            .arg("-c")
            .arg(&original)
            .output()
            .expect("gzip");
        fs::write(&source, compressed.stdout).expect("compressed");
        let absent = directory.join("absent.jsonl");
        assert!(hydrate_one(
            &source,
            &absent,
            Path::new("/usr/bin/gzip"),
            &AtomicU64::new(1),
            1,
            0
        )
        .is_err());
        assert!(!absent.exists());
        fs::write(&source, b"invalid gzip").expect("corrupt fixture");
        assert!(hydrate_one(
            &source,
            &absent,
            Path::new("/usr/bin/gzip"),
            &AtomicU64::new(1024),
            2,
            0
        )
        .is_err());
        assert!(!absent.exists());
        fs::remove_dir_all(&directory).expect("remove exact owned test directory");
    }
}
