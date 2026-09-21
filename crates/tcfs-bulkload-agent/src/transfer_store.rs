//! Private, durable chunk storage and completion records for native transfers.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{Read as _, Seek as _, Write as _};
use std::os::fd::AsRawFd as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use rusqlite::OptionalExtension as _;
use serde::{Deserialize, Serialize};
use tcfs_bulkload_proto::frame::ChunkSpec;

use crate::freshness::StatIdentity;
use crate::{BulkloadRefusal, Result, RowSchema};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
static PUT_CALLS: AtomicU64 = AtomicU64::new(0);
static PUT_NS: AtomicU64 = AtomicU64::new(0);
static FILE_SYNCS: AtomicU64 = AtomicU64::new(0);
static FILE_SYNC_NS: AtomicU64 = AtomicU64::new(0);
static DIR_SYNCS: AtomicU64 = AtomicU64::new(0);
static DIR_SYNC_NS: AtomicU64 = AtomicU64::new(0);
static PUBLISH_GROUPS: AtomicU64 = AtomicU64::new(0);
static PACK_APPEND_NS: AtomicU64 = AtomicU64::new(0);
static SQLITE_COMMITS: AtomicU64 = AtomicU64::new(0);
static SQLITE_COMMIT_NS: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum PublishFault {
    None,
    AfterAppend,
    AfterSync,
    AfterLocationInsert,
    AfterManifestInsert,
    BeforeCommit,
}

#[cfg(test)]
std::thread_local! {
    static PUBLISH_FAULT: std::cell::Cell<PublishFault> = const {
        std::cell::Cell::new(PublishFault::None)
    };
}

#[cfg(test)]
fn inject_fault(point: PublishFault) -> Result<()> {
    if PUBLISH_FAULT.with(std::cell::Cell::get) == point {
        return Err(BulkloadRefusal::Io(None));
    }
    Ok(())
}

#[cfg(test)]
macro_rules! publication_fault {
    ($point:ident) => {{
        inject_fault(PublishFault::$point)?;
    }};
}

#[cfg(not(test))]
macro_rules! publication_fault {
    ($point:ident) => {{}};
}

/// Maximum buffered chunks per producer (at most 64 MiB of chunk payload).
pub(crate) const PERSIST_BATCH: usize = 256;
#[cfg(test)]
const PERSIST_WORKERS: usize = 2;

/// Process-local chunk persistence instrumentation.
///
/// Durations sum worker time,
/// including failed operations, and must not be interpreted as wall-time shares.
/// Concurrent independent transfers in the same process also contribute.
#[derive(Clone, Copy, Debug)]
pub struct ChunkTiming {
    /// Calls to `put_chunk`, including already-present content.
    pub put_calls: u64,
    /// Aggregate worker nanoseconds inside `put_chunk`.
    pub put_ns: u64,
    /// Attempted chunk-file syncs.
    pub file_syncs: u64,
    /// Aggregate chunk-file sync worker nanoseconds.
    pub file_sync_ns: u64,
    /// Attempted chunk-directory syncs.
    pub dir_syncs: u64,
    /// Aggregate chunk-directory sync worker nanoseconds.
    pub dir_sync_ns: u64,
    /// Durable publication groups attempted by this process.
    pub publish_groups: u64,
    /// Aggregate nanoseconds spent appending payload bytes to the pack.
    pub pack_append_ns: u64,
    /// Durable `SQLite` publication commits attempted by this process.
    pub sqlite_commits: u64,
    /// Aggregate nanoseconds spent committing publication transactions.
    pub sqlite_commit_ns: u64,
}

impl ChunkTiming {
    /// Snapshot counters without resetting other callers' observations.
    #[must_use]
    pub fn snapshot() -> Self {
        Self {
            put_calls: PUT_CALLS.load(Ordering::Relaxed),
            put_ns: PUT_NS.load(Ordering::Relaxed),
            file_syncs: FILE_SYNCS.load(Ordering::Relaxed),
            file_sync_ns: FILE_SYNC_NS.load(Ordering::Relaxed),
            dir_syncs: DIR_SYNCS.load(Ordering::Relaxed),
            dir_sync_ns: DIR_SYNC_NS.load(Ordering::Relaxed),
            publish_groups: PUBLISH_GROUPS.load(Ordering::Relaxed),
            pack_append_ns: PACK_APPEND_NS.load(Ordering::Relaxed),
            sqlite_commits: SQLITE_COMMITS.load(Ordering::Relaxed),
            sqlite_commit_ns: SQLITE_COMMIT_NS.load(Ordering::Relaxed),
        }
    }

    /// Difference from a prior snapshot after the observed operation has joined.
    #[must_use]
    pub const fn since(self, before: Self) -> Self {
        Self {
            put_calls: self.put_calls.saturating_sub(before.put_calls),
            put_ns: self.put_ns.saturating_sub(before.put_ns),
            file_syncs: self.file_syncs.saturating_sub(before.file_syncs),
            file_sync_ns: self.file_sync_ns.saturating_sub(before.file_sync_ns),
            dir_syncs: self.dir_syncs.saturating_sub(before.dir_syncs),
            dir_sync_ns: self.dir_sync_ns.saturating_sub(before.dir_sync_ns),
            publish_groups: self.publish_groups.saturating_sub(before.publish_groups),
            pack_append_ns: self.pack_append_ns.saturating_sub(before.pack_append_ns),
            sqlite_commits: self.sqlite_commits.saturating_sub(before.sqlite_commits),
            sqlite_commit_ns: self
                .sqlite_commit_ns
                .saturating_sub(before.sqlite_commit_ns),
        }
    }
}

struct PutTimer(Instant);

impl Drop for PutTimer {
    fn drop(&mut self) {
        PUT_NS.fetch_add(nanos(self.0), Ordering::Relaxed);
    }
}

fn nanos(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

fn timed_sync(file: &fs::File, count: &AtomicU64, time: &AtomicU64) -> std::io::Result<()> {
    count.fetch_add(1, Ordering::Relaxed);
    let started = Instant::now();
    let result = file.sync_all();
    time.fetch_add(nanos(started), Ordering::Relaxed);
    result
}

/// Completed content capture; chunks retain file order, including repetitions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// Digest of the complete file.
    pub digest: [u8; 32],
    /// Content-defined chunks in order.
    pub chunks: Vec<ChunkSpec>,
}

/// Bounded preparation output consumed by the sole durable publisher.
pub(crate) enum PreparedEvent {
    Chunks {
        capture_id: usize,
        chunks: Vec<([u8; 32], Vec<u8>)>,
    },
    Complete {
        capture_id: usize,
        bytes_read: u64,
        key: Vec<u8>,
        manifest: Manifest,
    },
    Refused {
        capture_id: usize,
        bytes_read: u64,
        refusal: BulkloadRefusal,
    },
}

/// A completed preparation whose publication outcome can cross the protocol.
pub(crate) struct PublishAck {
    pub capture_id: usize,
    pub bytes_read: u64,
    pub captured: Result<Manifest>,
}

struct Exclusive(fs::File);

impl Drop for Exclusive {
    fn drop(&mut self) {
        // SAFETY: this guard owns a live descriptor for the acquired flock.
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// Exclusive owner of pack offsets and durable capture publication.
pub(crate) struct StorePublisher<'a> {
    store: &'a Store,
    pack: fs::File,
    _exclusive: Exclusive,
}

/// A private, source-bound transfer state directory.
pub struct Store {
    root: PathBuf,
    conn: rusqlite::Connection,
}

impl Store {
    /// Open or create a private transfer store outside the carried roots.
    ///
    /// # Errors
    /// Refuses symlinks, non-private directories and database failures.
    pub fn open(root: &Path) -> Result<Self> {
        private_dir(root)?;
        private_dir(&root.join("chunks"))?;
        let pack = root.join("chunks.pack");
        if let Ok(meta) = fs::symlink_metadata(&pack) {
            if !meta.is_file() || meta.permissions().mode() & 0o077 != 0 {
                return Err(BulkloadRefusal::PathEscapesRoot);
            }
        } else {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&pack)?;
        }
        let db = root.join("transfer.sqlite");
        if let Ok(meta) = fs::symlink_metadata(&db) {
            if !meta.is_file() || meta.permissions().mode() & 0o077 != 0 {
                return Err(BulkloadRefusal::PathEscapesRoot);
            }
        } else {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&db)?;
        }
        let conn = rusqlite::Connection::open(db).map_err(sqlite_error)?;
        conn.busy_timeout(std::time::Duration::from_secs(60))
            .map_err(sqlite_error)?;
        conn.execute_batch(
            "PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS captures (key BLOB PRIMARY KEY, manifest BLOB NOT NULL);
            CREATE TABLE IF NOT EXISTS outputs (key BLOB PRIMARY KEY, identity BLOB NOT NULL);
            CREATE TABLE IF NOT EXISTS directories (key BLOB PRIMARY KEY, identity BLOB NOT NULL);
            CREATE TABLE IF NOT EXISTS chunks (digest BLOB PRIMARY KEY, payload BLOB NOT NULL);
            CREATE TABLE IF NOT EXISTS chunk_locations (digest BLOB PRIMARY KEY, offset INTEGER NOT NULL, size INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value BLOB NOT NULL);",
        )
        .map_err(sqlite_error)?;
        Ok(Self {
            root: fs::canonicalize(root)?,
            conn,
        })
    }

    /// Open an initialized store without obtaining any write capability.
    pub(crate) fn open_reader(root: &Path) -> Result<Self> {
        let root = fs::canonicalize(root)?;
        let db = root.join("transfer.sqlite");
        let meta = fs::symlink_metadata(&db)?;
        if !meta.is_file() || meta.permissions().mode() & 0o077 != 0 {
            return Err(BulkloadRefusal::PathEscapesRoot);
        }
        let conn = rusqlite::Connection::open_with_flags(
            db,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(sqlite_error)?;
        conn.busy_timeout(std::time::Duration::from_secs(60))
            .map_err(sqlite_error)?;
        Ok(Self { root, conn })
    }

    /// Acquire the nonblocking single-writer guard and reconcile the pack tail.
    pub(crate) fn publisher(&self) -> Result<StorePublisher<'_>> {
        StorePublisher::open(self)
    }

    /// Canonical state root, used to reject recursive self-capture.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// A persistent source-store identifier prevents cross-host inode collisions.
    ///
    /// # Errors
    /// Refuses unavailable OS randomness or malformed persistent authority.
    pub fn authority(&self) -> Result<[u8; 32]> {
        let found: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT value FROM settings WHERE key='authority'",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite_error)?;
        if let Some(bytes) = found {
            return bytes
                .try_into()
                .map_err(|_| BulkloadRefusal::SchemaMismatch);
        }
        let mut random = [0_u8; 32];
        fs::File::open("/dev/urandom")?.read_exact(&mut random)?;
        self.conn
            .execute(
                "INSERT OR IGNORE INTO settings VALUES ('authority', ?1)",
                [random.as_slice()],
            )
            .map_err(sqlite_error)?;
        let bytes: Vec<u8> = self
            .conn
            .query_row(
                "SELECT value FROM settings WHERE key='authority'",
                [],
                |row| row.get(0),
            )
            .map_err(sqlite_error)?;
        bytes
            .try_into()
            .map_err(|_| BulkloadRefusal::SchemaMismatch)
    }

    /// Retrieve a capture without reopening source content.
    ///
    /// # Errors
    /// Refuses malformed records or database errors.
    pub fn capture(&self, key: &[u8]) -> Result<Option<Manifest>> {
        let bytes: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT manifest FROM captures WHERE key = ?1",
                [key],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite_error)?;
        bytes
            .map(|value| postcard::from_bytes(&value).map_err(Into::into))
            .transpose()
    }

    /// Commit only a completed identity-checked capture.
    ///
    /// # Errors
    /// Refuses serialization or database failures.
    pub fn record_capture(&self, key: &[u8], value: &Manifest) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO captures VALUES (?1, ?2)
            ON CONFLICT(key) DO UPDATE SET manifest=excluded.manifest",
                (key, postcard::to_stdvec(value)?),
            )
            .map_err(sqlite_error)?;
        Ok(())
    }

    /// Whether a current output is the same object recorded on successful apply.
    ///
    /// # Errors
    /// Refuses database failures.
    pub fn output_matches(&self, key: &[u8], identity: &StatIdentity) -> Result<bool> {
        let found: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT identity FROM outputs WHERE key = ?1",
                [key],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite_error)?;
        Ok(found == Some(identity_bytes(identity)?))
    }

    /// Record output completion after fsync and publication.
    ///
    /// # Errors
    /// Refuses database failures.
    pub fn record_output(&self, key: &[u8], identity: &StatIdentity) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO outputs VALUES (?1, ?2)
            ON CONFLICT(key) DO UPDATE SET identity=excluded.identity",
                (key, identity_bytes(identity)?),
            )
            .map_err(sqlite_error)?;
        Ok(())
    }

    /// Remember an unfinished directory by inode and its intended mode.
    ///
    /// # Errors
    /// Refuses persistence failures.
    pub fn pending_directory(
        &self,
        key: &[u8],
        dev: u64,
        ino: u64,
        mode: u32,
        record: bool,
    ) -> Result<bool> {
        let identity = postcard::to_stdvec(&(dev, ino, mode))?;
        if record {
            self.conn
                .execute(
                    "INSERT INTO directories VALUES (?1, ?2)
                ON CONFLICT(key) DO UPDATE SET identity=excluded.identity",
                    (key, &identity),
                )
                .map_err(sqlite_error)?;
            return Ok(true);
        }
        let found: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT identity FROM directories WHERE key = ?1",
                [key],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite_error)?;
        Ok(found == Some(identity))
    }

    /// Retire pending ownership after directory metadata is durable.
    ///
    /// # Errors
    /// Refuses persistence failures.
    pub fn complete_directory(&self, key: &[u8]) -> Result<()> {
        self.conn
            .execute("DELETE FROM directories WHERE key = ?1", [key])
            .map_err(sqlite_error)?;
        Ok(())
    }

    /// Read and authenticate a stored chunk. Missing or corrupt chunks are misses.
    ///
    /// # Errors
    /// Refuses unexpected filesystem failures.
    pub fn chunk(&self, digest: &[u8; 32]) -> Result<Option<Vec<u8>>> {
        let stored: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT payload FROM chunks WHERE digest = ?1",
                [digest.as_slice()],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite_error)?;
        if let Some(data) = stored {
            if data.len() > crate::hash::CDC_MAX_BYTES as usize
                || crate::hash::hash_bytes(&data) != *digest
            {
                return Err(BulkloadRefusal::DigestMismatch);
            }
            return Ok(Some(data));
        }
        let location: Option<(i64, i64)> = self
            .conn
            .query_row(
                "SELECT offset, size FROM chunk_locations WHERE digest = ?1",
                [digest.as_slice()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(sqlite_error)?;
        if let Some((offset, size)) = location {
            let offset = u64::try_from(offset).map_err(|_| BulkloadRefusal::DigestMismatch)?;
            let size = usize::try_from(size).map_err(|_| BulkloadRefusal::DigestMismatch)?;
            if size > crate::hash::CDC_MAX_BYTES as usize {
                return Err(BulkloadRefusal::DigestMismatch);
            }
            let mut pack = crate::hash::open_nofollow(&self.root.join("chunks.pack"))?;
            pack.seek(std::io::SeekFrom::Start(offset))?;
            let mut data = vec![0_u8; size];
            pack.read_exact(&mut data)?;
            if crate::hash::hash_bytes(&data) != *digest {
                return Err(BulkloadRefusal::DigestMismatch);
            }
            return Ok(Some(data));
        }
        Self::chunk_at(&self.root, digest)
    }

    fn chunk_at(root: &Path, digest: &[u8; 32]) -> Result<Option<Vec<u8>>> {
        let path = root.join("chunks").join(hex(digest));
        let file = match crate::hash::open_nofollow(&path) {
            Ok(file) => file,
            Err(BulkloadRefusal::Io(Some(libc::ENOENT))) => return Ok(None),
            Err(error) => return Err(error),
        };
        let meta = file.metadata()?;
        if !meta.is_file() || meta.len() > u64::from(crate::hash::CDC_MAX_BYTES) {
            return Err(BulkloadRefusal::DigestMismatch);
        }
        let mut data = Vec::new();
        file.take(u64::from(crate::hash::CDC_MAX_BYTES) + 1)
            .read_to_end(&mut data)?;
        if crate::hash::hash_bytes(&data) != *digest {
            return Err(BulkloadRefusal::DigestMismatch);
        }
        Ok(Some(data))
    }

    /// Publish an authenticated chunk atomically without replacing existing data.
    ///
    /// # Errors
    /// Refuses a digest mismatch or failed durable publication.
    pub fn put_chunk(&self, digest: &[u8; 32], data: &[u8]) -> Result<()> {
        Self::put_chunk_at(&self.root, digest, data, true)
    }

    fn put_chunk_at(
        root: &Path,
        digest: &[u8; 32],
        data: &[u8],
        sync_directory: bool,
    ) -> Result<()> {
        PUT_CALLS.fetch_add(1, Ordering::Relaxed);
        let _timer = PutTimer(Instant::now());
        if data.len() > crate::hash::CDC_MAX_BYTES as usize
            || crate::hash::hash_bytes(data) != *digest
        {
            return Err(BulkloadRefusal::DigestMismatch);
        }
        if Self::chunk_at(root, digest)?.is_some() {
            // A concurrent publisher may have linked the already-synced inode
            // but not yet synced its directory. Fence that link before reuse.
            if sync_directory {
                timed_sync(
                    &fs::File::open(root.join("chunks"))?,
                    &DIR_SYNCS,
                    &DIR_SYNC_NS,
                )?;
            }
            return Ok(());
        }
        let chunks = root.join("chunks");
        let staging = chunks.join(format!(
            ".part-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&staging)?;
        let result = (|| -> Result<()> {
            file.write_all(data)?;
            timed_sync(&file, &FILE_SYNCS, &FILE_SYNC_NS)?;
            let target = chunks.join(hex(digest));
            match fs::hard_link(&staging, &target) {
                Ok(()) => (),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    Self::chunk_at(root, digest)?.ok_or(BulkloadRefusal::DigestMismatch)?;
                }
                Err(error) => return Err(error.into()),
            }
            Ok(())
        })();
        fs::remove_file(staging)?;
        if sync_directory {
            timed_sync(&fs::File::open(chunks)?, &DIR_SYNCS, &DIR_SYNC_NS)?;
        }
        result
    }
}

impl StorePublisher<'_> {
    fn open(store: &Store) -> Result<StorePublisher<'_>> {
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(store.root.join("writer.lock"))?;
        // SAFETY: the owned descriptor remains open for the guard lifetime.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let exclusive = Exclusive(lock);
        let mut pack = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(store.root.join("chunks.pack"))?;
        let pack_len = pack.metadata()?.len();
        let mut committed_end = 0_u64;
        {
            let mut statement = store
                .conn
                .prepare("SELECT offset, size FROM chunk_locations ORDER BY offset, size")
                .map_err(sqlite_error)?;
            let ranges = statement
                .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))
                .map_err(sqlite_error)?;
            for range in ranges {
                let (offset, size) = range.map_err(sqlite_error)?;
                let offset = u64::try_from(offset).map_err(|_| BulkloadRefusal::DigestMismatch)?;
                let size = u64::try_from(size).map_err(|_| BulkloadRefusal::DigestMismatch)?;
                let end = offset
                    .checked_add(size)
                    .ok_or(BulkloadRefusal::DigestMismatch)?;
                if size > u64::from(crate::hash::CDC_MAX_BYTES)
                    || offset < committed_end
                    || end > pack_len
                {
                    return Err(BulkloadRefusal::DigestMismatch);
                }
                committed_end = end;
            }
        }
        if pack_len > committed_end {
            pack.set_len(committed_end)?;
            timed_sync(&pack, &FILE_SYNCS, &FILE_SYNC_NS)?;
        }
        pack.seek(std::io::SeekFrom::Start(committed_end))?;
        Ok(StorePublisher {
            store,
            pack,
            _exclusive: exclusive,
        })
    }

    fn indexed_or_legacy(&self, digest: &[u8; 32]) -> Result<bool> {
        let indexed: bool = self
            .store
            .conn
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM chunks WHERE digest = ?1
                    UNION ALL
                    SELECT 1 FROM chunk_locations WHERE digest = ?1
                )",
                [digest.as_slice()],
                |row| row.get(0),
            )
            .map_err(sqlite_error)?;
        Ok(indexed || Store::chunk_at(&self.store.root, digest)?.is_some())
    }

    fn missing_chunks<'a>(&self, events: &'a [PreparedEvent]) -> Result<Vec<([u8; 32], &'a [u8])>> {
        let mut known = HashSet::new();
        let mut missing = Vec::new();
        for event in events {
            if let PreparedEvent::Chunks { chunks, .. } = event {
                if chunks.len() > PERSIST_BATCH {
                    return Err(BulkloadRefusal::BudgetExceeded);
                }
                for (digest, data) in chunks {
                    if data.len() > crate::hash::CDC_MAX_BYTES as usize
                        || crate::hash::hash_bytes(data) != *digest
                    {
                        return Err(BulkloadRefusal::DigestMismatch);
                    }
                    if known.insert(*digest) && !self.indexed_or_legacy(digest)? {
                        missing.push((*digest, data.as_slice()));
                    }
                }
            }
        }
        for event in events {
            if let PreparedEvent::Complete { manifest, .. } = event {
                if manifest
                    .chunks
                    .iter()
                    .any(|chunk| chunk.size > u64::from(crate::hash::CDC_MAX_BYTES))
                {
                    return Err(BulkloadRefusal::BudgetExceeded);
                }
                for chunk in &manifest.chunks {
                    if !known.contains(&chunk.digest) && !self.indexed_or_legacy(&chunk.digest)? {
                        return Err(BulkloadRefusal::SealedObjectMissing);
                    }
                }
            }
        }
        Ok(missing)
    }

    fn append_chunks(
        &mut self,
        missing: Vec<([u8; 32], &[u8])>,
    ) -> Result<Vec<([u8; 32], u64, usize)>> {
        let append_started = Instant::now();
        let mut locations = Vec::with_capacity(missing.len());
        for (digest, data) in missing {
            let offset = self.pack.stream_position()?;
            self.pack.write_all(data)?;
            locations.push((digest, offset, data.len()));
        }
        PACK_APPEND_NS.fetch_add(nanos(append_started), Ordering::Relaxed);
        publication_fault!(AfterAppend);
        if !locations.is_empty() {
            timed_sync(&self.pack, &FILE_SYNCS, &FILE_SYNC_NS)?;
        }
        publication_fault!(AfterSync);
        Ok(locations)
    }

    fn commit_group(
        &self,
        locations: &[([u8; 32], u64, usize)],
        events: &[PreparedEvent],
    ) -> Result<()> {
        let has_captures = events
            .iter()
            .any(|event| matches!(event, PreparedEvent::Complete { .. }));
        if locations.is_empty() && !has_captures {
            return Ok(());
        }
        self.store
            .conn
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(sqlite_error)?;
        let persisted = (|| -> Result<()> {
            for (digest, offset, size) in locations {
                self.store
                    .conn
                    .execute(
                        "INSERT OR IGNORE INTO chunk_locations (digest, offset, size)
                         VALUES (?1, ?2, ?3)",
                        rusqlite::params![
                            digest.as_slice(),
                            i64::try_from(*offset).map_err(|_| BulkloadRefusal::BudgetExceeded)?,
                            i64::try_from(*size).map_err(|_| BulkloadRefusal::BudgetExceeded)?,
                        ],
                    )
                    .map_err(sqlite_error)?;
            }
            publication_fault!(AfterLocationInsert);
            for event in events {
                if let PreparedEvent::Complete { key, manifest, .. } = event {
                    self.store
                        .conn
                        .execute(
                            "INSERT INTO captures VALUES (?1, ?2)
                             ON CONFLICT(key) DO UPDATE SET manifest=excluded.manifest",
                            (key, postcard::to_stdvec(manifest)?),
                        )
                        .map_err(sqlite_error)?;
                }
            }
            publication_fault!(AfterManifestInsert);
            Ok(())
        })();
        if let Err(error) = persisted {
            let _ = self.store.conn.execute_batch("ROLLBACK");
            return Err(error);
        }
        SQLITE_COMMITS.fetch_add(1, Ordering::Relaxed);
        let commit_started = Instant::now();
        #[cfg(test)]
        let committed = inject_fault(PublishFault::BeforeCommit).and_then(|()| {
            self.store
                .conn
                .execute_batch("COMMIT")
                .map_err(sqlite_error)
        });
        #[cfg(not(test))]
        let committed = self
            .store
            .conn
            .execute_batch("COMMIT")
            .map_err(sqlite_error);
        SQLITE_COMMIT_NS.fetch_add(nanos(commit_started), Ordering::Relaxed);
        if let Err(error) = committed {
            let _ = self.store.conn.execute_batch("ROLLBACK");
            return Err(error);
        }
        Ok(())
    }

    fn acknowledgements(events: Vec<PreparedEvent>) -> Vec<PublishAck> {
        events
            .into_iter()
            .filter_map(|event| match event {
                PreparedEvent::Complete {
                    capture_id,
                    bytes_read,
                    manifest,
                    ..
                } => Some(PublishAck {
                    capture_id,
                    bytes_read,
                    captured: Ok(manifest),
                }),
                PreparedEvent::Refused {
                    capture_id,
                    bytes_read,
                    refusal,
                } => Some(PublishAck {
                    capture_id,
                    bytes_read,
                    captured: Err(refusal),
                }),
                PreparedEvent::Chunks { capture_id, .. } => {
                    let _ = capture_id;
                    None
                }
            })
            .collect()
    }

    /// Append, sync, and then atomically expose chunk locations and captures.
    pub(crate) fn publish_group(&mut self, events: Vec<PreparedEvent>) -> Result<Vec<PublishAck>> {
        if events.is_empty() {
            return Ok(Vec::new());
        }
        PUBLISH_GROUPS.fetch_add(1, Ordering::Relaxed);
        let missing = self.missing_chunks(&events)?;
        let locations = self.append_chunks(missing)?;
        self.commit_group(&locations, &events)?;
        Ok(Self::acknowledgements(events))
    }
}

#[cfg(test)]
fn persist_batch_with<F>(chunks: &[([u8; 32], Vec<u8>)], persist: &F) -> Result<()>
where
    F: Fn(&[u8; 32], &[u8]) -> Result<()> + Sync,
{
    if chunks.len() > PERSIST_BATCH {
        return Err(BulkloadRefusal::BudgetExceeded);
    }
    if chunks
        .iter()
        .any(|(_, data)| data.len() > crate::hash::CDC_MAX_BYTES as usize)
    {
        return Err(BulkloadRefusal::DigestMismatch);
    }
    if chunks.is_empty() {
        return Ok(());
    }
    std::thread::scope(|scope| {
        let mut workers = Vec::new();
        for group in chunks.chunks(chunks.len().div_ceil(PERSIST_WORKERS)) {
            workers.push(std::thread::Builder::new().spawn_scoped(scope, move || {
                for (digest, data) in group {
                    persist(digest, data)?;
                }
                Ok(())
            })?);
        }
        let mut outcome = Ok(());
        for worker in workers {
            let result = worker
                .join()
                .map_err(|_| BulkloadRefusal::Io(None))
                .and_then(|result| result);
            // Join even after an earlier refusal; no background writes escape.
            if outcome.is_ok() {
                outcome = result;
            }
        }
        outcome
    })
}

/// Bind a row to its source root identity and destination namespace.
///
/// # Errors
/// Refuses serialization failure.
pub fn row_key(authority: &[u8], row: &RowSchema) -> Result<Vec<u8>> {
    Ok(postcard::to_stdvec(&(authority, row))?)
}

fn identity_bytes(identity: &StatIdentity) -> Result<Vec<u8>> {
    Ok(postcard::to_stdvec(&(
        identity.dev,
        identity.ino,
        identity.size,
        identity.mtime_ns,
        identity.ctime_ns,
    ))?)
}

fn private_dir(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() && meta.permissions().mode().trailing_zeros() >= 6 => Ok(()),
        Ok(_) => Err(BulkloadRefusal::PathEscapesRoot),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            use std::os::unix::fs::DirBuilderExt as _;
            fs::DirBuilder::new().mode(0o700).create(path)?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

fn hex(digest: &[u8; 32]) -> String {
    use std::fmt::Write as _;
    digest.iter().fold(String::new(), |mut value, byte| {
        let _ = write!(value, "{byte:02x}");
        value
    })
}

fn sqlite_error(_: rusqlite::Error) -> BulkloadRefusal {
    BulkloadRefusal::SqliteIntegrityCheckFailed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct TestRoot(PathBuf);

    impl TestRoot {
        fn new() -> Result<Self> {
            let path = std::env::temp_dir().join(format!(
                "tcfs-transfer-store-{}-{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path)?;
            Ok(Self(path))
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn publication(data: &[u8]) -> (Vec<PreparedEvent>, Manifest) {
        let digest = crate::hash::hash_bytes(data);
        let manifest = Manifest {
            digest,
            chunks: vec![
                ChunkSpec {
                    digest,
                    size: data.len() as u64,
                },
                ChunkSpec {
                    digest,
                    size: data.len() as u64,
                },
            ],
        };
        (
            vec![
                PreparedEvent::Chunks {
                    capture_id: 7,
                    chunks: vec![(digest, data.to_vec()), (digest, data.to_vec())],
                },
                PreparedEvent::Complete {
                    capture_id: 7,
                    bytes_read: data.len() as u64,
                    key: b"capture".to_vec(),
                    manifest: manifest.clone(),
                },
            ],
            manifest,
        )
    }

    #[test]
    fn batch_joins_success_and_refusal_before_returning() -> Result<()> {
        let chunks = vec![([0; 32], vec![0]), ([1; 32], vec![1])];
        let finished = AtomicUsize::new(0);
        persist_batch_with(&chunks, &|_, _| {
            finished.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })?;
        assert_eq!(finished.load(Ordering::SeqCst), 2);
        finished.store(0, Ordering::SeqCst);
        let failed = persist_batch_with(&chunks, &|digest, _| {
            if digest.first() == Some(&0) {
                return Err(BulkloadRefusal::DigestMismatch);
            }
            finished.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        assert_eq!(failed, Err(BulkloadRefusal::DigestMismatch));
        // The second worker finishes even if the first worker failed first.
        assert_eq!(finished.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[test]
    fn group_deduplicates_chunks_and_preserves_manifest_order() -> Result<()> {
        let root = TestRoot::new()?;
        let store = Store::open(&root.0.join("state"))?;
        let first = b"first persisted chunk".to_vec();
        let second = b"second persisted chunk".to_vec();
        let first_digest = crate::hash::hash_bytes(&first);
        let second_digest = crate::hash::hash_bytes(&second);
        let manifest = Manifest {
            digest: crate::hash::hash_bytes(b"capture"),
            chunks: vec![
                ChunkSpec {
                    digest: first_digest,
                    size: first.len() as u64,
                },
                ChunkSpec {
                    digest: first_digest,
                    size: first.len() as u64,
                },
                ChunkSpec {
                    digest: second_digest,
                    size: second.len() as u64,
                },
            ],
        };
        let mut publisher = store.publisher()?;
        let acknowledgements = publisher.publish_group(vec![
            PreparedEvent::Chunks {
                capture_id: 1,
                chunks: vec![(first_digest, first.clone()), (first_digest, first.clone())],
            },
            PreparedEvent::Chunks {
                capture_id: 1,
                chunks: vec![
                    (first_digest, first.clone()),
                    (second_digest, second.clone()),
                ],
            },
            PreparedEvent::Complete {
                capture_id: 1,
                bytes_read: 0,
                key: b"capture".to_vec(),
                manifest,
            },
        ])?;
        assert_eq!(acknowledgements.len(), 1);
        drop(publisher);
        drop(store);

        let reopened = Store::open(&root.0.join("state"))?;
        let captured = reopened
            .capture(b"capture")?
            .ok_or(BulkloadRefusal::SealedObjectMissing)?;
        assert_eq!(captured.chunks.len(), 3);
        assert_eq!(
            captured.chunks.first().map(|chunk| chunk.digest),
            Some(first_digest)
        );
        assert_eq!(
            captured.chunks.get(1).map(|chunk| chunk.digest),
            Some(first_digest)
        );
        assert_eq!(
            captured.chunks.get(2).map(|chunk| chunk.digest),
            Some(second_digest)
        );
        assert_eq!(reopened.chunk(&first_digest)?, Some(first));
        assert_eq!(reopened.chunk(&second_digest)?, Some(second));
        assert_eq!(
            fs::metadata(reopened.root.join("chunks.pack"))?.len(),
            (b"first persisted chunk".len() + b"second persisted chunk".len()) as u64
        );
        Ok(())
    }

    #[test]
    fn publisher_is_exclusive_and_reconciles_unindexed_tail() -> Result<()> {
        let root = TestRoot::new()?;
        let state = root.0.join("state");
        let store = Store::open(&state)?;
        let publisher = store.publisher()?;
        let contender = Store::open(&state)?;
        assert!(contender.publisher().is_err());
        drop(publisher);
        drop(contender);
        drop(store);

        let mut pack = OpenOptions::new()
            .append(true)
            .open(state.join("chunks.pack"))?;
        pack.write_all(b"unindexed tail")?;
        pack.sync_all()?;
        drop(pack);
        let reopened = Store::open(&state)?;
        let reconciled = reopened.publisher()?;
        assert_eq!(fs::metadata(state.join("chunks.pack"))?.len(), 0);
        drop(reconciled);
        Ok(())
    }

    #[test]
    fn publisher_refuses_indexed_ranges_beyond_pack() -> Result<()> {
        let root = TestRoot::new()?;
        let state = root.0.join("state");
        let store = Store::open(&state)?;
        let (events, _) = publication(b"indexed content");
        let mut publisher = store.publisher()?;
        publisher.publish_group(events)?;
        drop(publisher);
        drop(store);
        OpenOptions::new()
            .write(true)
            .open(state.join("chunks.pack"))?
            .set_len(0)?;
        let reopened = Store::open(&state)?;
        assert!(matches!(
            reopened.publisher(),
            Err(BulkloadRefusal::DigestMismatch)
        ));
        Ok(())
    }

    #[test]
    fn interrupted_publication_hides_manifest_and_retries_cleanly() -> Result<()> {
        for fault in [
            PublishFault::AfterAppend,
            PublishFault::AfterSync,
            PublishFault::AfterLocationInsert,
            PublishFault::AfterManifestInsert,
            PublishFault::BeforeCommit,
        ] {
            let root = TestRoot::new()?;
            let state = root.0.join("state");
            let store = Store::open(&state)?;
            let (events, manifest) = publication(b"fault recovery content");
            let mut publisher = store.publisher()?;
            PUBLISH_FAULT.with(|active| active.set(fault));
            assert!(matches!(
                publisher.publish_group(events),
                Err(BulkloadRefusal::Io(None))
            ));
            PUBLISH_FAULT.with(|active| active.set(PublishFault::None));
            assert!(store.capture(b"capture")?.is_none());
            drop(publisher);
            drop(store);

            let reopened = Store::open(&state)?;
            let mut publisher = reopened.publisher()?;
            assert_eq!(fs::metadata(state.join("chunks.pack"))?.len(), 0);
            let (retry, _) = publication(b"fault recovery content");
            let acknowledgements = publisher.publish_group(retry)?;
            assert_eq!(acknowledgements.len(), 1);
            drop(publisher);
            assert_eq!(
                reopened
                    .capture(b"capture")?
                    .ok_or(BulkloadRefusal::SealedObjectMissing)?
                    .digest,
                manifest.digest
            );
        }
        Ok(())
    }

    #[test]
    fn oversized_batch_refuses_before_any_write() {
        let chunks = vec![([0; 32], Vec::new()); PERSIST_BATCH + 1];
        let calls = AtomicUsize::new(0);
        let result = persist_batch_with(&chunks, &|_, _| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        assert_eq!(result, Err(BulkloadRefusal::BudgetExceeded));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let oversized = vec![([0; 32], vec![0; crate::hash::CDC_MAX_BYTES as usize + 1])];
        assert_eq!(
            persist_batch_with(&oversized, &|_, _| {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }),
            Err(BulkloadRefusal::DigestMismatch)
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
