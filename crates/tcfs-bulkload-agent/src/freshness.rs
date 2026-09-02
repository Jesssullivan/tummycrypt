//! The freshness-cache seam.
//!
//! The walker never names a concrete cache. It takes anything implementing
//! [`FreshnessCache`], keyed on a [`StatIdentity`] -- the
//! `(dev, ino, size, mtime_ns, ctime_ns)` tuple that decides whether a seat
//! may be skipped on resume.
//!
//! Why a local trait: `tcfs-sync` grows its own `freshness.rs` (PR #586), but
//! the agent half must not depend on `tcfs-sync` -- that crate pulls tokio and
//! the whole sync stack straight through the R34 dependency wall. The trait
//! keeps M3 unblocked: when #586 lands, an adapter in the *daemon* half
//! implements this trait over it, and nothing in the agent changes.
//!
//! Both R25 headline metrics fall out of this seam. A cache that reports
//! `Fresh` for a seat whose bytes did not change is what drives
//! `bytes_reread_on_resume` to zero; a cache consulted once per seat is what
//! drives `files_statted_twice` to zero.

use std::collections::HashMap;

use tcfs_bulkload_proto::{BulkloadRefusal, Result, RowSchema};

/// The identity tuple a freshness decision is keyed on.
///
/// Deliberately *not* a content hash: the point of the cache is to avoid
/// reading bytes at all. `ctime_ns` is carried alongside `mtime_ns` because
/// mtime alone is forgeable by a restore tool and does not move when only
/// ownership or mode changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StatIdentity {
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
}

impl StatIdentity {
    /// Read the identity a row asserts.
    #[must_use]
    pub const fn from_row(row: &RowSchema) -> Self {
        Self {
            dev: row.dev,
            ino: row.ino,
            size: row.size,
            mtime_ns: row.mtime_ns,
            ctime_ns: row.ctime_ns,
        }
    }
}

/// What the cache says about a seat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    /// Seen before with this exact identity: the bytes need not be re-read.
    Fresh,
    /// Never seen, or seen with a different identity: the bytes must be read.
    Stale,
}

/// A cache of previously observed [`StatIdentity`] values.
///
/// Implementations must be cheap enough to consult once per seat on a walk of
/// millions of files, and must never panic: a cache that cannot answer refuses
/// with a [`BulkloadRefusal`] and the walker treats the seat as
/// [`Freshness::Stale`].
pub trait FreshnessCache {
    /// Ask whether `identity` has been seen before.
    ///
    /// # Errors
    ///
    /// Refuses if the backing store cannot be consulted.
    fn lookup(&self, identity: &StatIdentity) -> Result<Freshness>;

    /// Record `identity` as observed.
    ///
    /// # Errors
    ///
    /// Refuses if the backing store cannot be written.
    fn record(&mut self, identity: &StatIdentity) -> Result<()>;
}

/// A cache that remembers nothing: every seat is [`Freshness::Stale`].
///
/// This is the M0 bench's cold-walk baseline -- the "always re-read everything"
/// arm that `bytes_reread_on_resume` is measured against.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullCache;

impl FreshnessCache for NullCache {
    fn lookup(&self, _identity: &StatIdentity) -> Result<Freshness> {
        Ok(Freshness::Stale)
    }

    fn record(&mut self, _identity: &StatIdentity) -> Result<()> {
        Ok(())
    }
}

/// An in-memory cache. Useful for tests and single-shot runs.
#[derive(Debug, Default, Clone)]
pub struct MemoryCache {
    seen: HashMap<(u64, u64), StatIdentity>,
}

impl MemoryCache {
    /// An empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many distinct inodes this cache remembers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    /// Whether this cache remembers nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

impl FreshnessCache for MemoryCache {
    fn lookup(&self, identity: &StatIdentity) -> Result<Freshness> {
        match self.seen.get(&(identity.dev, identity.ino)) {
            Some(prev) if prev == identity => Ok(Freshness::Fresh),
            _ => Ok(Freshness::Stale),
        }
    }

    fn record(&mut self, identity: &StatIdentity) -> Result<()> {
        self.seen.insert((identity.dev, identity.ino), *identity);
        Ok(())
    }
}

/// A cache persisted in `SQLite`.
///
/// M1 ships the schema and the round-trip only; the M3 lane wires this to the
/// on-disk agent state directory and adds the eviction policy.
pub struct SqliteCache {
    conn: rusqlite::Connection,
}

impl std::fmt::Debug for SqliteCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteCache").finish_non_exhaustive()
    }
}

impl SqliteCache {
    const SCHEMA: &'static str = "CREATE TABLE IF NOT EXISTS freshness (
            dev       INTEGER NOT NULL,
            ino       INTEGER NOT NULL,
            size      INTEGER NOT NULL,
            mtime_ns  INTEGER NOT NULL,
            ctime_ns  INTEGER NOT NULL,
            PRIMARY KEY (dev, ino)
        ) WITHOUT ROWID";

    /// Open an ephemeral in-memory cache.
    ///
    /// # Errors
    ///
    /// Refuses if `SQLite` declines to open or to create the schema.
    pub fn open_in_memory() -> Result<Self> {
        let conn =
            rusqlite::Connection::open_in_memory().map_err(|_| BulkloadRefusal::Io(None))?;
        Self::from_connection(conn)
    }

    /// Open a cache backed by `path`, creating it if absent.
    ///
    /// # Errors
    ///
    /// Refuses if `SQLite` declines to open the file or to create the schema.
    pub fn open(path: &std::path::Path) -> Result<Self> {
        let conn = rusqlite::Connection::open(path).map_err(|_| BulkloadRefusal::Io(None))?;
        Self::from_connection(conn)
    }

    fn from_connection(conn: rusqlite::Connection) -> Result<Self> {
        conn.execute_batch(Self::SCHEMA)
            .map_err(|_| BulkloadRefusal::SqliteIntegrityCheckFailed)?;
        Ok(Self { conn })
    }

    /// `SQLite` stores signed 64-bit integers; nanosecond timestamps wider than
    /// that are a value this schema cannot carry, not a rounding opportunity.
    fn narrow(value: i128) -> Result<i64> {
        i64::try_from(value).map_err(|_| BulkloadRefusal::SqliteUnsupportedValue)
    }
}

impl FreshnessCache for SqliteCache {
    fn lookup(&self, identity: &StatIdentity) -> Result<Freshness> {
        let dev = Self::narrow(i128::from(identity.dev))?;
        let ino = Self::narrow(i128::from(identity.ino))?;
        let found: std::result::Result<(i64, i64, i64), rusqlite::Error> = self.conn.query_row(
            "SELECT size, mtime_ns, ctime_ns FROM freshness WHERE dev = ?1 AND ino = ?2",
            (dev, ino),
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        );
        match found {
            Ok((size, mtime_ns, ctime_ns)) => {
                let same = u64::try_from(size).is_ok_and(|s| s == identity.size)
                    && i128::from(mtime_ns) == identity.mtime_ns
                    && i128::from(ctime_ns) == identity.ctime_ns;
                Ok(if same {
                    Freshness::Fresh
                } else {
                    Freshness::Stale
                })
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(Freshness::Stale),
            Err(_) => Err(BulkloadRefusal::SqliteIntegrityCheckFailed),
        }
    }

    fn record(&mut self, identity: &StatIdentity) -> Result<()> {
        let dev = Self::narrow(i128::from(identity.dev))?;
        let ino = Self::narrow(i128::from(identity.ino))?;
        let size = Self::narrow(i128::from(identity.size))?;
        let mtime_ns = Self::narrow(identity.mtime_ns)?;
        let ctime_ns = Self::narrow(identity.ctime_ns)?;
        self.conn
            .execute(
                "INSERT INTO freshness (dev, ino, size, mtime_ns, ctime_ns)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(dev, ino) DO UPDATE SET
                     size = excluded.size,
                     mtime_ns = excluded.mtime_ns,
                     ctime_ns = excluded.ctime_ns",
                (dev, ino, size, mtime_ns, ctime_ns),
            )
            .map_err(|_| BulkloadRefusal::SqliteIntegrityCheckFailed)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::{Freshness, FreshnessCache, MemoryCache, NullCache, SqliteCache, StatIdentity};

    fn identity() -> StatIdentity {
        StatIdentity {
            dev: 16_777_220,
            ino: 42,
            size: 4096,
            mtime_ns: 1_756_000_000_000_000_000,
            ctime_ns: 1_756_000_000_000_000_001,
        }
    }

    fn exercises_contract<C: FreshnessCache>(mut cache: C, remembers: bool) {
        let id = identity();
        assert_eq!(cache.lookup(&id).unwrap(), Freshness::Stale);
        cache.record(&id).unwrap();
        let expected = if remembers {
            Freshness::Fresh
        } else {
            Freshness::Stale
        };
        assert_eq!(cache.lookup(&id).unwrap(), expected);

        // A seat whose mtime moved is stale even at the same (dev, ino).
        let touched = StatIdentity {
            mtime_ns: id.mtime_ns + 1,
            ..id
        };
        assert_eq!(cache.lookup(&touched).unwrap(), Freshness::Stale);
    }

    #[test]
    fn null_cache_never_remembers() {
        exercises_contract(NullCache, false);
    }

    #[test]
    fn memory_cache_remembers() {
        exercises_contract(MemoryCache::new(), true);
    }

    #[test]
    fn sqlite_cache_remembers() {
        exercises_contract(SqliteCache::open_in_memory().unwrap(), true);
    }

    #[test]
    fn sqlite_cache_updates_in_place() {
        let mut cache = SqliteCache::open_in_memory().unwrap();
        let id = identity();
        cache.record(&id).unwrap();
        let touched = StatIdentity {
            mtime_ns: id.mtime_ns + 1,
            ..id
        };
        cache.record(&touched).unwrap();
        assert_eq!(cache.lookup(&touched).unwrap(), Freshness::Fresh);
        assert_eq!(cache.lookup(&id).unwrap(), Freshness::Stale);
    }
}
