
//! Append-only JSONL-backed [`SessionStore`] — default for the minimal/standalone build.
//!
//! [`JsonlSessionStore`] persists each session's durable history in a single
//! append-only JSONL file — `{session_id}.jsonl` inside a configured
//! directory — following the same validated line-delimited format this repo
//! already uses for its own run transcripts (`.rusty/runs/*/events.jsonl`).
//!
//! # On-disk format
//!
//! Every line is exactly one JSON object tagged with a `kind` field, so
//! events and snapshots share one file per session:
//!
//! ```json
//! {"kind":"event","session_sequence":7,"envelope":{...}}
//! {"kind":"snapshot","session_id":"...","root_agent_id":"...","agents":[...],"session_sequence":99,"timestamp":"..."}
//! ```
//!
//! The file is opened with `OpenOptions::append(true)` (POSIX `O_APPEND`,
//! which makes each line append atomic) and every line is produced with
//! `serde_json::to_writer` followed by a `\n`. All writes to a session are
//! routed through **one single-writer task per session file**: `append` and
//! `save_snapshot` send a command over an `mpsc` channel and await an
//! acknowledgement, so concurrent appends to the same session can never
//! interleave inside a line.
//!
//! # Durability
//!
//! The writer task calls `File::sync_data()` every [`DEFAULT_SYNC_INTERVAL`]
//! appends (configurable via [`JsonlSessionStore::with_sync_interval`]) and
//! once more when the task shuts down, bounding how much acknowledged history
//! can be lost on a crash.
//!
//! # Reads
//!
//! `load_session` performs a **full sequential scan** of the session file —
//! the JSONL store has no indexes. The **last** snapshot line in the file is
//! the effective snapshot (a later `save_snapshot` logically replaces an
//! earlier one); events whose `session_sequence` is `None` or greater than the
//! snapshot's sequence are returned as trailing events, ordered by sequence.
//!
//! # Positioning
//!
//! `JsonlSessionStore` is the default `SessionStore` for the minimal /
//! standalone build (spec §67 "Minimal/local" packaging), where a bundled
//! SQLite dependency may be undesirable: it needs only `tokio`'s fs/sync
//! features and the standard library.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use harness_protocol::ids::SessionId;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

use crate::store::{
    is_durable, DurableSessionEvent, DurableSessionSnapshot, SessionStore, StoreError,
    StoredSession,
};

/// Default number of appends between `File::sync_data()` flushes.
///
/// Bounds crash data loss to the last (at most) this many acknowledged
/// appends; the writer also flushes once on shutdown.
pub const DEFAULT_SYNC_INTERVAL: u32 = 64;

/// One line of a session's JSONL file, tagged by `kind`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum JsonlRecord {
    /// A durable event appended to the session history.
    Event(DurableSessionEvent),
    /// A session snapshot; the last line of this kind wins on load.
    Snapshot(DurableSessionSnapshot),
}

/// A write request routed to a session's single-writer task.
struct WriteCommand {
    /// The record to append as one JSONL line.
    record: JsonlRecord,
    /// Acknowledges completion (or the terminal error) to the caller.
    reply: oneshot::Sender<Result<(), StoreError>>,
}

/// [`SessionStore`] backed by append-only JSONL files, one per session.
///
/// See the [module docs](self) for the on-disk format, the single-writer
/// model, and the durability contract.
pub struct JsonlSessionStore {
    /// Directory holding one `{session_id}.jsonl` file per session.
    dir: PathBuf,
    /// Appends between `File::sync_data()` calls in each writer task.
    sync_interval: u32,
    /// The single-writer task channel for each live session.
    writers: Mutex<HashMap<SessionId, mpsc::Sender<WriteCommand>>>,
}

impl JsonlSessionStore {
    /// Creates a store rooted at `dir`, using [`DEFAULT_SYNC_INTERVAL`].
    ///
    /// The directory is created on the first write to a session.
    pub fn new(dir: impl AsRef<Path>) -> Self {
        Self::with_sync_interval(dir, DEFAULT_SYNC_INTERVAL)
    }

    /// Creates a store with a custom durability flush interval.
    ///
    /// `sync_interval` is the number of appends between `File::sync_data()`
    /// calls and must be at least `1`.
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

    /// The append-only file backing `id` (one file per session).
    fn path_for(&self, id: SessionId) -> PathBuf {
        self.dir.join(format!("{id}.jsonl"))
    }

    /// Returns — spawning it on first use — the single-writer channel for `id`.
    ///
    /// A channel whose writer task has terminated (closed sender) is replaced
    /// with a fresh one so the session remains writable.
    fn writer_for(&self, id: SessionId) -> mpsc::Sender<WriteCommand> {
        let mut writers = self
            .writers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        // `Option::cloned` clones the referent (`Sender`), never the reference.
        if let Some(tx) = writers.get(&id).filter(|tx| !tx.is_closed()).cloned() {
            return tx;
        }
        writers.remove(&id);

        let path = self.path_for(id);
        let (tx, rx) = mpsc::channel(64);
        let sync_interval = self.sync_interval;
        tokio::spawn(async move {
            writer_task(path, rx, sync_interval).await;
        });
        writers.insert(id, tx.clone());
        tx
    }

    /// Reads the complete append-only record stream for a session. Restore and
    /// reconnect-resume deliberately share this parser; callers decide
    /// whether snapshots should filter the returned event history.
    async fn read_records(
        &self,
        id: SessionId,
    ) -> Result<(Option<DurableSessionSnapshot>, Vec<DurableSessionEvent>), StoreError> {
        let path = self.path_for(id);
        let file = match tokio::fs::File::open(&path).await {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(StoreError::NotFound(id));
            }
            Err(error) => return Err(StoreError::Io(error.into())),
        };

        let mut events = Vec::new();
        let mut snapshot = None;
        let mut record_count = 0usize;
        let mut reader = BufReader::new(file);
        let mut line = String::new();

        loop {
            line.clear();
            let read = reader.read_line(&mut line).await?;
            if read == 0 {
                break;
            }
            let trimmed = line.trim_end();
            if trimmed.is_empty() {
                continue;
            }
            record_count += 1;
            let record: JsonlRecord = serde_json::from_str(trimmed).map_err(|error| {
                StoreError::InvalidState(format!(
                    "corrupt jsonl record in {}: {error}",
                    path.display()
                ))
            })?;
            match record {
                JsonlRecord::Event(event) => events.push(event),
                JsonlRecord::Snapshot(value) => snapshot = Some(value),
            }
        }

        if record_count == 0 {
            return Err(StoreError::NotFound(id));
        }
        events.sort_by_key(|event| event.session_sequence.unwrap_or(u64::MAX));
        Ok((snapshot, events))
    }
}

/// Serializes `record` as a single JSON line — `serde_json::to_writer` +
/// `\n` — and appends it to `file`.
async fn write_record(file: &mut tokio::fs::File, record: &JsonlRecord) -> Result<(), StoreError> {
    let mut line = Vec::new();
    serde_json::to_writer(&mut line, record)?;
    line.push(b'\n');
    file.write_all(&line).await?;
    Ok(())
}

/// Fails every still-queued command after a terminal writer error, so no
/// caller is left hanging on an acknowledgement.
fn fail_queued(mut rx: mpsc::Receiver<WriteCommand>, error: StoreError) {
    while let Ok(command) = rx.try_recv() {
        let _ = command.reply.send(Err(error.clone()));
    }
}

/// The single-writer task for one session file.
///
/// Owns the append-only file handle, serializes every command into one JSONL
/// line, and calls `sync_data()` every `sync_interval` appends (plus once on
/// shutdown). On a write/flush failure it acknowledges the failing command
/// with the error, fails all still-queued commands, and terminates.
async fn writer_task(path: PathBuf, mut rx: mpsc::Receiver<WriteCommand>, sync_interval: u32) {
    if let Some(parent) = path.parent() {
        if let Err(error) = tokio::fs::create_dir_all(parent).await {
            fail_queued(rx, StoreError::Io(error.into()));
            return;
        }
    }

    let mut file = match tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await
    {
        Ok(file) => file,
        Err(error) => {
            fail_queued(rx, StoreError::Io(error.into()));
            return;
        }
    };

    let mut appends_since_sync: u32 = 0;
    while let Some(command) = rx.recv().await {
        if let Err(error) = write_record(&mut file, &command.record).await {
            let _ = command.reply.send(Err(error.clone()));
            fail_queued(rx, error);
            return;
        }

        appends_since_sync = appends_since_sync.saturating_add(1);
        if appends_since_sync >= sync_interval {
            appends_since_sync = 0;
            if let Err(error) = file.sync_data().await {
                let error = StoreError::Io(error.into());
                let _ = command.reply.send(Err(error.clone()));
                fail_queued(rx, error);
                return;
            }
        }

        let _ = command.reply.send(Ok(()));
    }

    // Channel closed: every sender (and thus every store handle) is gone, so
    // flush acknowledged history to stable storage before exiting.
    let _ = file.sync_data().await;
}

#[async_trait]
impl SessionStore for JsonlSessionStore {
    async fn load_session(&self, id: SessionId) -> Result<StoredSession, StoreError> {
        let (snapshot, mut events) = self.read_records(id).await?;

        // Restore only needs events not already represented by the latest
        // snapshot. The reconnect history path below intentionally does not
        // apply this filter.
        if let Some(snap) = &snapshot {
            events.retain(|event| {
                event
                    .session_sequence
                    .map_or(true, |seq| seq > snap.session_sequence)
            });
        }

        Ok(StoredSession {
            session_id: id,
            snapshot,
            events,
        })
    }

    async fn events_since(
        &self,
        id: SessionId,
        since_seq: u64,
    ) -> Result<Vec<DurableSessionEvent>, StoreError> {
        let (_, mut events) = self.read_records(id).await?;
        events.retain(|event| event.session_sequence.is_some_and(|seq| seq > since_seq));
        Ok(events)
    }

    async fn append(&self, event: DurableSessionEvent) -> Result<(), StoreError> {
        if !is_durable(&event.envelope.event) {
            return Err(StoreError::InvalidState(format!(
                "refusing to persist ephemeral event: {:?}",
                event.envelope.event
            )));
        }
        let session_id = event.envelope.session_id;
        let tx = self.writer_for(session_id);
        let (reply, ack) = oneshot::channel();
        tx.send(WriteCommand {
            record: JsonlRecord::Event(event),
            reply,
        })
        .await
        .map_err(|_| {
            StoreError::Backend(format!(
                "session writer task for {session_id} is not running"
            ))
        })?;
        ack.await.map_err(|_| {
            StoreError::Backend(format!(
                "session writer task for {session_id} terminated before acknowledging"
            ))
        })?
    }

    async fn save_snapshot(&self, snapshot: DurableSessionSnapshot) -> Result<(), StoreError> {
        let session_id = snapshot.session_id;
        let tx = self.writer_for(session_id);
        let (reply, ack) = oneshot::channel();
        tx.send(WriteCommand {
            record: JsonlRecord::Snapshot(snapshot),
            reply,
        })
        .await
        .map_err(|_| {
            StoreError::Backend(format!(
                "session writer task for {session_id} is not running"
            ))
        })?;
        ack.await.map_err(|_| {
            StoreError::Backend(format!(
                "session writer task for {session_id} terminated before acknowledging"
            ))
        })?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_protocol::commands::AgentStatus;
    use harness_protocol::events::{AgentEvent, AgentEventEnvelope, EventVisibility};
    use harness_protocol::ids::{AgentId, EventId, RunId, Timestamp};

    /// Creates a unique scratch directory for one test.
    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "harness-session-store-jsonl-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp store dir");
        dir
    }

    /// A fully-qualified, sequenced envelope for testing.
    fn envelope(session: SessionId, seq: u64) -> AgentEventEnvelope {
        AgentEventEnvelope {
            event_id: EventId::new(),
            session_id: session,
            agent_id: AgentId::new(),
            parent_agent_id: None,
            run_id: Some(RunId::new()),
            agent_sequence: seq,
            session_sequence: Some(seq),
            timestamp: Timestamp::now(),
            visibility: EventVisibility::User,
            event: AgentEvent::StateChanged {
                from: AgentStatus::Idle,
                to: AgentStatus::PreparingContext,
            },
        }
    }

    /// A durable event with the given session sequence.
    fn event(session: SessionId, seq: u64) -> DurableSessionEvent {
        DurableSessionEvent {
            session_sequence: Some(seq),
            envelope: envelope(session, seq),
        }
    }

    /// A minimal snapshot at the given session sequence.
    fn snapshot(session: SessionId, seq: u64) -> DurableSessionSnapshot {
        DurableSessionSnapshot {
            session_id: session,
            root_agent_id: AgentId::new(),
            agents: Vec::new(),
            session_sequence: seq,
            timestamp: Timestamp::now(),
        }
    }

    #[tokio::test]
    async fn writes_one_tagged_jsonl_file_per_session() {
        let dir = temp_dir("one-file-per-session");
        let store = JsonlSessionStore::new(dir.as_path());
        let session = SessionId::new();

        store.append(event(session, 1)).await.expect("append event");
        store
            .save_snapshot(snapshot(session, 1))
            .await
            .expect("save snapshot");

        let path = dir.join(format!("{session}.jsonl"));
        let raw = tokio::fs::read_to_string(&path)
            .await
            .expect("read session file");
        let lines: Vec<&str> = raw.lines().collect();
        assert_eq!(lines.len(), 2, "one line per record: {raw}");
        assert!(
            lines[0].contains("\"kind\":\"event\""),
            "line 0: {}",
            lines[0]
        );
        assert!(
            lines[1].contains("\"kind\":\"snapshot\""),
            "line 1: {}",
            lines[1]
        );
    }

    #[tokio::test]
    async fn roundtrip_events_and_snapshot() {
        let dir = temp_dir("roundtrip");
        let store = JsonlSessionStore::new(dir.as_path());
        let session = SessionId::new();

        for seq in 1..=5 {
            store
                .append(event(session, seq))
                .await
                .expect("append event");
        }
        store
            .save_snapshot(snapshot(session, 3))
            .await
            .expect("save snapshot");

        let stored = store.load_session(session).await.expect("load session");
        assert_eq!(stored.session_id, session);

        let loaded_snapshot = stored.snapshot.expect("snapshot present");
        assert_eq!(loaded_snapshot.session_id, session);
        assert_eq!(loaded_snapshot.session_sequence, 3);

        // Only the events appended after the snapshot's sequence remain.
        let sequences: Vec<Option<u64>> =
            stored.events.iter().map(|e| e.session_sequence).collect();
        assert_eq!(sequences, vec![Some(4), Some(5)]);
        for (event, expected) in stored.events.iter().zip([4u64, 5]) {
            assert_eq!(event.envelope.agent_sequence, expected);
            assert_eq!(event.envelope.session_id, session);
        }
    }

    #[tokio::test]
    async fn load_returns_all_events_when_no_snapshot() {
        let dir = temp_dir("no-snapshot");
        let store = JsonlSessionStore::new(dir.as_path());
        let session = SessionId::new();

        store.append(event(session, 1)).await.expect("append event");
        store.append(event(session, 2)).await.expect("append event");

        let stored = store.load_session(session).await.expect("load session");
        assert!(stored.snapshot.is_none());
        let sequences: Vec<Option<u64>> =
            stored.events.iter().map(|e| e.session_sequence).collect();
        assert_eq!(sequences, vec![Some(1), Some(2)]);
    }

    #[tokio::test]
    async fn load_missing_session_is_not_found() {
        let dir = temp_dir("not-found");
        let store = JsonlSessionStore::new(dir.as_path());

        let error = store
            .load_session(SessionId::new())
            .await
            .expect_err("no file exists for an unknown session");
        assert!(matches!(error, StoreError::NotFound(_)));
    }

    #[tokio::test]
    async fn empty_session_file_is_not_found() {
        let dir = temp_dir("empty-file");
        let store = JsonlSessionStore::new(dir.as_path());
        let session = SessionId::new();

        // A zero-record file holds no stored data.
        let path = dir.join(format!("{session}.jsonl"));
        tokio::fs::write(&path, b"")
            .await
            .expect("write empty file");

        let error = store
            .load_session(session)
            .await
            .expect_err("empty file has no stored data");
        assert!(matches!(error, StoreError::NotFound(_)));
    }

    #[tokio::test]
    async fn data_survives_store_recreation() {
        let dir = temp_dir("restart");
        let session = SessionId::new();

        {
            let store = JsonlSessionStore::new(dir.as_path());
            store.append(event(session, 1)).await.expect("append event");
            store.append(event(session, 2)).await.expect("append event");
            store
                .save_snapshot(snapshot(session, 2))
                .await
                .expect("save snapshot");
        } // store dropped -> all senders dropped -> writer task flushes and exits

        let reopened = JsonlSessionStore::new(dir.as_path());
        let stored = reopened
            .load_session(session)
            .await
            .expect("reload session");
        assert_eq!(
            stored.snapshot.as_ref().map(|s| s.session_sequence),
            Some(2)
        );
        assert_eq!(
            stored.events.len(),
            0,
            "both events captured by the snapshot"
        );
    }

    #[tokio::test]
    async fn concurrent_appends_are_serialized_by_single_writer() {
        let dir = temp_dir("concurrent");
        let store = std::sync::Arc::new(JsonlSessionStore::new(dir.as_path()));
        let session = SessionId::new();

        let mut handles = Vec::new();
        for seq in 0..32 {
            let store = store.clone();
            handles.push(tokio::spawn(async move {
                store
                    .append(event(session, seq))
                    .await
                    .expect("append event");
            }));
        }
        for handle in handles {
            handle.await.expect("join append task");
        }

        // Every append must have landed as a complete, parseable line.
        let stored = store.load_session(session).await.expect("load session");
        let mut sequences: Vec<u64> = stored
            .events
            .iter()
            .map(|e| e.session_sequence.expect("sequenced event"))
            .collect();
        sequences.sort_unstable();
        assert_eq!(sequences, (0..32).collect::<Vec<u64>>());
    }

    #[tokio::test]
    async fn later_snapshot_replaces_earlier() {
        let dir = temp_dir("replace");
        let store = JsonlSessionStore::new(dir.as_path());
        let session = SessionId::new();

        store.append(event(session, 1)).await.expect("append event");
        store
            .save_snapshot(snapshot(session, 1))
            .await
            .expect("save snapshot");
        store.append(event(session, 2)).await.expect("append event");
        store
            .save_snapshot(snapshot(session, 2))
            .await
            .expect("save snapshot");

        let stored = store.load_session(session).await.expect("load session");
        assert_eq!(
            stored.snapshot.as_ref().map(|s| s.session_sequence),
            Some(2),
            "the last snapshot line wins"
        );
        assert_eq!(stored.events.len(), 0);
    }

    #[tokio::test]
    async fn unsequenced_events_are_treated_as_trailing() {
        let dir = temp_dir("unsequenced");
        let store = JsonlSessionStore::new(dir.as_path());
        let session = SessionId::new();

        store.append(event(session, 1)).await.expect("append event");
        store
            .save_snapshot(snapshot(session, 1))
            .await
            .expect("save snapshot");

        let mut unsequenced = event(session, 2);
        unsequenced.session_sequence = None;
        unsequenced.envelope.session_sequence = None;
        store
            .append(unsequenced)
            .await
            .expect("append unsequenced event");

        let stored = store.load_session(session).await.expect("load session");
        assert_eq!(stored.events.len(), 1);
        assert_eq!(stored.events[0].session_sequence, None);
    }

    #[tokio::test]
    async fn corrupt_record_line_is_reported() {
        let dir = temp_dir("corrupt");
        let store = JsonlSessionStore::new(dir.as_path());
        let session = SessionId::new();

        store.append(event(session, 1)).await.expect("append event");

        // Append a garbage line directly to the session file.
        let path = dir.join(format!("{session}.jsonl"));
        let mut file = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .await
            .expect("open session file");
        file.write_all(b"{ not json }\n")
            .await
            .expect("write garbage");
        file.sync_data().await.expect("flush garbage");

        let error = store
            .load_session(session)
            .await
            .expect_err("corrupt line must surface as an error");
        assert!(matches!(error, StoreError::InvalidState(_)));
    }
}
