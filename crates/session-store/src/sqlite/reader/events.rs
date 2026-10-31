//! Reading durable events.

use rusqlite::{params, Connection, Params};
use rusty_protocol::{events::AgentEventEnvelope, ids::SessionId};

use super::{checkout, ReadPool};
use crate::{
    sqlite::errors::map_sqlite_error,
    store::{DurableSessionEvent, StoreError},
};

const EVENTS_AFTER_SEQUENCE: &str = "SELECT session_sequence, envelope FROM durable_events
     WHERE session_id = ?1 AND session_sequence > ?2
     ORDER BY session_sequence ASC, appended_at ASC";

const ALL_EVENTS: &str = "SELECT session_sequence, envelope FROM durable_events
     WHERE session_id = ?1
     ORDER BY session_sequence ASC, appended_at ASC";

/// The events after a reconnect cursor, without the restore snapshot's cutoff. The index makes
/// this a bounded range query however long the history is.
pub(in crate::sqlite) fn events_since(
    pool: &ReadPool,
    id: SessionId,
    since_sequence: u64,
) -> Result<Vec<DurableSessionEvent>, StoreError> {
    let connection = checkout(pool)?;
    query_events(
        &connection,
        EVENTS_AFTER_SEQUENCE,
        params![id.to_string(), since_sequence as i64],
        "resumed event",
    )
}

pub(super) fn all_events(
    connection: &Connection,
    id: SessionId,
) -> Result<Vec<DurableSessionEvent>, StoreError> {
    query_events(connection, ALL_EVENTS, params![id.to_string()], "event")
}

/// The sequence column orders events and wins over whatever the envelope says: an event appended
/// without a sequence was given one when stored.
fn query_events(
    connection: &Connection,
    sql: &str,
    parameters: impl Params,
    purpose: &str,
) -> Result<Vec<DurableSessionEvent>, StoreError> {
    let mut statement = connection
        .prepare(sql)
        .map_err(|error| map_sqlite_error(&format!("prepare {purpose} query"), error))?;
    let rows = statement
        .query_map(parameters, |row| {
            Ok((row.get::<_, i64>(0)? as u64, row.get::<_, String>(1)?))
        })
        .map_err(|error| map_sqlite_error(&format!("query {purpose}s"), error))?;
    rows.map(|row| {
        let (sequence, envelope_json) =
            row.map_err(|error| map_sqlite_error(&format!("read {purpose} row"), error))?;
        decode_event(sequence, &envelope_json)
    })
    .collect()
}

fn decode_event(sequence: u64, envelope_json: &str) -> Result<DurableSessionEvent, StoreError> {
    let mut envelope: AgentEventEnvelope =
        serde_json::from_str(envelope_json).map_err(|error| {
            StoreError::InvalidState(format!("corrupt durable event envelope: {error}"))
        })?;
    envelope.session_sequence = Some(sequence);
    Ok(DurableSessionEvent {
        envelope,
        session_sequence: Some(sequence),
    })
}
