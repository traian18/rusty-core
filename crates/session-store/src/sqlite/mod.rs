//! A [`SessionStore`] on a WAL-mode SQLite database.
//!
//! ```text
//!        append / save_snapshot / prune_events_before
//!                          │
//!             mpsc channel of WriteCommand
//!                          ▼
//!      writer task ── owns ──> the one write connection
//!
//!      pooled read connections ◄── load_session / events_since / current_sequence / ...
//! ```
//!
//! **Writes** go through one background task (`writer`), one transaction per command, so
//! concurrent appends from many sessions never interleave. **Reads** (`reader`) use a connection
//! pool and `spawn_blocking`, and proceed while the writer works.
//!
//! **Durability.** The writer runs `journal_mode = WAL` (confirmed at open) and
//! `synchronous = NORMAL`. A crash loses at most the uncommitted tail, never a partial row: on the
//! next open SQLite replays the WAL, and the idempotent schema re-applies cleanly.
//!
//! **Storage.** `append` writes one `durable_events` row holding the whole envelope as JSON;
//! `save_snapshot` upserts the session's single `snapshots` row. The `agents` and `usage_records`
//! tables in the schema are not populated: the snapshot's JSON is the source of truth for
//! restore, and usage can be derived from the event log. An event appended without a sequence is
//! given the session's highest plus one inside the append transaction; a duplicate sequence
//! surfaces as [`StoreError::InvalidState`].

mod errors;
mod reader;
mod schema;
mod writer;

use std::path::Path;

use async_trait::async_trait;
use rusqlite::Connection;
use rusty_protocol::ids::SessionId;

use self::{
    errors::map_sqlite_error,
    reader::{build_read_pool, ReadPool},
    schema::prepare_writer_connection,
    writer::WriterHandle,
};
use crate::store::{
    is_durable, DurableSessionEvent, DurableSessionSnapshot, RawRecord, SessionStore,
    SessionSummary, StoreError, StoredSession,
};

/// A session store on one SQLite file.
pub struct SqliteSessionStore {
    writer: WriterHandle,
    read_pool: ReadPool,
}

impl SqliteSessionStore {
    /// Opens the database at `path`, creating it and its folder if needed. Fails with
    /// [`StoreError::Backend`] when the database refuses WAL mode.
    ///
    /// Must be called inside a Tokio runtime: it starts the writer task.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref();
        create_parent_folder(path)?;
        let mut connection =
            Connection::open(path).map_err(|error| map_sqlite_error("open database", error))?;
        prepare_writer_connection(&mut connection)?;
        Ok(Self {
            writer: WriterHandle::spawn(connection),
            read_pool: build_read_pool(path)?,
        })
    }

    async fn read<T: Send + 'static>(
        &self,
        what: &'static str,
        query: impl FnOnce(&ReadPool) -> Result<T, StoreError> + Send + 'static,
    ) -> Result<T, StoreError> {
        let pool = self.read_pool.clone();
        tokio::task::spawn_blocking(move || query(&pool))
            .await
            .map_err(|error| StoreError::Backend(format!("{what} task failed: {error}")))?
    }
}

#[async_trait]
impl SessionStore for SqliteSessionStore {
    async fn list_sessions(&self) -> Result<Vec<SessionSummary>, StoreError> {
        self.read("session catalog", reader::list_sessions).await
    }

    async fn load_session(&self, id: SessionId) -> Result<StoredSession, StoreError> {
        self.read("load session", move |pool| reader::load_session(pool, id))
            .await
    }

    async fn events_since(
        &self,
        id: SessionId,
        since_seq: u64,
    ) -> Result<Vec<DurableSessionEvent>, StoreError> {
        self.read("event history", move |pool| {
            reader::events_since(pool, id, since_seq)
        })
        .await
    }

    async fn current_sequence(&self, id: SessionId) -> Result<u64, StoreError> {
        self.read("sequence", move |pool| reader::current_sequence(pool, id))
            .await
    }

    async fn raw_records(&self, id: SessionId) -> Result<Vec<RawRecord>, StoreError> {
        self.read("raw records", move |pool| reader::raw_records(pool, id))
            .await
    }

    async fn append(&self, event: DurableSessionEvent) -> Result<(), StoreError> {
        if !is_durable(&event.envelope.event) {
            return Err(StoreError::InvalidState(format!(
                "refusing to persist ephemeral event: {:?}",
                event.envelope.event
            )));
        }
        self.writer.append(event).await
    }

    async fn save_snapshot(&self, snapshot: DurableSessionSnapshot) -> Result<(), StoreError> {
        self.writer.save_snapshot(snapshot).await
    }

    async fn prune_events_before(&self, id: SessionId, sequence: u64) -> Result<u64, StoreError> {
        self.writer.prune(id, sequence).await
    }
}

fn create_parent_folder(path: &Path) -> Result<(), StoreError> {
    match path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        Some(parent) => Ok(std::fs::create_dir_all(parent)?),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests;
