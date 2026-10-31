//! Appending one event.

use rusqlite::{params, Connection, Transaction};
use rusty_protocol::ids::SessionId;

use super::session_row::{ensure_session, RootAgent};
use crate::{
    sqlite::errors::{map_sqlite_error, now_ms},
    store::{DurableSessionEvent, StoreError},
};

/// Stores `event` as one `durable_events` row in its own transaction.
///
/// The unique `(session_id, session_sequence)` index rejects a duplicate sequence at write time.
/// An event that arrives without a sequence gets the session's highest plus one.
pub(super) fn append_event(
    connection: &mut Connection,
    event: DurableSessionEvent,
) -> Result<(), StoreError> {
    let transaction = connection
        .transaction()
        .map_err(|error| map_sqlite_error("begin append transaction", error))?;
    let envelope = &event.envelope;
    ensure_session(
        &transaction,
        &envelope.session_id,
        &envelope.agent_id,
        now_ms(),
        RootAgent::KeepExisting,
    )?;
    let sequence = match event.session_sequence.or(envelope.session_sequence) {
        Some(sequence) => sequence,
        None => next_sequence(&transaction, &envelope.session_id)?,
    };
    insert_event(&transaction, &event, sequence)?;
    transaction
        .commit()
        .map_err(|error| map_sqlite_error("commit append transaction", error))
}

fn next_sequence(transaction: &Transaction, session_id: &SessionId) -> Result<u64, StoreError> {
    let highest: Option<i64> = transaction
        .query_row(
            "SELECT MAX(session_sequence) FROM durable_events WHERE session_id = ?1",
            params![session_id.to_string()],
            |row| row.get(0),
        )
        .map_err(|error| map_sqlite_error("read max session sequence", error))?;
    Ok(highest.map_or(1, |highest| (highest as u64).saturating_add(1)))
}

fn insert_event(
    transaction: &Transaction,
    event: &DurableSessionEvent,
    sequence: u64,
) -> Result<(), StoreError> {
    let envelope = &event.envelope;
    let envelope_json = serde_json::to_string(envelope)?;
    let visibility_json = serde_json::to_string(&envelope.visibility)?;
    transaction
        .execute(
            "INSERT INTO durable_events
                 (event_id, session_id, agent_id, parent_agent_id, run_id,
                  agent_sequence, session_sequence, timestamp, visibility, envelope, appended_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                envelope.event_id.to_string(),
                envelope.session_id.to_string(),
                envelope.agent_id.to_string(),
                envelope.parent_agent_id.map(|id| id.to_string()),
                envelope.run_id.map(|id| id.to_string()),
                envelope.agent_sequence as i64,
                sequence as i64,
                envelope.timestamp.timestamp_millis(),
                visibility_json,
                envelope_json,
                now_ms(),
            ],
        )
        .map_err(|error| map_sqlite_error("append durable event", error))?;
    Ok(())
}
