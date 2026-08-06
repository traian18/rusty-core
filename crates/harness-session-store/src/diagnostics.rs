//! Read-only diagnostics and explicit repair tooling (RC-305).
//!
//! [`diagnose_store`] scans every session's raw record stream (see
//! [`SessionStore::raw_records`]) and reports, per session: snapshot
//! presence/version/sequence, durable event counts and sequence extents,
//! sequence gaps, duplicate sequences, and corrupt records — all without
//! mutating anything.
//!
//! [`repair_jsonl`] is the explicit repair tool for JSONL stores: it rewrites
//! a session file, dropping unparseable trailing records (a truncated write
//! left by a crash mid-append) while preserving every parseable record in
//! order. It never runs automatically — a crash may have dropped acknowledged
//! history, so repair is a conscious operator action.

use std::path::Path;

use harness_protocol::ids::SessionId;

use crate::store::{RawRecord, SessionStore, StoreError};

/// Per-session diagnostics computed from the raw record stream.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionDiagnostics {
    /// The session.
    pub session_id: SessionId,
    /// Whether the session has a snapshot checkpoint.
    pub has_snapshot: bool,
    /// The latest snapshot's sequence, if any.
    pub snapshot_sequence: Option<u64>,
    /// The latest snapshot's schema version, if any.
    pub snapshot_version: Option<u64>,
    /// Number of durable events.
    pub durable_event_count: u64,
    /// The first durable sequence, if any.
    pub first_sequence: Option<u64>,
    /// The last durable sequence, if any.
    pub last_sequence: Option<u64>,
    /// Sequence gaps as `(after_sequence, gap_size)` in store order.
    pub sequence_gaps: Vec<(u64, u64)>,
    /// Sequences that appear more than once.
    pub duplicate_sequences: Vec<u64>,
    /// Corrupt records (unparseable payloads) found in the stream.
    pub corrupt_records: u64,
    /// Trailing records that could not be parsed (truncated writes).
    pub trailing_records: u64,
}

impl SessionDiagnostics {
    /// `true` when the session's durable stream is internally consistent.
    pub fn is_healthy(&self) -> bool {
        self.duplicate_sequences.is_empty()
            && self.corrupt_records == 0
            && self.trailing_records == 0
    }
}

/// Diagnostics over every session a store reports.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoreDiagnostics {
    /// Per-session diagnostics.
    pub sessions: Vec<SessionDiagnostics>,
    /// Sessions whose record stream could not be read at all:
    /// `(session_id, reason)`.
    pub unreadable: Vec<(SessionId, String)>,
}

/// Scans every session known to `store` and computes read-only diagnostics.
pub async fn diagnose_store(store: &dyn SessionStore) -> StoreDiagnostics {
    let mut diagnostics = StoreDiagnostics::default();
    let summaries = match store.list_sessions().await {
        Ok(summaries) => summaries,
        Err(error) => {
            diagnostics.unreadable.push((
                SessionId::new(),
                format!("list_sessions failed: {error}"),
            ));
            return diagnostics;
        }
    };

    for summary in summaries {
        let session_id = summary.session_id;
        match diagnose_session(store, session_id).await {
            Ok(report) => diagnostics.sessions.push(report),
            Err(error) => diagnostics
                .unreadable
                .push((session_id, error.to_string())),
        }
    }
    diagnostics
}

/// Computes diagnostics for one session without mutating anything.
pub async fn diagnose_session(
    store: &dyn SessionStore,
    session_id: SessionId,
) -> Result<SessionDiagnostics, StoreError> {
    let records = store.raw_records(session_id).await?;

    let mut report = SessionDiagnostics {
        session_id,
        ..Default::default()
    };

    // Track every durable sequence for gap/duplicate detection.
    let mut sequences: Vec<u64> = Vec::new();

    for record in records {
        match record {
            RawRecord::Snapshot(snapshot) => {
                report.has_snapshot = true;
                report.snapshot_sequence = Some(snapshot.session_sequence);
                report.snapshot_version = Some(snapshot.schema_version);
            }
            RawRecord::Event(event) => {
                match event.session_sequence {
                    Some(sequence) => sequences.push(sequence),
                    None => {
                        // A durable event without a final sequence is corrupt.
                        report.corrupt_records += 1;
                    }
                }
            }
        }
    }

    sequences.sort_unstable();
    report.durable_event_count = sequences.len() as u64;
    report.first_sequence = sequences.first().copied();
    report.last_sequence = sequences.last().copied();

    // Duplicates.
    let mut seen: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
    for sequence in &sequences {
        *seen.entry(*sequence).or_insert(0) += 1;
    }
    for (sequence, count) in seen {
        if count > 1 {
            report.duplicate_sequences.push(sequence);
        }
    }
    report.duplicate_sequences.sort_unstable();

    // Gaps (strictly increasing scan; first event starts the stream).
    let mut previous: Option<u64> = None;
    for sequence in &sequences {
        if let Some(prev) = previous {
            if *sequence > prev.saturating_add(1) {
                report
                    .sequence_gaps
                    .push((prev, sequence.saturating_sub(prev)));
            }
        }
        previous = Some(*sequence);
    }

    Ok(report)
}

/// Outcome of a JSONL repair operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairReport {
    /// Records kept after repair (events and snapshots).
    pub kept_records: u64,
    /// Records removed because they could not be parsed.
    pub removed_records: u64,
    /// `true` when the file was rewritten (corruption was found).
    pub rewritten: bool,
}

/// Repairs a JSONL session file, dropping unparseable trailing records.
///
/// The file is rewritten only when corruption is found: every parseable line
/// is preserved in order, and unparseable **trailing** lines (a truncated
/// write left by a crash) are dropped. The rewrite is atomic (temp file +
/// rename), so a crash during repair never destroys the previous file. An
/// unparseable line in the middle of the file is **not** silently dropped:
/// it is reported through [`StoreError::InvalidState`] so an operator can
/// investigate rather than lose audit history.
///
/// This is explicit repair tooling — it is never invoked automatically by
/// the store.
pub async fn repair_jsonl(path: &Path) -> Result<RepairReport, StoreError> {
    let content = match tokio::fs::read_to_string(path).await {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(StoreError::NotFound(SessionId::new()));
        }
        Err(error) => return Err(StoreError::Io(error.into())),
    };

    let mut kept: Vec<&str> = Vec::new();
    let mut removed = 0u64;
    let mut lines = content.lines().peekable();
    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let is_last = lines.peek().is_none();
        match serde_json::from_str::<serde_json::Value>(trimmed) {
            Ok(_) => kept.push(line),
            Err(_) if is_last => {
                // Truncated trailing write: drop it.
                removed += 1;
            }
            Err(_) => {
                return Err(StoreError::InvalidState(format!(
                    "corrupt non-trailing record in {} (line {}); refusing to drop it automatically",
                    path.display(),
                    kept.len() + removed as usize + 1,
                )));
            }
        }
    }

    if removed == 0 {
        return Ok(RepairReport {
            kept_records: kept.len() as u64,
            removed_records: 0,
            rewritten: false,
        });
    }

    // Atomic rewrite: temp file in the same directory, then rename.
    let temp_path = path.with_extension("repair.tmp");
    let mut out = String::new();
    for line in &kept {
        out.push_str(line);
        out.push('\n');
    }
    tokio::fs::write(&temp_path, out).await?;
    tokio::fs::rename(&temp_path, path).await?;

    Ok(RepairReport {
        kept_records: kept.len() as u64,
        removed_records: removed,
        rewritten: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn repair_drops_only_trailing_truncated_record() {
        let dir = std::env::temp_dir().join(format!(
            "harness-diagnostics-{}-{}",
            std::process::id(),
            harness_protocol::ids::EventId::new()
        ));
        tokio::fs::create_dir_all(&dir).await.expect("create dir");
        let path = dir.join("session.jsonl");
        // Two good records plus a truncated third line (no trailing newline).
        let content = "{\"kind\":\"event\",\"session_sequence\":1,\"envelope\":{\"event\":\"state_changed\"}}\n{\"kind\":\"event\",\"session_sequence\":2,\"envelope\":{\"event\":\"state_changed\"}}\n{\"kind\":\"event\",\"session_seq";
        tokio::fs::write(&path, content).await.expect("write");

        let report = repair_jsonl(&path).await.expect("repair");
        assert_eq!(report.kept_records, 2);
        assert_eq!(report.removed_records, 1);
        assert!(report.rewritten);

        let repaired = tokio::fs::read_to_string(&path).await.expect("read repaired");
        assert_eq!(repaired.lines().count(), 2);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn repair_rejects_corrupt_middle_record() {
        let dir = std::env::temp_dir().join(format!(
            "harness-diagnostics-{}-{}",
            std::process::id(),
            harness_protocol::ids::EventId::new()
        ));
        tokio::fs::create_dir_all(&dir).await.expect("create dir");
        let path = dir.join("session.jsonl");
        let content =
            "{\"kind\":\"event\",\"session_sequence\":1,\"envelope\":{}}\nNOT JSON\n{\"kind\":\"event\",\"session_sequence\":2,\"envelope\":{}}\n";
        tokio::fs::write(&path, content).await.expect("write");

        let error = repair_jsonl(&path).await.expect_err("middle corruption");
        assert!(matches!(error, StoreError::InvalidState(_)));
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn repair_is_a_noop_on_clean_file() {
        let dir = std::env::temp_dir().join(format!(
            "harness-diagnostics-{}-{}",
            std::process::id(),
            harness_protocol::ids::EventId::new()
        ));
        tokio::fs::create_dir_all(&dir).await.expect("create dir");
        let path = dir.join("session.jsonl");
        tokio::fs::write(&path, "{\"kind\":\"event\",\"session_sequence\":1}\n")
            .await
            .expect("write");

        let report = repair_jsonl(&path).await.expect("repair");
        assert!(!report.rewritten);
        assert_eq!(report.removed_records, 0);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
