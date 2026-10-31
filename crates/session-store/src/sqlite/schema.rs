//! Opening the writer connection: pragmas, WAL confirmation and the schema.

use std::time::Duration;

use rusqlite::Connection;

use super::errors::map_sqlite_error;
use crate::store::StoreError;

/// How long a connection waits on a lock before failing with `SQLITE_BUSY`, so concurrent WAL
/// checkpoints do not fail under normal operation.
pub(super) const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Applied on every open. Every statement is `IF NOT EXISTS`, so re-applying after a crash is safe.
const SCHEMA_SQL: &str = include_str!("../../migrations/0001_init.sql");

/// Columns older databases lack, with the statement that adds each.
const LATE_SNAPSHOT_COLUMNS: [(&str, &str); 2] = [
    (
        "schema_version",
        "ALTER TABLE snapshots ADD COLUMN schema_version INTEGER NOT NULL DEFAULT 0;",
    ),
    (
        "metadata",
        "ALTER TABLE snapshots ADD COLUMN metadata TEXT NOT NULL DEFAULT '{}';",
    ),
];

/// Readies the connection that owns every write: pragmas, WAL mode, schema, and the columns that
/// databases created by older versions are missing.
pub(super) fn prepare_writer_connection(connection: &mut Connection) -> Result<(), StoreError> {
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(|error| map_sqlite_error("set busy timeout", error))?;
    connection
        .execute_batch("PRAGMA foreign_keys = ON; PRAGMA synchronous = NORMAL;")
        .map_err(|error| map_sqlite_error("apply writer pragmas", error))?;
    require_wal_mode(connection)?;
    connection
        .execute_batch(SCHEMA_SQL)
        .map_err(|error| map_sqlite_error("apply schema migration", error))?;
    add_missing_snapshot_columns(connection)
}

/// The durability contract needs WAL; a database that refuses it cannot honor it.
fn require_wal_mode(connection: &Connection) -> Result<(), StoreError> {
    let mode: String = connection
        .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
        .map_err(|error| map_sqlite_error("set journal_mode=WAL", error))?;
    if mode == "wal" {
        Ok(())
    } else {
        Err(StoreError::Backend(format!(
            "expected journal_mode 'wal', got {mode:?}"
        )))
    }
}

/// SQLite has no `ADD COLUMN IF NOT EXISTS`, so existing columns are read first.
fn add_missing_snapshot_columns(connection: &Connection) -> Result<(), StoreError> {
    let existing = snapshot_column_names(connection)?;
    for (column, statement) in LATE_SNAPSHOT_COLUMNS {
        if !existing.iter().any(|name| name == column) {
            connection
                .execute_batch(statement)
                .map_err(|error| map_sqlite_error(&format!("add {column} column"), error))?;
        }
    }
    Ok(())
}

fn snapshot_column_names(connection: &Connection) -> Result<Vec<String>, StoreError> {
    let mut statement = connection
        .prepare("PRAGMA table_info(snapshots)")
        .map_err(|error| map_sqlite_error("inspect snapshots columns", error))?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|error| map_sqlite_error("read snapshots columns", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| map_sqlite_error("collect snapshots columns", error))?;
    Ok(names)
}
