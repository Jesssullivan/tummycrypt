//! Optional external bulkload import; this is not a provider-supported API.
//!
//! Only a reviewed immutable candidate is accepted. No schema, migration or
//! control-table changes are made. Callers must retain both input snapshots.
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;
use std::time::Duration;

use rusqlite::types::Value;
use rusqlite::{params_from_iter, Connection, OpenFlags, TransactionBehavior};

use super::{columns, encode_row, identifier, native_insert_is_safe, sql_refusal};
use crate::{BulkloadRefusal, Result};

/// One bounded unit committed without changing any unrelated live row.
#[derive(Debug)]
pub struct Applied {
    pub table: String,
    pub inserted: usize,
    pub corrected: usize,
    pub conflicts: usize,
}

/// Apply only new rows and guarded path corrections from an offline candidate.
///
/// Each row gets its own immediate transaction with zero busy timeout. Existing
/// unequal live rows win. Changed schema/migration authority stops the operation.
/// Receipts are emitted after each commit, so a later refusal cannot hide earlier
/// committed units. A failed receipt must be reconciled from retained snapshots.
/// A private sibling `*.bulkload-import.sqlite` journal durably records insertion
/// intents before live commits. Never remove it to retry: an absent attempted ID
/// is a conflict, including ambiguous crashes and later operator deletions.
/// # Errors
/// Refuses schema drift, unknown triggers, absent files, busy writers and budgets.
pub fn apply_state_candidate(
    live: &Path,
    base: &Path,
    candidate: &Path,
    max_rows: usize,
    receipt: &impl Fn(&Applied) -> Result<()>,
) -> Result<()> {
    if max_rows == 0 || live == base || live == candidate {
        return Err(BulkloadRefusal::BudgetExceeded);
    }
    let identity = |path: &Path| {
        std::fs::metadata(path)
            .map(|metadata| (metadata.dev(), metadata.ino()))
            .map_err(|_| BulkloadRefusal::Io(None))
    };
    if identity(live)? == identity(base)? || identity(live)? == identity(candidate)? {
        return Err(BulkloadRefusal::SqliteStateChanged);
    }
    let base = retained_input(base)?;
    let candidate = retained_input(candidate)?;
    let expected_authority = authority(&base)?;
    let mut destination = Connection::open_with_flags(live, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .map_err(sql_refusal)?;
    let journal = intent_journal(live)?;
    destination
        .busy_timeout(Duration::ZERO)
        .map_err(sql_refusal)?;
    destination
        .execute_batch("PRAGMA foreign_keys=ON; PRAGMA recursive_triggers=ON;")
        .map_err(sql_refusal)?;
    let mut units = 0usize;
    for table in [
        "projects",
        "thread_sections",
        "project_roots",
        "threads",
        "thread_dynamic_tools",
        "thread_artifacts",
        "thread_spawn_edges",
    ] {
        let schema = columns(&base, table)?;
        if schema != columns(&candidate, table)? {
            return Err(BulkloadRefusal::SqliteStateChanged);
        }
        if schema.is_empty() {
            continue;
        }
        let keys: Vec<_> = schema
            .iter()
            .enumerate()
            .filter(|(_, column)| column.3 > 0)
            .map(|(index, _)| index)
            .collect();
        if keys.is_empty() {
            return Err(BulkloadRefusal::SqliteUnsupportedValue);
        }
        let query = format!("SELECT * FROM {}", identifier(table));
        let mut statement = candidate.prepare(&query).map_err(sql_refusal)?;
        let mut rows = statement.query([]).map_err(sql_refusal)?;
        while let Some(row) = rows.next().map_err(sql_refusal)? {
            let values = (0..schema.len())
                .map(|index| row.get::<_, Value>(index))
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(sql_refusal)?;
            let predicate = keys
                .iter()
                .filter_map(|index| schema.get(*index))
                .map(|column| format!("{} IS ?", identifier(&column.0)))
                .collect::<Vec<_>>()
                .join(" AND ");
            let lookup = format!("SELECT * FROM {} WHERE {predicate}", identifier(table));
            let key_values: Vec<_> = keys.iter().filter_map(|index| values.get(*index)).collect();
            let original = lookup_row(&base, &lookup, &key_values, schema.len())?;
            if original.as_ref() == Some(&values) {
                continue;
            }
            units += 1;
            if units > max_rows {
                return Err(BulkloadRefusal::BudgetExceeded);
            }
            if table == "threads" {
                let rollout = schema
                    .iter()
                    .position(|column| column.0 == "rollout_path")
                    .ok_or(BulkloadRefusal::SqliteUnsupportedValue)?;
                if !matches!(values.get(rollout),Some(Value::Text(path)) if Path::new(path).is_absolute() && Path::new(path).is_file() && Path::new(path).extension()==Some(std::ffi::OsStr::new("jsonl")))
                {
                    return Err(BulkloadRefusal::SqliteStateChanged);
                }
            }
            let unit = Unit {
                table,
                schema: &schema,
                values: &values,
                original,
                keys: &key_values,
                predicate: &predicate,
                lookup: &lookup,
            };
            let applied = apply_unit(&mut destination, &journal, &expected_authority, &unit)?;
            receipt(&applied)?;
        }
    }
    Ok(())
}

fn retained_input(path: &Path) -> Result<Connection> {
    let connection =
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(sql_refusal)?;
    connection.execute_batch("BEGIN").map_err(sql_refusal)?;
    Ok(connection)
}

struct Unit<'a> {
    table: &'a str,
    schema: &'a [(String, String, i64, i64)],
    values: &'a [Value],
    original: Option<Vec<Value>>,
    keys: &'a [&'a Value],
    predicate: &'a str,
    lookup: &'a str,
}

fn apply_unit(
    destination: &mut Connection,
    journal: &Connection,
    expected: &[u8],
    unit: &Unit<'_>,
) -> Result<Applied> {
    let transaction = destination
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sql_refusal)?;
    if authority(&transaction)? != expected
        || !native_insert_is_safe(&transaction, unit.table, unit.schema)?
    {
        return Err(BulkloadRefusal::SqliteStateChanged);
    }
    let current = lookup_row(&transaction, unit.lookup, unit.keys, unit.schema.len())?;
    let mut applied = Applied {
        table: unit.table.to_owned(),
        inserted: 0,
        corrected: 0,
        conflicts: 0,
    };
    // Remember all observed base-absent identities, including rows another
    // writer already supplied. Later deletion must not enable resurrection.
    let fresh_intent = if unit.original.is_none() {
        let key_blob = encode_row(
            &unit
                .keys
                .iter()
                .map(|value| (*value).clone())
                .collect::<Vec<_>>(),
        );
        journal
            .execute(
                "INSERT OR IGNORE INTO attempts(table_name,key_blob) VALUES(?1,?2)",
                rusqlite::params![unit.table, key_blob],
            )
            .map_err(sql_refusal)?
            != 0
    } else {
        false
    };
    match (unit.original.as_ref(), current) {
        (None, None) => {
            if fresh_intent {
                let sql = format!(
                    "INSERT OR IGNORE INTO {} VALUES ({})",
                    identifier(unit.table),
                    vec!["?"; unit.schema.len()].join(",")
                );
                match transaction.execute(&sql, params_from_iter(unit.values)) {
                    Ok(count) => {
                        applied.inserted = count;
                        applied.conflicts = usize::from(count == 0);
                    }
                    Err(rusqlite::Error::SqliteFailure(error, _))
                        if error.code == rusqlite::ErrorCode::ConstraintViolation =>
                    {
                        applied.conflicts = 1;
                    }
                    Err(error) => return Err(sql_refusal(error)),
                }
            } else {
                applied.conflicts = 1;
            }
        }
        (Some(original), Some(current)) if unit.table == "threads" => {
            (applied.corrected, applied.conflicts) = correct_paths(
                &transaction,
                unit.schema,
                unit.predicate,
                unit.keys,
                original,
                &current,
                unit.values,
            )?;
        }
        (_, Some(current)) if current == unit.values => {}
        _ => applied.conflicts = 1,
    }
    transaction.commit().map_err(sql_refusal)?;
    Ok(applied)
}

fn intent_journal(live: &Path) -> Result<Connection> {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    let path = std::fs::canonicalize(live)
        .map_err(|_| BulkloadRefusal::Io(None))?
        .with_extension("bulkload-import.sqlite");
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
    {
        Ok(file) => file.sync_all().map_err(|_| BulkloadRefusal::Io(None))?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata =
                std::fs::symlink_metadata(&path).map_err(|_| BulkloadRefusal::Io(None))?;
            if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
                return Err(BulkloadRefusal::Io(None));
            }
        }
        Err(_) => return Err(BulkloadRefusal::Io(None)),
    }
    let connection = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .map_err(sql_refusal)?;
    connection
        .busy_timeout(Duration::ZERO)
        .map_err(sql_refusal)?;
    connection.execute_batch("PRAGMA synchronous=FULL; PRAGMA journal_mode=DELETE; CREATE TABLE IF NOT EXISTS attempts(table_name TEXT NOT NULL,key_blob BLOB NOT NULL,PRIMARY KEY(table_name,key_blob));").map_err(sql_refusal)?;
    std::fs::File::open(path.parent().ok_or(BulkloadRefusal::Io(None))?)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| BulkloadRefusal::Io(None))?;
    Ok(connection)
}

fn correct_paths(
    connection: &Connection,
    schema: &[(String, String, i64, i64)],
    predicate: &str,
    keys: &[&Value],
    original: &[Value],
    current: &[Value],
    values: &[Value],
) -> Result<(usize, usize)> {
    let indices: Vec<_> = schema
        .iter()
        .enumerate()
        .filter(|(_, column)| matches!(column.0.as_str(), "rollout_path" | "cwd"))
        .map(|(index, _)| index)
        .collect();
    if indices.len() != 2
        || original
            .iter()
            .zip(values)
            .enumerate()
            .any(|(index, (old, new))| !indices.contains(&index) && old != new)
    {
        return Err(BulkloadRefusal::SqliteStateChanged);
    }
    if indices
        .iter()
        .all(|index| current.get(*index) == values.get(*index))
    {
        return Ok((0, 0));
    }
    if !indices
        .iter()
        .all(|index| current.get(*index) == original.get(*index))
    {
        return Ok((0, 1));
    }
    let assignments = indices
        .iter()
        .filter_map(|index| schema.get(*index))
        .map(|column| format!("{}=?", identifier(&column.0)))
        .collect::<Vec<_>>()
        .join(",");
    let guards = indices
        .iter()
        .filter_map(|index| schema.get(*index))
        .map(|column| format!("{} IS ?", identifier(&column.0)))
        .collect::<Vec<_>>()
        .join(" AND ");
    let mut parameters: Vec<_> = indices
        .iter()
        .filter_map(|index| values.get(*index))
        .collect();
    parameters.extend(keys.iter().copied());
    parameters.extend(indices.iter().filter_map(|index| original.get(*index)));
    let changed = connection
        .execute(
            &format!("UPDATE threads SET {assignments} WHERE {predicate} AND {guards}"),
            params_from_iter(parameters),
        )
        .map_err(sql_refusal)?;
    Ok((changed, usize::from(changed == 0)))
}

fn lookup_row(
    connection: &Connection,
    sql: &str,
    keys: &[&Value],
    count: usize,
) -> Result<Option<Vec<Value>>> {
    let mut statement = connection.prepare(sql).map_err(sql_refusal)?;
    let mut rows = statement
        .query(params_from_iter(keys))
        .map_err(sql_refusal)?;
    rows.next()
        .map_err(sql_refusal)?
        .map(|row| {
            (0..count)
                .map(|index| row.get(index))
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(sql_refusal)
        })
        .transpose()
}

fn authority(connection: &Connection) -> Result<Vec<u8>> {
    let mut result = Vec::new();
    for sql in ["SELECT type,name,tbl_name,sql FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name", "SELECT * FROM _sqlx_migrations ORDER BY version", "PRAGMA user_version", "PRAGMA application_id"] {
        let mut statement=connection.prepare(sql).map_err(sql_refusal)?;
        let count=statement.column_count();
        let mut rows=statement.query([]).map_err(sql_refusal)?;
        while let Some(row)=rows.next().map_err(sql_refusal)? {
            let values=(0..count).map(|index|row.get(index)).collect::<std::result::Result<Vec<Value>,_>>().map_err(sql_refusal)?;
            let encoded=encode_row(&values);
            result.extend_from_slice(&(encoded.len() as u64).to_le_bytes()); result.extend(encoded);
        }
    }
    Ok(result)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    #[allow(clippy::too_many_lines)] // One shared lifecycle proves lock, retry and deletion semantics.
    fn busy_writer_refuses_readers_continue_and_live_fields_win() {
        let dir = std::env::temp_dir().join(format!("tcfs-online-test-{}", std::process::id()));
        std::fs::create_dir(&dir).expect("owned directory");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("private");
        let live = dir.join("live.sqlite");
        let base = dir.join("base.sqlite");
        let candidate = dir.join("candidate.sqlite");
        let raw = dir.join("thread.jsonl");
        std::fs::write(&raw, b"{}\n").expect("raw");
        let writer = Connection::open(&live).expect("live");
        writer.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE _sqlx_migrations(version INTEGER PRIMARY KEY,checksum BLOB); CREATE TABLE projects(id TEXT PRIMARY KEY); CREATE TABLE thread_sections(id TEXT PRIMARY KEY); CREATE TABLE project_roots(id TEXT PRIMARY KEY); CREATE TABLE threads(id TEXT PRIMARY KEY,rollout_path TEXT,cwd TEXT,title TEXT); INSERT INTO threads VALUES('old','/old/path','/old/cwd','original');").expect("schema");
        super::super::snapshot(&live, &base, 100).expect("base");
        super::super::snapshot(&base, &candidate, 100).expect("candidate");
        let proposed = Connection::open(&candidate).expect("candidate");
        proposed
            .execute(
                "UPDATE threads SET rollout_path=?1,cwd='/seat'",
                [raw.to_str().expect("path")],
            )
            .expect("path correction");
        proposed
            .execute(
                "INSERT INTO threads VALUES('new',?1,'/seat','new')",
                [raw.to_str().expect("path")],
            )
            .expect("new row");
        writer
            .execute_batch("BEGIN IMMEDIATE; UPDATE threads SET title='active writer';")
            .expect("active writer");
        assert!(apply_state_candidate(&live, &base, &candidate, 100, &|_| Ok(())).is_err());
        writer
            .execute_batch("COMMIT")
            .expect("writer finishes naturally");
        let reader = Connection::open(&live).expect("reader");
        reader.execute_batch("BEGIN").expect("reader snapshot");
        assert_eq!(
            reader
                .query_row("SELECT count(*) FROM threads", [], |row| row
                    .get::<_, u64>(0))
                .expect("count"),
            1
        );
        apply_state_candidate(&live, &base, &candidate, 100, &|_| Ok(()))
            .expect("online import alongside reader");
        assert_eq!(
            reader
                .query_row("SELECT count(*) FROM threads", [], |row| row
                    .get::<_, u64>(0))
                .expect("reader retains snapshot"),
            1
        );
        assert_eq!(
            writer
                .query_row("SELECT title FROM threads WHERE id='old'", [], |row| row
                    .get::<_, String>(
                    0
                ))
                .expect("title"),
            "active writer"
        );
        assert_eq!(
            writer
                .query_row("SELECT count(*) FROM threads", [], |row| row
                    .get::<_, u64>(0))
                .expect("new count"),
            2
        );
        apply_state_candidate(&live, &base, &candidate, 100, &|_| Ok(()))
            .expect("idempotent retry");
        writer
            .execute("DELETE FROM threads WHERE id='new'", [])
            .expect("operator deletion");
        apply_state_candidate(&live, &base, &candidate, 100, &|_| Ok(()))
            .expect("retry retains deletion");
        assert_eq!(
            writer
                .query_row("SELECT count(*) FROM threads WHERE id='new'", [], |row| row
                    .get::<_, u64>(0))
                .expect("not resurrected"),
            0
        );
        proposed
            .execute(
                "INSERT INTO threads VALUES('independent',?1,'/seat','independent')",
                [raw.to_str().expect("path")],
            )
            .expect("candidate independent row");
        writer
            .execute(
                "INSERT INTO threads VALUES('independent',?1,'/seat','independent')",
                [raw.to_str().expect("path")],
            )
            .expect("provider independent row");
        apply_state_candidate(&live, &base, &candidate, 100, &|_| Ok(()))
            .expect("observe independent row");
        writer
            .execute("DELETE FROM threads WHERE id='independent'", [])
            .expect("operator deletes independent row");
        apply_state_candidate(&live, &base, &candidate, 100, &|_| Ok(()))
            .expect("retry preserves independent deletion");
        assert_eq!(
            writer
                .query_row(
                    "SELECT count(*) FROM threads WHERE id='independent'",
                    [],
                    |row| row.get::<_, u64>(0)
                )
                .expect("not resurrected"),
            0
        );
        reader.execute_batch("COMMIT").expect("reader done");
        writer
            .execute_batch("ALTER TABLE threads ADD COLUMN extra TEXT;")
            .expect("schema drift");
        assert!(apply_state_candidate(&live, &base, &candidate, 100, &|_| Ok(())).is_err());
        drop(reader);
        drop(writer);
        drop(proposed);
        std::fs::remove_dir_all(&dir).expect("remove exact owned test directory");
    }
}
