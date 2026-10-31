//! The list of stored sessions.

use std::str::FromStr;

use rusty_protocol::ids::SessionId;

use super::{checkout, load::load_session, ReadPool};
use crate::{
    sqlite::errors::map_sqlite_error,
    store::{summarize_session, SessionSummary, StoreError},
};

/// A summary of every stored session, most recently updated first.
pub(in crate::sqlite) fn list_sessions(pool: &ReadPool) -> Result<Vec<SessionSummary>, StoreError> {
    let mut summaries = Vec::new();
    for id in stored_session_ids(pool)? {
        if let Some(summary) = summarize_session(&load_session(pool, id)?) {
            summaries.push(summary);
        }
    }
    summaries.sort_by_key(|summary| std::cmp::Reverse(summary.updated_at));
    Ok(summaries)
}

fn stored_session_ids(pool: &ReadPool) -> Result<Vec<SessionId>, StoreError> {
    let connection = checkout(pool)?;
    let mut statement = connection
        .prepare("SELECT session_id FROM sessions ORDER BY updated_at DESC")
        .map_err(|error| map_sqlite_error("prepare session catalog query", error))?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| map_sqlite_error("query session catalog", error))?;
    rows.map(|row| {
        let value = row.map_err(|error| map_sqlite_error("read session catalog row", error))?;
        SessionId::from_str(&value).map_err(|error| {
            StoreError::InvalidState(format!("corrupt session_id {value:?}: {error}"))
        })
    })
    .collect()
}
