//! Durable orchestration events and snapshots.
//!
//! The runner commits every transition here *before* publishing its events or
//! dispatching its effects, so a subscriber never observes a state the store
//! could lose, `RunCompleted` is only published once the final result is
//! durable, and a restore never re-runs a step whose success was committed.

use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

use async_trait::async_trait;
use harness_core::orchestration::{
    OrchestrationDefinitionId, OrchestrationEvent, OrchestrationResult, OrchestrationRunId,
    OrchestrationRunState,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A persisted orchestration event with run-level correlation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrchestrationEventEnvelope {
    pub run_id: OrchestrationRunId,
    pub definition_id: OrchestrationDefinitionId,
    pub revision: u64,
    /// Monotonic, gap-free per run, starting at 1.
    pub sequence: u64,
    pub timestamp_ms: u64,
    pub event: OrchestrationEvent,
}

/// Everything needed to resume a run at a step boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrchestrationSnapshot {
    pub state: OrchestrationRunState,
    /// Sequence of the last event committed with this snapshot.
    pub last_sequence: u64,
    /// Run time consumed so far, carried across restores for the elapsed budget.
    pub elapsed_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<OrchestrationResult>,
}

#[derive(Debug, Error)]
pub enum OrchestrationStoreError {
    #[error("orchestration store I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("orchestration store encoding error: {0}")]
    Encoding(#[from] serde_json::Error),
    #[error("invalid run id {0:?} for a file store")]
    InvalidRunId(String),
    #[error("event sequence gap for run {run_id}: expected {expected}, got {actual}")]
    SequenceGap {
        run_id: String,
        expected: u64,
        actual: u64,
    },
}

#[async_trait]
pub trait OrchestrationStore: Send + Sync {
    /// Atomically (from the runner's perspective) append `events` and replace
    /// the run's snapshot. Must not return until both are durable.
    async fn commit(
        &self,
        events: &[OrchestrationEventEnvelope],
        snapshot: &OrchestrationSnapshot,
    ) -> Result<(), OrchestrationStoreError>;

    async fn load_snapshot(
        &self,
        run_id: &OrchestrationRunId,
    ) -> Result<Option<OrchestrationSnapshot>, OrchestrationStoreError>;

    async fn load_events(
        &self,
        run_id: &OrchestrationRunId,
    ) -> Result<Vec<OrchestrationEventEnvelope>, OrchestrationStoreError>;
}

#[derive(Default)]
struct MemoryRun {
    events: Vec<OrchestrationEventEnvelope>,
    snapshot: Option<OrchestrationSnapshot>,
}

/// Process-local store, for tests and embedders that persist elsewhere.
#[derive(Default)]
pub struct InMemoryOrchestrationStore {
    runs: Mutex<BTreeMap<OrchestrationRunId, MemoryRun>>,
}

impl InMemoryOrchestrationStore {
    pub fn new() -> Self {
        Self::default()
    }
}

fn check_sequence(
    run_id: &OrchestrationRunId,
    last: u64,
    events: &[OrchestrationEventEnvelope],
) -> Result<(), OrchestrationStoreError> {
    for (offset, event) in events.iter().enumerate() {
        let expected = last + 1 + offset as u64;
        if event.sequence != expected {
            return Err(OrchestrationStoreError::SequenceGap {
                run_id: run_id.to_string(),
                expected,
                actual: event.sequence,
            });
        }
    }
    Ok(())
}

#[async_trait]
impl OrchestrationStore for InMemoryOrchestrationStore {
    async fn commit(
        &self,
        events: &[OrchestrationEventEnvelope],
        snapshot: &OrchestrationSnapshot,
    ) -> Result<(), OrchestrationStoreError> {
        let mut runs = self.runs.lock().expect("store lock poisoned");
        let run = runs.entry(snapshot.state.run_id.clone()).or_default();
        let last = run.events.last().map_or(0, |event| event.sequence);
        check_sequence(&snapshot.state.run_id, last, events)?;
        run.events.extend_from_slice(events);
        run.snapshot = Some(snapshot.clone());
        Ok(())
    }

    async fn load_snapshot(
        &self,
        run_id: &OrchestrationRunId,
    ) -> Result<Option<OrchestrationSnapshot>, OrchestrationStoreError> {
        let runs = self.runs.lock().expect("store lock poisoned");
        Ok(runs.get(run_id).and_then(|run| run.snapshot.clone()))
    }

    async fn load_events(
        &self,
        run_id: &OrchestrationRunId,
    ) -> Result<Vec<OrchestrationEventEnvelope>, OrchestrationStoreError> {
        let runs = self.runs.lock().expect("store lock poisoned");
        Ok(runs
            .get(run_id)
            .map(|run| run.events.clone())
            .unwrap_or_default())
    }
}

/// One directory per run: `events.jsonl` (append-only, fsynced) and
/// `snapshot.json` (replaced atomically via write-to-temp + rename).
///
/// Events are appended before the snapshot is replaced, so after a crash the
/// log may run ahead of the snapshot but never behind it; the snapshot is the
/// restore point and later events are informational.
pub struct FileOrchestrationStore {
    root: PathBuf,
    /// Last committed sequence per run, loaded from disk on first write.
    /// The lock also serializes writers within this process; the layout is
    /// not meant for concurrent writers across processes.
    last_sequences: tokio::sync::Mutex<BTreeMap<OrchestrationRunId, u64>>,
}

impl FileOrchestrationStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            last_sequences: tokio::sync::Mutex::new(BTreeMap::new()),
        }
    }

    fn run_dir(&self, run_id: &OrchestrationRunId) -> Result<PathBuf, OrchestrationStoreError> {
        let id = run_id.as_str();
        let safe = !id.is_empty()
            && id != "."
            && id != ".."
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        if !safe {
            return Err(OrchestrationStoreError::InvalidRunId(id.to_string()));
        }
        Ok(self.root.join(id))
    }
}

fn read_events(path: &Path) -> Result<Vec<OrchestrationEventEnvelope>, OrchestrationStoreError> {
    match std::fs::read_to_string(path) {
        Ok(contents) => contents
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).map_err(Into::into))
            .collect(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

/// Returns the new last committed sequence.
fn commit_blocking(
    dir: &Path,
    last: Option<u64>,
    events: &[OrchestrationEventEnvelope],
    snapshot: &OrchestrationSnapshot,
) -> Result<u64, OrchestrationStoreError> {
    std::fs::create_dir_all(dir)?;
    let events_path = dir.join("events.jsonl");
    let last = match last {
        Some(last) => last,
        None => read_events(&events_path)?
            .last()
            .map_or(0, |event| event.sequence),
    };
    if !events.is_empty() {
        check_sequence(&snapshot.state.run_id, last, events)?;
        let mut buffer = Vec::new();
        for event in events {
            serde_json::to_writer(&mut buffer, event)?;
            buffer.push(b'\n');
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&events_path)?;
        file.write_all(&buffer)?;
        file.sync_all()?;
    }
    let temp = dir.join("snapshot.json.tmp");
    {
        let mut file = std::fs::File::create(&temp)?;
        serde_json::to_writer_pretty(&mut file, snapshot)?;
        file.sync_all()?;
    }
    std::fs::rename(&temp, dir.join("snapshot.json"))?;
    Ok(events.last().map_or(last, |event| event.sequence))
}

#[async_trait]
impl OrchestrationStore for FileOrchestrationStore {
    async fn commit(
        &self,
        events: &[OrchestrationEventEnvelope],
        snapshot: &OrchestrationSnapshot,
    ) -> Result<(), OrchestrationStoreError> {
        let run_id = snapshot.state.run_id.clone();
        let dir = self.run_dir(&run_id)?;
        let mut last_sequences = self.last_sequences.lock().await;
        let last = last_sequences.get(&run_id).copied();
        let events = events.to_vec();
        let snapshot = snapshot.clone();
        let committed =
            tokio::task::spawn_blocking(move || commit_blocking(&dir, last, &events, &snapshot))
                .await
                .map_err(|error| std::io::Error::other(error.to_string()))??;
        last_sequences.insert(run_id, committed);
        Ok(())
    }

    async fn load_snapshot(
        &self,
        run_id: &OrchestrationRunId,
    ) -> Result<Option<OrchestrationSnapshot>, OrchestrationStoreError> {
        let path = self.run_dir(run_id)?.join("snapshot.json");
        tokio::task::spawn_blocking(move || match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        })
        .await
        .map_err(|error| std::io::Error::other(error.to_string()))?
    }

    async fn load_events(
        &self,
        run_id: &OrchestrationRunId,
    ) -> Result<Vec<OrchestrationEventEnvelope>, OrchestrationStoreError> {
        let path = self.run_dir(run_id)?.join("events.jsonl");
        tokio::task::spawn_blocking(move || read_events(&path))
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))?
    }
}
