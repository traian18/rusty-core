//! Saving a session's snapshot.

use rusqlite::{params, Connection};

use super::session_row::{ensure_session, RootAgent};
use crate::{
    sqlite::errors::{map_sqlite_error, now_ms},
    store::{DurableSessionSnapshot, StoreError},
};

/// Replaces the session's single snapshot row, in its own transaction, and corrects the session's
/// root agent to the snapshot's. The snapshot's schema version and dependency metadata are stored
/// with it so both survive a restart.
///
/// The timestamp is stored as RFC 3339 text so it round-trips without loss; SQLite accepts text in
/// the column and RFC 3339 sorts like time.
pub(super) fn save_snapshot(
    connection: &mut Connection,
    snapshot: DurableSessionSnapshot,
) -> Result<(), StoreError> {
    let transaction = connection
        .transaction()
        .map_err(|error| map_sqlite_error("begin snapshot transaction", error))?;
    ensure_session(
        &transaction,
        &snapshot.session_id,
        &snapshot.root_agent_id,
        now_ms(),
        RootAgent::CorrectToGiven,
    )?;
    transaction
        .execute(
            "INSERT INTO snapshots
                 (session_id, root_agent_id, agents, session_sequence, timestamp,
                  schema_version, metadata)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(session_id) DO UPDATE SET
                 root_agent_id    = excluded.root_agent_id,
                 agents           = excluded.agents,
                 session_sequence = excluded.session_sequence,
                 timestamp        = excluded.timestamp,
                 schema_version   = excluded.schema_version,
                 metadata         = excluded.metadata",
            params![
                snapshot.session_id.to_string(),
                snapshot.root_agent_id.to_string(),
                serde_json::to_string(&snapshot.agents)?,
                snapshot.session_sequence as i64,
                snapshot.timestamp.to_rfc3339(),
                snapshot.schema_version as i64,
                serde_json::to_string(&snapshot.metadata)?,
            ],
        )
        .map_err(|error| map_sqlite_error("save snapshot", error))?;
    transaction
        .commit()
        .map_err(|error| map_sqlite_error("commit snapshot transaction", error))
}
