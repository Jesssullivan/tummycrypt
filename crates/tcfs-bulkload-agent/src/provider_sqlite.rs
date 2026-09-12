//! Standalone provider snapshots using `SQLite`'s online backup API.
//!
//! This module never installs a snapshot over a live database or merges rows.
//! All tables, including unknown tables and recovery orphans, are preserved.

use std::fs::{self, OpenOptions};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::Path;
use std::time::Duration;

use rusqlite::backup::{Backup, StepResult};
use rusqlite::{Connection, OpenFlags};

use crate::{BulkloadRefusal, Result};

/// Capture a standalone database in an existing private directory.
///
/// `max_steps` bounds retries and total backup steps, including restarts caused
/// by concurrent writers. Each step copies at most 128 pages; lock contention
/// returns immediately. The caller may retry with a *new* output path.
///
/// The source is opened read-only using normal WAL-aware `SQLite` access, never
/// immutable mode. The output must not exist. On failure an incomplete private
/// output may remain; only a successful return authorizes its use as a snapshot.
/// Neither failure nor success removes source data.
///
/// # Errors
/// Refuses non-private output directories, existing outputs, exhausted step
/// budgets, `SQLite` errors, and failed database or foreign-key integrity checks.
pub fn snapshot(source: &Path, output: &Path, max_steps: u32) -> Result<()> {
    if max_steps == 0 {
        return Err(BulkloadRefusal::BudgetExceeded);
    }
    let parent = output.parent().ok_or(BulkloadRefusal::Io(None))?;
    let metadata = fs::metadata(parent).map_err(|_| BulkloadRefusal::Io(None))?;
    if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
        return Err(BulkloadRefusal::Io(None));
    }
    let source = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| BulkloadRefusal::Io(None))?;
    source
        .busy_timeout(Duration::ZERO)
        .map_err(|_| BulkloadRefusal::Io(None))?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(output)
        .map_err(|_| BulkloadRefusal::Io(None))?;
    let mut destination = Connection::open_with_flags(output, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .map_err(|_| BulkloadRefusal::Io(None))?;
    destination
        .busy_timeout(Duration::ZERO)
        .map_err(|_| BulkloadRefusal::Io(None))?;
    {
        let backup =
            Backup::new(&source, &mut destination).map_err(|_| BulkloadRefusal::Io(None))?;
        let mut complete = false;
        for _ in 0..max_steps {
            match backup.step(128).map_err(|_| BulkloadRefusal::Io(None))? {
                StepResult::Done => {
                    complete = true;
                    break;
                }
                StepResult::More => {}
                // Do not spin or sleep while another process holds a lock.
                _ => return Err(BulkloadRefusal::SqliteStateChanged),
            }
        }
        if !complete {
            return Err(BulkloadRefusal::BudgetExceeded);
        }
    }
    // A WAL source must produce one portable database, without relying on
    // destination sidecars. Changing the mode affects only the new snapshot.
    let mode: String = destination
        .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
        .map_err(|_| BulkloadRefusal::Io(None))?;
    if mode != "delete" {
        return Err(BulkloadRefusal::SqliteStateChanged);
    }
    let check: String = destination
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .map_err(|_| BulkloadRefusal::SqliteIntegrityCheckFailed)?;
    if check != "ok" {
        return Err(BulkloadRefusal::SqliteIntegrityCheckFailed);
    }
    let mut statement = destination
        .prepare("PRAGMA foreign_key_check")
        .map_err(|_| BulkloadRefusal::SqliteIntegrityCheckFailed)?;
    if statement
        .query([])
        .and_then(|mut rows| rows.next().map(|row| row.is_some()))
        .map_err(|_| BulkloadRefusal::SqliteIntegrityCheckFailed)?
    {
        return Err(BulkloadRefusal::SqliteIntegrityCheckFailed);
    }
    file.sync_all().map_err(|_| BulkloadRefusal::Io(None))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn captures_live_wal_and_orphans_without_overwriting(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "tcfs-provider-snapshot-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir)?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        let source_path = dir.join("source.sqlite");
        let output = dir.join("snapshot.sqlite");
        let source = Connection::open(&source_path)?;
        source.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE lost_and_found(payload BLOB); INSERT INTO lost_and_found VALUES (x'010203');")?;
        assert!(snapshot(&source_path, &output, 0).is_err());
        assert!(!output.exists());
        assert!(snapshot(&source_path, &output, 100).is_ok());
        assert_eq!(fs::metadata(&output)?.permissions().mode() & 0o777, 0o600);
        let captured = Connection::open_with_flags(&output, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let value: Vec<u8> =
            captured.query_row("SELECT payload FROM lost_and_found", [], |row| row.get(0))?;
        assert_eq!(value, vec![1, 2, 3]);
        source.execute("INSERT INTO lost_and_found VALUES (x'04')", [])?;
        assert!(snapshot(&source_path, &output, 100).is_err());
        let count: i64 =
            captured.query_row("SELECT count(*) FROM lost_and_found", [], |row| row.get(0))?;
        assert_eq!(count, 1);
        assert!(!dir.join("snapshot.sqlite-wal").exists());
        source.execute_batch("PRAGMA foreign_keys=OFF; CREATE TABLE parent(id INTEGER PRIMARY KEY); CREATE TABLE child(parent_id INTEGER REFERENCES parent(id)); INSERT INTO child VALUES(88);")?;
        let invalid = dir.join("invalid.sqlite");
        assert!(matches!(
            snapshot(&source_path, &invalid, 100),
            Err(BulkloadRefusal::SqliteIntegrityCheckFailed)
        ));
        drop(captured);
        drop(source);
        fs::remove_file(output)?;
        fs::remove_file(invalid)?;
        fs::remove_file(source_path)?;
        fs::remove_dir(dir)?;
        Ok(())
    }
}
