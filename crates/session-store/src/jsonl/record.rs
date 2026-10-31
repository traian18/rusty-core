//! Reading a session's file: one JSON record per line, either an event or a snapshot.

use std::path::Path;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};

use rusty_protocol::ids::SessionId;

use crate::store::{DurableSessionEvent, DurableSessionSnapshot, StoreError};

/// One line of a session file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum JsonlRecord {
    Event(DurableSessionEvent),
    Snapshot(DurableSessionSnapshot),
}

/// What a session file holds: the latest snapshot and the events, in sequence order.
pub(super) struct SessionRecords {
    pub snapshot: Option<DurableSessionSnapshot>,
    pub events: Vec<DurableSessionEvent>,
}

/// Reads the session file at `path`; [`StoreError::NotFound`] when it is missing or has no lines.
///
/// A last line without a newline that does not parse is the trace of a crash in the middle of an
/// append. It is dropped with a warning. A bad line anywhere else is corruption and fails.
pub(super) async fn read_records(path: &Path, id: SessionId) -> Result<SessionRecords, StoreError> {
    let file = open_existing(path, id).await?;
    let mut reader = BufReader::new(file);
    let mut records = SessionRecords {
        snapshot: None,
        events: Vec::new(),
    };
    let mut non_blank_lines = 0usize;
    let mut line = String::new();

    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
            break;
        }
        if line.trim_end().is_empty() {
            continue;
        }
        non_blank_lines += 1;
        match parse_line(&line, path)? {
            Some(JsonlRecord::Event(event)) => records.events.push(event),
            Some(JsonlRecord::Snapshot(snapshot)) => records.snapshot = Some(snapshot),
            None => {}
        }
    }

    if non_blank_lines == 0 {
        return Err(StoreError::NotFound(id));
    }
    records
        .events
        .sort_by_key(|event| event.session_sequence.unwrap_or(u64::MAX));
    Ok(records)
}

async fn open_existing(path: &Path, id: SessionId) -> Result<tokio::fs::File, StoreError> {
    match tokio::fs::File::open(path).await {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(StoreError::NotFound(id)),
        Err(error) => Err(StoreError::Io(error.into())),
    }
}

fn parse_line(line: &str, path: &Path) -> Result<Option<JsonlRecord>, StoreError> {
    match serde_json::from_str(line.trim_end()) {
        Ok(record) => Ok(Some(record)),
        Err(error) if !line.ends_with('\n') => {
            tracing::warn!(
                path = %path.display(),
                %error,
                "dropping truncated trailing jsonl record (crash mid-append)"
            );
            Ok(None)
        }
        Err(error) => Err(StoreError::InvalidState(format!(
            "corrupt jsonl record in {}: {error}",
            path.display()
        ))),
    }
}
