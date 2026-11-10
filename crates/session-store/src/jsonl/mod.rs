//! A [`SessionStore`](crate::store::SessionStore) that keeps each session in its own append-only `<session id>.jsonl` file.
//!
//! It needs no database, which makes it the store for hosts that only ever run a handful of
//! sessions. See `record` for the file format and crash recovery, and `writer` for how appends are
//! ordered and synced.

mod record;
mod session_store;
mod writer;

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    str::FromStr,
    sync::Mutex,
};

use rusty_protocol::ids::SessionId;

use self::{
    record::{read_records, SessionRecords},
    writer::{spawn_writer, WriterQueue},
};
use crate::store::{StoreError, StoredSession};

/// Appends between forced syncs to disk, unless the caller picks another interval.
pub const DEFAULT_SYNC_INTERVAL: u32 = 64;

/// A session store on a folder of JSONL files.
pub struct JsonlSessionStore {
    dir: PathBuf,
    sync_interval: u32,
    writers: Mutex<HashMap<SessionId, WriterQueue>>,
}

impl JsonlSessionStore {
    pub fn new(dir: impl AsRef<Path>) -> Self {
        Self::with_sync_interval(dir, DEFAULT_SYNC_INTERVAL)
    }

    /// `sync_interval` is how many appends happen between syncs to disk; 1 syncs every append.
    ///
    /// # Panics
    ///
    /// When `sync_interval` is 0, which is a programming error rather than a runtime condition.
    pub fn with_sync_interval(dir: impl AsRef<Path>, sync_interval: u32) -> Self {
        assert!(
            sync_interval >= 1,
            "sync_interval must be >= 1, got {sync_interval}"
        );
        Self {
            dir: dir.as_ref().to_path_buf(),
            sync_interval,
            writers: Mutex::new(HashMap::new()),
        }
    }

    fn path_for(&self, id: SessionId) -> PathBuf {
        self.dir.join(format!("{id}.jsonl"))
    }

    /// The live writer for the session, starting a new one when there is none or it has stopped.
    fn writer_for(&self, id: SessionId) -> WriterQueue {
        let mut writers = self
            .writers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(queue) = writers.get(&id).filter(|queue| !queue.is_closed()) {
            return queue.clone();
        }
        let queue = spawn_writer(self.path_for(id), self.sync_interval);
        writers.insert(id, queue.clone());
        queue
    }

    async fn read(&self, id: SessionId) -> Result<SessionRecords, StoreError> {
        read_records(&self.path_for(id), id).await
    }
}

fn stored_session(id: SessionId, records: SessionRecords) -> StoredSession {
    StoredSession {
        session_id: id,
        snapshot: records.snapshot,
        events: records.events,
    }
}

/// The session a `<id>.jsonl` file belongs to; `None` for any other file.
fn session_id_of_file(path: &Path) -> Option<SessionId> {
    if path.extension()?.to_str()? != "jsonl" {
        return None;
    }
    SessionId::from_str(path.file_stem()?.to_str()?).ok()
}

#[cfg(test)]
mod tests;
