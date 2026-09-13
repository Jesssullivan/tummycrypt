//! Standalone provider snapshots using `SQLite`'s online backup API.
//!
//! This module never installs a snapshot over a live database.
//! All tables, including unknown tables and recovery orphans, are preserved.

use std::fs::{self, OpenOptions};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::Path;
use std::time::Duration;

use rusqlite::backup::{Backup, StepResult};
use rusqlite::{Connection, OpenFlags};

use crate::{BulkloadRefusal, Result};

pub mod hydrate;

fn mapped_path(path: &Path, mapping: &PathMapping<'_>) -> std::path::PathBuf {
    if mapping.source_home == Path::new("/Users/jess") {
        if let Ok(relative) = path.strip_prefix("/Volumes/TinylandState/tinyland-state/codex") {
            return mapping.destination_home.join(".codex").join(relative);
        }
    }
    path.strip_prefix(mapping.source_home).map_or_else(
        |_| path.to_path_buf(),
        |relative| mapping.destination_home.join(relative),
    )
}

fn mapped_rollout_path(path: &Path, mapping: &PathMapping<'_>) -> std::path::PathBuf {
    let mapped = mapped_path(path, mapping);
    match mapped.to_str() {
        Some(value) if value.ends_with(".jsonl.gz") || value.ends_with(".jsonl.zst") => {
            mapped.with_extension("")
        }
        _ => mapped,
    }
}

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

/// Accounting for an offline candidate. Preserved rows require explicit
/// resolution before any future installation; they are never silently dropped.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Composition {
    /// Rows inserted in native provider tables.
    pub inserted: u64,
    /// Rows already equal, or already imported from this immutable source.
    pub equivalent: u64,
    /// Divergent, recovery, or unsupported rows retained with typed values.
    pub preserved: u64,
    /// All unresolved provenance rows in the candidate, including earlier runs.
    pub unresolved: u64,
    /// Existing candidate threads whose typed home paths were corrected.
    pub paths_corrected: u64,
    /// Existing source-prefix threads whose mapped rollout is unavailable.
    pub unavailable_rollouts: u64,
}

/// Compose retained snapshots into a new offline candidate.
///
/// Supports matching Codex history schemas and log schemas. History key or
/// secondary-index collisions preserve the incoming row separately. Log IDs
/// are host-local: incoming IDs are remapped and their original typed rows
/// retained with provenance. This intentionally does not deduplicate log
/// events across different sources. Recovery and unknown tables are retained
/// as typed rows, not interpreted or installed as provider state.
///
/// `source_id` must identify immutable snapshot bytes (prefer their digest).
/// Reusing it for different bytes is invalid. Neither input may change during
/// composition. No provider process needs to pause for this offline operation.
/// A returned candidate is never permission to replace a live database.
///
/// # Errors
/// Refuses invalid source IDs, `SQLite` errors, and invalid candidate integrity.
/// Failure leaves an incomplete private candidate, with both inputs untouched.
pub fn compose_snapshots(
    base: &Path,
    incoming: &Path,
    output: &Path,
    source_id: &str,
    max_steps: u32,
) -> Result<Composition> {
    compose(base, incoming, output, source_id, max_steps, None)
}

/// Typed home-path translation for importing missing provider threads.
pub struct PathMapping<'a> {
    /// Source home prefix, e.g. `/Users/jess`.
    pub source_home: &'a Path,
    /// Destination home prefix, e.g. `/home/jess`.
    pub destination_home: &'a Path,
}

/// Compose missing Codex state rows with existing destination rollout files.
///
/// Imports compatible project/section dependencies before missing threads.
/// Path fields alone are translated. Missing rollout files, divergent keys,
/// and newer source-only columns stay in typed provenance; no control or
/// enrollment table is activated and no target schema migration is invented.
///
/// # Errors
/// Uses the same offline/private-output refusals as [`compose_snapshots`].
pub fn compose_state_snapshots(
    base: &Path,
    incoming: &Path,
    output: &Path,
    source_id: &str,
    max_steps: u32,
    mapping: &PathMapping<'_>,
) -> Result<Composition> {
    if !mapping.source_home.is_absolute() || !mapping.destination_home.is_absolute() {
        return Err(BulkloadRefusal::PathNotPortable);
    }
    compose(base, incoming, output, source_id, max_steps, Some(mapping))
}

fn compose(
    base: &Path,
    incoming: &Path,
    output: &Path,
    source_id: &str,
    max_steps: u32,
    mapping: Option<&PathMapping<'_>>,
) -> Result<Composition> {
    if source_id.is_empty() {
        return Err(BulkloadRefusal::SqliteUnsupportedValue);
    }
    snapshot(base, output, max_steps)?;
    let source = Connection::open_with_flags(incoming, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(sql_refusal)?;
    source.busy_timeout(Duration::ZERO).map_err(sql_refusal)?;
    source.execute_batch("BEGIN").map_err(sql_refusal)?;
    let mut destination = Connection::open(output).map_err(sql_refusal)?;
    destination
        .busy_timeout(Duration::ZERO)
        .map_err(sql_refusal)?;
    destination
        .execute_batch("PRAGMA foreign_keys=ON")
        .map_err(sql_refusal)?;
    let transaction = destination.transaction().map_err(sql_refusal)?;
    transaction
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS bulkload_provider_rows (
        source_id TEXT NOT NULL, table_name TEXT NOT NULL,
        ordinal INTEGER NOT NULL, columns_blob BLOB NOT NULL,
        row_blob BLOB NOT NULL, disposition TEXT NOT NULL,
        native_id INTEGER, PRIMARY KEY(source_id,table_name,ordinal))",
        )
        .map_err(sql_refusal)?;
    let mut tables = source
        .prepare("SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name")
        .map_err(sql_refusal)?
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(sql_refusal)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(sql_refusal)?;
    let mut summary = Composition::default();
    if let Some(mapping) = mapping {
        correct_base_paths(base, &transaction, mapping, &mut summary)?;
    }
    tables.sort_by_key(|table| match table.as_str() {
        "projects" | "thread_sections" => 0,
        "threads" => 1,
        _ => 2,
    });
    for table in tables {
        compose_table(
            &source,
            &transaction,
            &table,
            source_id,
            mapping,
            &mut summary,
        )?;
    }
    let foreign_key_errors: i64 = transaction
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .map_err(sql_refusal)?;
    if foreign_key_errors != 0 {
        return Err(BulkloadRefusal::SqliteIntegrityCheckFailed);
    }
    summary.unresolved = transaction
        .query_row(
            "SELECT count(*) FROM bulkload_provider_rows WHERE disposition IN ('conflict','unsupported','source-columns','base-path-unavailable')",
            [],
            |row| row.get(0),
        )
        .map_err(sql_refusal)?;
    transaction.commit().map_err(sql_refusal)?;
    let integrity: String = destination
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .map_err(sql_refusal)?;
    if integrity != "ok" {
        return Err(BulkloadRefusal::SqliteIntegrityCheckFailed);
    }
    fs::File::open(output)?.sync_all()?;
    Ok(summary)
}

fn sql_refusal(_: rusqlite::Error) -> BulkloadRefusal {
    BulkloadRefusal::SqliteUnsupportedValue
}

fn correct_base_paths(
    base: &Path,
    destination: &Connection,
    mapping: &PathMapping<'_>,
    summary: &mut Composition,
) -> Result<()> {
    use rusqlite::{params, types::Value};
    let source =
        Connection::open_with_flags(base, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(sql_refusal)?;
    let schema = columns(&source, "threads")?;
    if schema.is_empty() {
        return Ok(());
    }
    if !native_insert_is_safe(destination, "threads", &schema)? {
        return Err(BulkloadRefusal::SqliteUnsupportedValue);
    }
    let index = |name: &str| {
        schema
            .iter()
            .position(|column| column.0 == name)
            .ok_or(BulkloadRefusal::SqliteUnsupportedValue)
    };
    let id = index("id")?;
    let rollout = index("rollout_path")?;
    let cwd = index("cwd")?;
    let columns_blob =
        postcard::to_stdvec(&schema).map_err(|_| BulkloadRefusal::SqliteUnsupportedValue)?;
    let mut statement = source
        .prepare("SELECT * FROM threads")
        .map_err(sql_refusal)?;
    let mut rows = statement.query([]).map_err(sql_refusal)?;
    while let Some(row) = rows.next().map_err(sql_refusal)? {
        let values = (0..schema.len())
            .map(|column| row.get::<_, Value>(column))
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(sql_refusal)?;
        let needs_mapping = [rollout,cwd].iter().any(|index| {
            matches!(values.get(*index), Some(Value::Text(path)) if (if *index == rollout {mapped_rollout_path(Path::new(path), mapping)} else {mapped_path(Path::new(path), mapping)}) != Path::new(path))
        });
        if !needs_mapping {
            continue;
        }
        let encoded = encode_row(&values);
        let provenance = format!("base-path:{}", blake3::hash(&encoded).to_hex());
        let disposition = if let Some(mapped) =
            mapped_values("threads", &schema, &schema, &values, Some(mapping))?
        {
            let updated = destination.prepare_cached("UPDATE threads SET rollout_path=?1,cwd=?2 WHERE id IS ?3 AND rollout_path IS ?4 AND cwd IS ?5")
                .map_err(sql_refusal)?.execute(params![
                    mapped.get(rollout), mapped.get(cwd), values.get(id), values.get(rollout), values.get(cwd)
                ]).map_err(sql_refusal)?;
            if updated != 1 {
                return Err(BulkloadRefusal::SqliteStateChanged);
            }
            summary.paths_corrected += 1;
            "base-path-corrected"
        } else {
            summary.unavailable_rollouts += 1;
            "base-path-unavailable"
        };
        destination.prepare_cached("INSERT INTO bulkload_provider_rows VALUES (?1,'threads',0,?2,?3,?4,NULL) ON CONFLICT(source_id,table_name,ordinal) DO UPDATE SET disposition=excluded.disposition WHERE columns_blob=excluded.columns_blob AND row_blob=excluded.row_blob")
            .map_err(sql_refusal)?.execute(params![provenance,columns_blob,encoded,disposition]).map_err(sql_refusal)?;
    }
    Ok(())
}

fn identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn columns(connection: &Connection, table: &str) -> Result<Vec<(String, String, i64, i64)>> {
    connection
        .prepare(&format!("PRAGMA table_info({})", identifier(table)))
        .map_err(sql_refusal)?
        .query_map([], |row| {
            Ok((row.get(1)?, row.get(2)?, row.get(3)?, row.get(5)?))
        })
        .map_err(sql_refusal)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(sql_refusal)
}

// Typed provenance format: repeated tag/u64-LE-length/payload values. Tags
// 0..4 are NULL, i64-LE, f64 bits-LE, UTF-8 text, and opaque blob respectively.
// The columns_blob stores postcard (name, declared type, not-null, PK ordinal)
// tuples in exactly the same order. Even identical recovery rows keep distinct
// source ordinals; no DISTINCT/set reduction discards their multiplicity.
fn encode_row(values: &[rusqlite::types::Value]) -> Vec<u8> {
    use rusqlite::types::Value;
    let mut encoded = Vec::new();
    for value in values {
        let (tag, bytes) = match value {
            Value::Null => (0, Vec::new()),
            Value::Integer(value) => (1, value.to_le_bytes().to_vec()),
            Value::Real(value) => (2, value.to_bits().to_le_bytes().to_vec()),
            Value::Text(value) => (3, value.as_bytes().to_vec()),
            Value::Blob(value) => (4, value.clone()),
        };
        encoded.push(tag);
        encoded.extend_from_slice(&u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_le_bytes());
        encoded.extend_from_slice(&bytes);
    }
    encoded
}

fn compose_table(
    source: &Connection,
    destination: &Connection,
    table: &str,
    source_id: &str,
    mapping: Option<&PathMapping<'_>>,
    summary: &mut Composition,
) -> Result<()> {
    use rusqlite::{params, OptionalExtension as _};
    let schema = columns(source, table)?;
    let target_schema = columns(destination, table)?;
    let history = matches!(
        table,
        "thread_turns"
            | "thread_items"
            | "thread_history_projection_state"
            | "thread_realtime_items"
            | "logs"
    ) && schema == target_schema;
    let state = mapping.is_some()
        && matches!(
            table,
            "projects"
                | "thread_sections"
                | "project_roots"
                | "threads"
                | "thread_dynamic_tools"
                | "thread_artifacts"
                | "thread_spawn_edges"
        )
        && !target_schema.is_empty()
        && target_schema.iter().all(|column| schema.contains(column));
    let native = (history || state) && native_insert_is_safe(destination, table, &target_schema)?;
    let names: Vec<_> = target_schema
        .iter()
        .map(|column| identifier(&column.0))
        .collect();
    let columns_blob =
        postcard::to_stdvec(&schema).map_err(|_| BulkloadRefusal::SqliteUnsupportedValue)?;
    let mut select = source
        .prepare(&format!("SELECT * FROM {}", identifier(table)))
        .map_err(sql_refusal)?;
    let count = select.column_count();
    let mut rows = select.query([]).map_err(sql_refusal)?;
    let mut ordinal = 0_i64;
    while let Some(row) = rows.next().map_err(sql_refusal)? {
        ordinal = ordinal
            .checked_add(1)
            .ok_or(BulkloadRefusal::BudgetExceeded)?;
        let values = (0..count)
            .map(|index| row.get::<_, rusqlite::types::Value>(index))
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(sql_refusal)?;
        let encoded = encode_row(&values);
        let previous: Option<Vec<u8>> = destination.prepare_cached(
            "SELECT row_blob FROM bulkload_provider_rows WHERE source_id=?1 AND table_name=?2 AND ordinal=?3"
        ).map_err(sql_refusal)?.query_row(
            params![source_id, table, ordinal], |row| row.get(0)
        ).optional().map_err(sql_refusal)?;
        if let Some(previous) = previous {
            if previous != encoded {
                return Err(BulkloadRefusal::SqliteStateChanged);
            }
            summary.equivalent += 1;
            continue;
        }
        let mapped = mapped_values(table, &schema, &target_schema, &values, mapping)?;
        let (disposition, native_id) = if native && mapped.is_some() {
            let mapped = mapped.ok_or(BulkloadRefusal::SqliteUnsupportedValue)?;
            let (disposition, native_id) =
                insert_provider_row(destination, table, &names, &mapped, summary)?;
            (
                if schema != target_schema && disposition != "conflict" {
                    "source-columns"
                } else {
                    disposition
                },
                native_id,
            )
        } else {
            summary.preserved += 1;
            ("unsupported", None)
        };
        destination
            .prepare_cached("INSERT INTO bulkload_provider_rows VALUES (?1,?2,?3,?4,?5,?6,?7)")
            .map_err(sql_refusal)?
            .execute(params![
                source_id,
                table,
                ordinal,
                columns_blob,
                encoded,
                disposition,
                native_id
            ])
            .map_err(sql_refusal)?;
    }
    Ok(())
}

fn mapped_values(
    table: &str,
    schema: &[(String, String, i64, i64)],
    target: &[(String, String, i64, i64)],
    values: &[rusqlite::types::Value],
    mapping: Option<&PathMapping<'_>>,
) -> Result<Option<Vec<rusqlite::types::Value>>> {
    use rusqlite::types::Value;
    let mut selected = Vec::new();
    for column in target {
        let Some(index) = schema.iter().position(|source| source.0 == column.0) else {
            return Ok(None);
        };
        let mut value = values
            .get(index)
            .ok_or(BulkloadRefusal::SqliteUnsupportedValue)?
            .clone();
        if let Some(mapping) = mapping.filter(|_| {
            (table == "threads" && matches!(column.0.as_str(), "rollout_path" | "cwd"))
                || (table == "project_roots" && column.0 == "path")
        }) {
            let Value::Text(path) = &value else {
                return Ok(None);
            };
            let path = Path::new(path);
            if !path.is_absolute()
                || path
                    .components()
                    .any(|part| part == std::path::Component::ParentDir)
            {
                return Ok(None);
            }
            let mapped = if column.0 == "rollout_path" {
                mapped_rollout_path(path, mapping)
            } else {
                mapped_path(path, mapping)
            };
            if column.0 == "rollout_path" && !mapped.is_file() {
                return Ok(None);
            }
            value = Value::Text(
                mapped
                    .to_str()
                    .ok_or(BulkloadRefusal::PathNotPortable)?
                    .to_owned(),
            );
        }
        selected.push(value);
    }
    Ok(Some(selected))
}

fn native_insert_is_safe(
    destination: &Connection,
    table: &str,
    schema: &[(String, String, i64, i64)],
) -> Result<bool> {
    let mut statement = destination
        .prepare("SELECT name,sql FROM sqlite_schema WHERE type='trigger' AND tbl_name=?1")
        .map_err(sql_refusal)?;
    let triggers = statement
        .query_map([table], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(sql_refusal)?;
    for trigger in triggers {
        let (name, sql) = trigger.map_err(sql_refusal)?;
        let sql = sql.split_whitespace().collect::<Vec<_>>().join(" ");
        // Exact known provider DDL, allowing formatting whitespace only.
        // This DELETE-only trigger cannot fire during INSERT OR IGNORE.
        // Unknown INSERT triggers could rewrite/delete retained base rows.
        let known_delete = table == "thread_history_projection_state"
            && name == "thread_realtime_items_projection_cleanup"
            && sql
                == "CREATE TRIGGER thread_realtime_items_projection_cleanup AFTER DELETE ON thread_history_projection_state BEGIN DELETE FROM thread_realtime_items WHERE thread_id = OLD.thread_id; END";
        if !(known_delete || table == "threads" && known_thread_trigger(&name, &sql)) {
            return Ok(false);
        }
    }
    if table != "logs" {
        return Ok(true);
    }
    let rowid_column = schema.first().is_some_and(|column| {
        column.0 == "id" && column.1.eq_ignore_ascii_case("INTEGER") && column.3 == 1
    }) && schema.iter().skip(1).all(|column| column.3 == 0);
    let pk_indexes: i64 = destination
        .query_row(
            "SELECT count(*) FROM pragma_index_list(?1) WHERE origin='pk'",
            [table],
            |row| row.get(0),
        )
        .map_err(sql_refusal)?;
    // INTEGER PRIMARY KEY DESC and WITHOUT ROWID tables have a PK index;
    // neither gives the required NULL -> fresh rowid allocation semantics.
    Ok(rowid_column && pk_indexes == 0)
}

fn known_thread_trigger(name: &str, sql: &str) -> bool {
    // Observed provider DDL only. Each INSERT-trigger update targets NEW.id;
    // UPDATE triggers do not change any other thread. Unknown bodies refuse.
    const TRIGGERS: &[(&str, &str)] = &[
        ("threads_created_at_ms_after_insert", "CREATE TRIGGER threads_created_at_ms_after_insert AFTER INSERT ON threads WHEN NEW.created_at_ms IS NULL BEGIN UPDATE threads SET created_at_ms = NEW.created_at * 1000 WHERE id = NEW.id; END"),
        ("threads_updated_at_ms_after_insert", "CREATE TRIGGER threads_updated_at_ms_after_insert AFTER INSERT ON threads WHEN NEW.updated_at_ms IS NULL BEGIN UPDATE threads SET updated_at_ms = NEW.updated_at * 1000 WHERE id = NEW.id; END"),
        ("threads_created_at_ms_after_update", "CREATE TRIGGER threads_created_at_ms_after_update AFTER UPDATE OF created_at ON threads WHEN NEW.created_at != OLD.created_at AND NEW.created_at_ms IS OLD.created_at_ms BEGIN UPDATE threads SET created_at_ms = NEW.created_at * 1000 WHERE id = NEW.id; END"),
        ("threads_updated_at_ms_after_update", "CREATE TRIGGER threads_updated_at_ms_after_update AFTER UPDATE OF updated_at ON threads WHEN NEW.updated_at != OLD.updated_at AND NEW.updated_at_ms IS OLD.updated_at_ms BEGIN UPDATE threads SET updated_at_ms = NEW.updated_at * 1000 WHERE id = NEW.id; END"),
        ("threads_recency_at_after_insert", "CREATE TRIGGER threads_recency_at_after_insert AFTER INSERT ON threads WHEN NEW.recency_at_ms = 0 BEGIN UPDATE threads SET recency_at = NEW.updated_at, recency_at_ms = COALESCE(NEW.updated_at_ms, NEW.updated_at * 1000) WHERE id = NEW.id; END"),
    ];
    TRIGGERS
        .iter()
        .any(|known| known.0 == name && known.1 == sql)
}

fn insert_provider_row(
    destination: &Connection,
    table: &str,
    names: &[String],
    values: &[rusqlite::types::Value],
    summary: &mut Composition,
) -> Result<(&'static str, Option<i64>)> {
    use rusqlite::params_from_iter;
    let mut insert_values = values.to_vec();
    if table == "logs" {
        if names.first().map(String::as_str) != Some("\"id\"") {
            return Err(BulkloadRefusal::SqliteUnsupportedValue);
        }
        *insert_values
            .first_mut()
            .ok_or(BulkloadRefusal::SqliteUnsupportedValue)? = rusqlite::types::Value::Null;
    } else {
        let predicate = names
            .iter()
            .map(|name| format!("{name} IS ?"))
            .collect::<Vec<_>>()
            .join(" AND ");
        let exists: bool = destination
            .prepare_cached(&format!(
                "SELECT EXISTS(SELECT 1 FROM {} WHERE {predicate})",
                identifier(table)
            ))
            .map_err(sql_refusal)?
            .query_row(params_from_iter(values), |row| row.get(0))
            .map_err(sql_refusal)?;
        if exists {
            summary.equivalent += 1;
            return Ok(("equivalent", None));
        }
    }
    let placeholders = vec!["?"; names.len()].join(",");
    let inserted = destination
        .prepare_cached(&format!(
            "INSERT OR IGNORE INTO {} ({}) VALUES ({placeholders})",
            identifier(table),
            names.join(",")
        ))
        .map_err(sql_refusal)?
        .execute(params_from_iter(&insert_values));
    let inserted = match inserted {
        Ok(count) => count,
        Err(rusqlite::Error::SqliteFailure(error, _))
            if error.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            0
        }
        Err(error) => return Err(sql_refusal(error)),
    };
    if inserted == 1 {
        summary.inserted += 1;
        Ok((
            "inserted",
            (table == "logs").then(|| destination.last_insert_rowid()),
        ))
    } else {
        summary.preserved += 1;
        Ok(("conflict", None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn state_import_maps_existing_rollouts_and_keeps_missing_rows_private(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let dir = std::env::temp_dir().join(format!("tcfs-provider-state-{}", std::process::id()));
        fs::create_dir(&dir)?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        let seat = dir.join("seat");
        fs::create_dir(&seat)?;
        fs::write(seat.join("session.jsonl"), b"{}\n")?;
        let base = dir.join("base.sqlite");
        let incoming = dir.join("incoming.sqlite");
        let output = dir.join("candidate.sqlite");
        let left = Connection::open(&base)?;
        let right = Connection::open(&incoming)?;
        for connection in [&left, &right] {
            connection.execute_batch("CREATE TABLE projects(id TEXT PRIMARY KEY, name TEXT); CREATE TABLE threads(id TEXT PRIMARY KEY, rollout_path TEXT NOT NULL, cwd TEXT NOT NULL, project_id TEXT REFERENCES projects(id));")?;
        }
        left.execute_batch("INSERT INTO threads VALUES('stale','/source/home/session.jsonl','/source/home/git/existing',NULL);")?;
        left.execute(
            "INSERT INTO threads VALUES('native',?1,'/native/work',NULL)",
            [seat.join("session.jsonl").to_string_lossy().as_ref()],
        )?;
        right.execute_batch("ALTER TABLE threads ADD COLUMN originator TEXT; INSERT INTO projects VALUES('project','source project'); INSERT INTO threads VALUES('ready','/source/home/session.jsonl','/source/home/git/lab','project','kept'); INSERT INTO threads VALUES('missing','/source/home/absent.jsonl','/source/home/git/lab','project','kept too');")?;
        let result = compose_state_snapshots(
            &base,
            &incoming,
            &output,
            "source",
            100,
            &PathMapping {
                source_home: Path::new("/source/home"),
                destination_home: &seat,
            },
        );
        assert!(matches!(
            result,
            Ok(Composition {
                inserted: 2,
                preserved: 1,
                unresolved: 2,
                paths_corrected: 1,
                ..
            })
        ));
        let candidate = Connection::open_with_flags(&output, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let path: String = candidate.query_row(
            "SELECT rollout_path FROM threads WHERE id='ready'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(Path::new(&path), seat.join("session.jsonl"));
        let corrected: String = candidate.query_row(
            "SELECT rollout_path FROM threads WHERE id='stale' AND project_id IS NULL",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(Path::new(&corrected), seat.join("session.jsonl"));
        let native_cwd: String =
            candidate.query_row("SELECT cwd FROM threads WHERE id='native'", [], |row| {
                row.get(0)
            })?;
        assert_eq!(native_cwd, "/native/work");
        let missing: i64 = candidate.query_row(
            "SELECT count(*) FROM threads WHERE id='missing'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(missing, 0);
        let preserved: i64 = candidate.query_row(
            "SELECT count(*) FROM bulkload_provider_rows WHERE table_name='threads'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(preserved, 3);
        drop(candidate);
        drop(left);
        drop(right);
        for file in [base, incoming, output, seat.join("session.jsonl")] {
            fs::remove_file(file)?;
        }
        fs::remove_dir(seat)?;
        fs::remove_dir(dir)?;
        Ok(())
    }

    #[test]
    fn native_insert_refuses_triggers_and_non_rowid_log_keys(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = Connection::open_in_memory()?;
        db.execute_batch("CREATE TABLE logs(id INTEGER PRIMARY KEY, body TEXT); CREATE TRIGGER danger BEFORE INSERT ON logs BEGIN DELETE FROM logs; END;")?;
        let schema = vec![
            ("id".into(), "INTEGER".into(), 0, 1),
            ("body".into(), "TEXT".into(), 0, 0),
        ];
        assert!(matches!(
            native_insert_is_safe(&db, "logs", &schema),
            Ok(false)
        ));
        db.execute_batch("DROP TRIGGER danger;")?;
        assert!(matches!(
            native_insert_is_safe(&db, "logs", &schema),
            Ok(true)
        ));
        db.execute_batch(
            "DROP TABLE logs; CREATE TABLE logs(id INTEGER PRIMARY KEY DESC, body TEXT);",
        )?;
        assert!(matches!(
            native_insert_is_safe(&db, "logs", &schema),
            Ok(false)
        ));
        db.execute_batch(
            "DROP TABLE logs; CREATE TABLE logs(id INTEGER PRIMARY KEY, body TEXT) WITHOUT ROWID;",
        )?;
        assert!(matches!(
            native_insert_is_safe(&db, "logs", &schema),
            Ok(false)
        ));
        db.execute_batch("CREATE TABLE thread_history_projection_state(thread_id TEXT PRIMARY KEY); CREATE TABLE thread_realtime_items(thread_id TEXT); CREATE TRIGGER thread_realtime_items_projection_cleanup AFTER DELETE ON thread_history_projection_state BEGIN DELETE FROM thread_realtime_items WHERE thread_id = OLD.thread_id; END;")?;
        assert!(matches!(
            native_insert_is_safe(&db, "thread_history_projection_state", &[]),
            Ok(true)
        ));
        db.execute_batch("DROP TRIGGER thread_realtime_items_projection_cleanup; CREATE TRIGGER thread_realtime_items_projection_cleanup AFTER INSERT ON thread_history_projection_state BEGIN DELETE FROM thread_realtime_items WHERE thread_id = NEW.thread_id; END;")?;
        assert!(matches!(
            native_insert_is_safe(&db, "thread_history_projection_state", &[]),
            Ok(false)
        ));
        Ok(())
    }

    #[test]
    fn offline_union_preserves_collisions_and_remaps_log_ids(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let dir = std::env::temp_dir().join(format!("tcfs-provider-union-{}", std::process::id()));
        fs::create_dir(&dir)?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        let base = dir.join("base.sqlite");
        let incoming = dir.join("incoming.sqlite");
        let output = dir.join("candidate.sqlite");
        let repeated = dir.join("repeated.sqlite");
        let left = Connection::open(&base)?;
        let right = Connection::open(&incoming)?;
        for connection in [&left, &right] {
            connection.execute_batch("CREATE TABLE thread_items(thread_id TEXT, item_id TEXT, payload BLOB, PRIMARY KEY(thread_id,item_id)); CREATE TABLE logs(id INTEGER PRIMARY KEY AUTOINCREMENT, body TEXT);")?;
        }
        left.execute_batch("INSERT INTO thread_items VALUES('thread','item',x'01'); INSERT INTO logs VALUES(1,'sting');")?;
        right.execute_batch("INSERT INTO thread_items VALUES('thread','item',x'02'); INSERT INTO thread_items VALUES('thread','new',x'03'); INSERT INTO logs VALUES(1,'neo'); CREATE TABLE lost_and_found(value BLOB); INSERT INTO lost_and_found VALUES(x'ff'),(x'ff');")?;
        let first = compose_snapshots(&base, &incoming, &output, "neo-snapshot", 100);
        assert!(first.is_ok());
        let candidate = Connection::open_with_flags(&output, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let logs: i64 = candidate.query_row("SELECT count(*) FROM logs", [], |row| row.get(0))?;
        assert_eq!(logs, 2);
        let original: Vec<u8> = candidate.query_row(
            "SELECT payload FROM thread_items WHERE item_id='item'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(original, vec![1]);
        let conflicts: i64 = candidate.query_row(
            "SELECT count(*) FROM bulkload_provider_rows WHERE disposition='conflict'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(conflicts, 1);
        let orphans: i64 = candidate.query_row(
            "SELECT count(*) FROM bulkload_provider_rows WHERE table_name='lost_and_found'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(orphans, 2);
        let rerun = compose_snapshots(&output, &incoming, &repeated, "neo-snapshot", 100);
        assert!(matches!(
            rerun,
            Ok(Composition {
                inserted: 0,
                preserved: 0,
                unresolved: 4,
                ..
            })
        ));
        let again = Connection::open_with_flags(&repeated, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let logs_again: i64 = again.query_row("SELECT count(*) FROM logs", [], |row| row.get(0))?;
        assert_eq!(logs_again, 2);
        drop(again);
        drop(candidate);
        drop(left);
        drop(right);
        for file in [base, incoming, output, repeated] {
            fs::remove_file(file)?;
        }
        fs::remove_dir(dir)?;
        Ok(())
    }

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
