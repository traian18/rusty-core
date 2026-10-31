//! Reading a session's snapshot.

use std::str::FromStr;

use rusqlite::{params, Connection, OptionalExtension};
use rusty_protocol::ids::{AgentId, SessionId, Timestamp};

use crate::{
    sqlite::errors::map_sqlite_error,
    store::{DurableSessionMetadata, DurableSessionSnapshot, StoreError, StoredAgentState},
};

/// A `snapshots` row as stored, before it is decoded.
struct SnapshotRow {
    root_agent_id: String,
    agents_json: String,
    session_sequence: i64,
    timestamp_rfc3339: String,
    schema_version: i64,
    metadata_json: String,
}

/// The session's latest snapshot, if one was saved.
///
/// Rows written before versioning existed carry version 0 and empty metadata, which
/// `version::migrate_snapshot` knows how to upgrade.
pub(super) fn load_snapshot(
    connection: &Connection,
    id: SessionId,
    purpose: &str,
) -> Result<Option<DurableSessionSnapshot>, StoreError> {
    let row = connection
        .query_row(
            "SELECT root_agent_id, agents, session_sequence, timestamp, schema_version, metadata
             FROM snapshots WHERE session_id = ?1",
            params![id.to_string()],
            |row| {
                Ok(SnapshotRow {
                    root_agent_id: row.get(0)?,
                    agents_json: row.get(1)?,
                    session_sequence: row.get(2)?,
                    timestamp_rfc3339: row.get(3)?,
                    schema_version: row.get(4)?,
                    metadata_json: row.get(5)?,
                })
            },
        )
        .optional()
        .map_err(|error| map_sqlite_error(purpose, error))?;
    row.map(|row| decode(id, row)).transpose()
}

fn decode(id: SessionId, row: SnapshotRow) -> Result<DurableSessionSnapshot, StoreError> {
    let agents: Vec<StoredAgentState> = serde_json::from_str(&row.agents_json)?;
    let root_agent_id = AgentId::from_str(&row.root_agent_id).map_err(|error| {
        corrupt(format!(
            "root_agent_id {:?} in snapshot: {error}",
            row.root_agent_id
        ))
    })?;
    let timestamp = serde_json::from_str::<Timestamp>(&format!("\"{}\"", row.timestamp_rfc3339))
        .map_err(|error| {
            corrupt(format!(
                "snapshot timestamp {:?}: {error}",
                row.timestamp_rfc3339
            ))
        })?;
    let metadata: DurableSessionMetadata =
        serde_json::from_str(&row.metadata_json).map_err(|error| {
            corrupt(format!(
                "snapshot metadata {:?}: {error}",
                row.metadata_json
            ))
        })?;
    Ok(DurableSessionSnapshot {
        session_id: id,
        root_agent_id,
        agents,
        session_sequence: row.session_sequence as u64,
        timestamp,
        schema_version: row.schema_version as u64,
        metadata,
    })
}

fn corrupt(detail: String) -> StoreError {
    StoreError::InvalidState(format!("corrupt {detail}"))
}
