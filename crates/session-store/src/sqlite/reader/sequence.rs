//! The highest sequence a session has committed.

use rusqlite::{params, Connection};
use rusty_protocol::ids::SessionId;

use super::{checkout, ReadPool};
use crate::{sqlite::errors::map_sqlite_error, store::StoreError};

/// The larger of the highest event sequence and the snapshot's, or 0 for a session with neither.
pub(in crate::sqlite) fn current_sequence(
    pool: &ReadPool,
    id: SessionId,
) -> Result<u64, StoreError> {
    let connection = checkout(pool)?;
    let events = highest_sequence(&connection, "durable_events", id, "event")?;
    let snapshot = highest_sequence(&connection, "snapshots", id, "snapshot")?;
    Ok(events.max(snapshot))
}

fn highest_sequence(
    connection: &Connection,
    table: &str,
    id: SessionId,
    what: &str,
) -> Result<u64, StoreError> {
    let highest: Option<i64> = connection
        .query_row(
            &format!("SELECT MAX(session_sequence) FROM {table} WHERE session_id = ?1"),
            params![id.to_string()],
            |row| row.get(0),
        )
        .map_err(|error| map_sqlite_error(&format!("read max {what} sequence"), error))?;
    Ok(highest.map_or(0, |value| value as u64))
}
