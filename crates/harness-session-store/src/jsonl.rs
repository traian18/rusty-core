

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Mutex;

use async_trait::async_trait;
use harness_protocol::ids::SessionId;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

use crate::store::{
    is_durable, summarize_session, DurableSessionEvent, DurableSessionSnapshot, RawRecord,
    SessionStore, SessionSummary, StoreError, StoredSession,
};

pub const DEFAULT_SYNC_INTERVAL: u32 = 64;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum JsonlRecord {
    Event(DurableSessionEvent),
    Snapshot(DurableSessionSnapshot),
}

struct WriteCommand {
    record: JsonlRecord,
    reply: oneshot::Sender<Result<(), StoreError>>,
}

pub struct JsonlSessionStore {
    dir: PathBuf,
    sync_interval: u32,
    writers: Mutex<HashMap<SessionId, mpsc::Sender<WriteCommand>>>,
}

impl JsonlSessionStore {
    pub fn new(dir: impl AsRef<Path>) -> Self {
        Self::with_sync_interval(dir, DEFAULT_SYNC_INTERVAL)
    }

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

    fn writer_for(&self, id: SessionId) -> mpsc::Sender<WriteCommand> {
        let mut writers = self
            .writers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

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
            let line_had_newline = line.ends_with('\n');
            let record: JsonlRecord = match serde_json::from_str(trimmed) {
                Ok(record) => record,
                Err(error) if !line_had_newline => {
                    tracing::warn!(
                        path = %path.display(),
                        %error,
                        "dropping truncated trailing jsonl record (crash mid-append)"
                    );
                    continue;
                }
                Err(error) => {
                    return Err(StoreError::InvalidState(format!(
                        "corrupt jsonl record in {}: {error}",
                        path.display()
                    )));
                }
            };
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

async fn write_record(file: &mut tokio::fs::File, record: &JsonlRecord) -> Result<(), StoreError> {
    let mut line = Vec::new();
    serde_json::to_writer(&mut line, record)?;
    line.push(b'\n');
    file.write_all(&line).await?;
    Ok(())
}

fn fail_queued(mut rx: mpsc::Receiver<WriteCommand>, error: StoreError) {
    while let Ok(command) = rx.try_recv() {
        let _ = command.reply.send(Err(error.clone()));
    }
}

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

    let _ = file.sync_data().await;
}

#[async_trait]
impl SessionStore for JsonlSessionStore {
    async fn list_sessions(&self) -> Result<Vec<SessionSummary>, StoreError> {
        let mut entries = match tokio::fs::read_dir(&self.dir).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut summaries = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
                continue;
            };
            let Ok(id) = SessionId::from_str(stem) else {
                continue;
            };
            let Ok((snapshot, events)) = self.read_records(id).await else {
                continue;
            };
            let stored = StoredSession {
                session_id: id,
                snapshot,
                events,
            };
            if let Some(summary) = summarize_session(&stored) {
                summaries.push(summary);
            }
        }
        summaries.sort_by_key(|summary| std::cmp::Reverse(summary.updated_at));
        Ok(summaries)
    }

    async fn load_session(&self, id: SessionId) -> Result<StoredSession, StoreError> {
        let (snapshot, mut events) = self.read_records(id).await?;

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

    async fn current_sequence(&self, id: SessionId) -> Result<u64, StoreError> {
        let (snapshot, events) = match self.read_records(id).await {
            Ok(records) => records,
            Err(StoreError::NotFound(_)) => (None, Vec::new()),
            Err(error) => return Err(error),
        };
        let snapshot_max = snapshot
            .map(|snapshot| snapshot.session_sequence)
            .unwrap_or(0);
        let event_max = events
            .iter()
            .filter_map(|event| event.session_sequence)
            .max()
            .unwrap_or(0);
        Ok(snapshot_max.max(event_max))
    }

    async fn raw_records(&self, id: SessionId) -> Result<Vec<RawRecord>, StoreError> {
        let (snapshot, events) = self.read_records(id).await?;
        let mut records: Vec<RawRecord> = events.into_iter().map(RawRecord::Event).collect();
        if let Some(snapshot) = snapshot {
            records.push(RawRecord::Snapshot(snapshot));
        }
        Ok(records)
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
    use harness_protocol::{
        commands::AgentStatus,
        events::{AgentEvent, AgentEventEnvelope, EventVisibility},
        ids::{AgentId, EventId, RunId, Timestamp},
    };

    fn temp_dir() -> PathBuf {
        std::env::temp_dir().join(format!(
            "harness-jsonl-test-{}-{}",
            std::process::id(),
            EventId::new()
        ))
    }

    fn event(session_id: SessionId, sequence: u64) -> DurableSessionEvent {
        DurableSessionEvent {
            envelope: AgentEventEnvelope {
                event_id: EventId::new(),
                session_id,
                agent_id: AgentId::new(),
                parent_agent_id: None,
                run_id: Some(RunId::new()),
                agent_sequence: sequence,
                session_sequence: Some(sequence),
                timestamp: Timestamp::now(),
                visibility: EventVisibility::User,
                event: AgentEvent::StateChanged {
                    from: AgentStatus::Idle,
                    to: AgentStatus::PreparingContext,
                },
            },
            session_sequence: Some(sequence),
        }
    }

    #[tokio::test]
    async fn appends_and_loads_a_session() {
        let directory = temp_dir();
        let store = JsonlSessionStore::with_sync_interval(&directory, 1);
        let session = SessionId::new();
        store.append(event(session, 1)).await.expect("append");
        let stored = store.load_session(session).await.expect("load");
        assert_eq!(stored.events.len(), 1);
        assert!(stored.snapshot.is_none());
        let _ = tokio::fs::remove_dir_all(directory).await;
    }

    #[tokio::test]
    async fn missing_session_is_reported() {
        let store = JsonlSessionStore::new(temp_dir());
        let error = store.load_session(SessionId::new()).await.expect_err("missing");
        assert!(matches!(error, StoreError::NotFound(_)));
    }

    #[tokio::test]
    async fn current_sequence_is_zero_for_fresh_session() {
        let store = JsonlSessionStore::new(temp_dir());
        assert_eq!(store.current_sequence(SessionId::new()).await.expect("sequence"), 0);
    }

    #[tokio::test]
    async fn current_sequence_resumes_from_snapshot_and_events() {
        let directory = temp_dir();
        let store = JsonlSessionStore::with_sync_interval(&directory, 1);
        let session = SessionId::new();
        store.append(event(session, 1)).await.expect("append");
        store.append(event(session, 2)).await.expect("append");
        assert_eq!(store.current_sequence(session).await.expect("sequence"), 2);
        let _ = tokio::fs::remove_dir_all(directory).await;
    }

    #[tokio::test]
    async fn truncated_trailing_line_does_not_corrupt_session() {
        let directory = temp_dir();
        let store = JsonlSessionStore::with_sync_interval(&directory, 1);
        let session = SessionId::new();
        store.append(event(session, 1)).await.expect("append");
        let path = store.path_for(session);
        let mut file = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .await
            .expect("open");
        file.write_all(b"{\"kind\":\"event\"").await.expect("truncate");
        drop(file);
        let stored = store.load_session(session).await.expect("load");
        assert_eq!(stored.events.len(), 1);
        let _ = tokio::fs::remove_dir_all(directory).await;
    }
}
