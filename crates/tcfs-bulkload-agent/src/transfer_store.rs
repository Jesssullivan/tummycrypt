//! Private, durable chunk storage and completion records for native transfers.

use std::fs::{self, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::OptionalExtension as _;
use serde::{Deserialize, Serialize};
use tcfs_bulkload_proto::frame::ChunkSpec;

use crate::freshness::StatIdentity;
use crate::{BulkloadRefusal, Result, RowSchema};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

/// Completed content capture; chunks retain file order, including repetitions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// Digest of the complete file.
    pub digest: [u8; 32],
    /// Content-defined chunks in order.
    pub chunks: Vec<ChunkSpec>,
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
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(sqlite_error)?;
        conn.execute_batch(
            "PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS captures (key BLOB PRIMARY KEY, manifest BLOB NOT NULL);
            CREATE TABLE IF NOT EXISTS outputs (key BLOB PRIMARY KEY, identity BLOB NOT NULL);
            CREATE TABLE IF NOT EXISTS directories (key BLOB PRIMARY KEY, identity BLOB NOT NULL);
            CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value BLOB NOT NULL);",
        )
        .map_err(sqlite_error)?;
        Ok(Self {
            root: fs::canonicalize(root)?,
            conn,
        })
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
        let path = self.root.join("chunks").join(hex(digest));
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
        if data.len() > crate::hash::CDC_MAX_BYTES as usize
            || crate::hash::hash_bytes(data) != *digest
        {
            return Err(BulkloadRefusal::DigestMismatch);
        }
        if self.chunk(digest)?.is_some() {
            return Ok(());
        }
        let chunks = self.root.join("chunks");
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
            file.sync_all()?;
            let target = chunks.join(hex(digest));
            match fs::hard_link(&staging, &target) {
                Ok(()) => (),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    self.chunk(digest)?.ok_or(BulkloadRefusal::DigestMismatch)?;
                }
                Err(error) => return Err(error.into()),
            }
            Ok(())
        })();
        fs::remove_file(staging)?;
        fs::File::open(chunks)?.sync_all()?;
        result
    }
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
