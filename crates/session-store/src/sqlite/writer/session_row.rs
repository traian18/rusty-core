//! The `sessions` row every event and snapshot refers to.

use rusqlite::{params, Connection};
use rusty_protocol::ids::{AgentId, SessionId};

use crate::{sqlite::errors::map_sqlite_error, store::StoreError};

const INSERT_KEEPING_ROOT: &str =
    "INSERT INTO sessions (session_id, root_agent_id, created_at, updated_at)
     VALUES (?1, ?2, ?3, ?3)
     ON CONFLICT(session_id) DO UPDATE SET updated_at = excluded.updated_at";

const INSERT_CORRECTING_ROOT: &str =
    "INSERT INTO sessions (session_id, root_agent_id, created_at, updated_at)
     VALUES (?1, ?2, ?3, ?3)
     ON CONFLICT(session_id) DO UPDATE SET
         root_agent_id = excluded.root_agent_id,
         updated_at    = excluded.updated_at";

/// Which root agent an existing session row ends up with.
#[derive(Clone, Copy)]
pub(super) enum RootAgent {
    /// Keep the one recorded on first insert. An event's agent may be a child.
    KeepExisting,
    /// Replace it. A snapshot knows the session's true root agent.
    CorrectToGiven,
}

/// Makes sure the session has a row and touches its `updated_at`.
pub(super) fn ensure_session(
    connection: &Connection,
    session_id: &SessionId,
    root_agent_id: &AgentId,
    updated_at: i64,
    root: RootAgent,
) -> Result<(), StoreError> {
    let sql = match root {
        RootAgent::KeepExisting => INSERT_KEEPING_ROOT,
        RootAgent::CorrectToGiven => INSERT_CORRECTING_ROOT,
    };
    connection
        .execute(
            sql,
            params![
                session_id.to_string(),
                root_agent_id.to_string(),
                updated_at
            ],
        )
        .map_err(|error| map_sqlite_error("ensure session row", error))?;
    Ok(())
}
