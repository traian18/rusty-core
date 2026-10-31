//! Restoring a session: its latest snapshot plus the events after it.

use rusty_protocol::ids::SessionId;

use super::{checkout, events::all_events, snapshot::load_snapshot, ReadPool};
use crate::store::{StoreError, StoredSession};

pub(in crate::sqlite) fn load_session(
    pool: &ReadPool,
    id: SessionId,
) -> Result<StoredSession, StoreError> {
    let connection = checkout(pool)?;
    let snapshot = load_snapshot(&connection, id, "load snapshot")?;
    let mut events = all_events(&connection, id)?;

    if let Some(snapshot) = &snapshot {
        events.retain(|event| {
            event
                .session_sequence
                .is_some_and(|sequence| sequence > snapshot.session_sequence)
        });
    }
    if snapshot.is_none() && events.is_empty() {
        return Err(StoreError::NotFound(id));
    }
    Ok(StoredSession {
        session_id: id,
        snapshot,
        events,
    })
}
