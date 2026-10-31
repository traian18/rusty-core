//! Retention: deleting old events.

use rusqlite::{params, Connection};
use rusty_protocol::ids::SessionId;

use crate::{sqlite::errors::map_sqlite_error, store::StoreError};

/// Deletes the session's events with a sequence at or below `sequence` and returns how many.
///
/// A snapshot at or above `sequence` must already exist so the session can still be restored.
/// The caller owns that rule (`retention::prune_plan`); this does not check it.
pub(super) fn prune_events(
    connection: &mut Connection,
    session_id: SessionId,
    sequence: u64,
) -> Result<u64, StoreError> {
    let transaction = connection
        .transaction()
        .map_err(|error| map_sqlite_error("begin prune transaction", error))?;
    let removed = transaction
        .execute(
            "DELETE FROM durable_events WHERE session_id = ?1 AND session_sequence <= ?2",
            params![session_id.to_string(), sequence as i64],
        )
        .map_err(|error| map_sqlite_error("prune durable events", error))?;
    transaction
        .commit()
        .map_err(|error| map_sqlite_error("commit prune transaction", error))?;
    Ok(removed as u64)
}
