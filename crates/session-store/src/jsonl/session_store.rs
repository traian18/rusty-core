//! [`SessionStore`] for [`JsonlSessionStore`]: each operation reads or appends through `record`
//! and `writer`.

use async_trait::async_trait;
use rusty_protocol::ids::SessionId;

use super::{record::JsonlRecord, writer::write_through, JsonlSessionStore};
use super::{session_id_of_file, stored_session};
use crate::store::{
    is_durable, summarize_session, DurableSessionEvent, DurableSessionSnapshot, RawRecord,
    SessionStore, SessionSummary, StoreError, StoredSession,
};

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
            let Some(id) = session_id_of_file(&entry.path()) else {
                continue;
            };
            let Ok(records) = self.read(id).await else {
                continue;
            };
            let stored = stored_session(id, records);
            summaries.extend(summarize_session(&stored));
        }
        summaries.sort_by_key(|summary| std::cmp::Reverse(summary.updated_at));
        Ok(summaries)
    }

    async fn load_session(&self, id: SessionId) -> Result<StoredSession, StoreError> {
        let mut stored = stored_session(id, self.read(id).await?);
        if let Some(snapshot) = &stored.snapshot {
            let checkpoint = snapshot.session_sequence;
            stored.events.retain(|event| {
                event
                    .session_sequence
                    .is_none_or(|sequence| sequence > checkpoint)
            });
        }
        Ok(stored)
    }

    async fn events_since(
        &self,
        id: SessionId,
        since_seq: u64,
    ) -> Result<Vec<DurableSessionEvent>, StoreError> {
        let mut events = self.read(id).await?.events;
        events.retain(|event| {
            event
                .session_sequence
                .is_some_and(|sequence| sequence > since_seq)
        });
        Ok(events)
    }

    async fn current_sequence(&self, id: SessionId) -> Result<u64, StoreError> {
        let records = match self.read(id).await {
            Ok(records) => records,
            Err(StoreError::NotFound(_)) => return Ok(0),
            Err(error) => return Err(error),
        };
        let snapshot_max = records
            .snapshot
            .map_or(0, |snapshot| snapshot.session_sequence);
        let event_max = records
            .events
            .iter()
            .filter_map(|event| event.session_sequence)
            .max()
            .unwrap_or(0);
        Ok(snapshot_max.max(event_max))
    }

    async fn raw_records(&self, id: SessionId) -> Result<Vec<RawRecord>, StoreError> {
        let records = self.read(id).await?;
        let mut raw: Vec<RawRecord> = records.events.into_iter().map(RawRecord::Event).collect();
        raw.extend(records.snapshot.map(RawRecord::Snapshot));
        Ok(raw)
    }

    async fn append(&self, event: DurableSessionEvent) -> Result<(), StoreError> {
        if !is_durable(&event.envelope.event) {
            return Err(StoreError::InvalidState(format!(
                "refusing to persist ephemeral event: {:?}",
                event.envelope.event
            )));
        }
        let session_id = event.envelope.session_id;
        write_through(
            &self.writer_for(session_id),
            session_id,
            JsonlRecord::Event(event),
        )
        .await
    }

    async fn save_snapshot(&self, snapshot: DurableSessionSnapshot) -> Result<(), StoreError> {
        let session_id = snapshot.session_id;
        write_through(
            &self.writer_for(session_id),
            session_id,
            JsonlRecord::Snapshot(snapshot),
        )
        .await
    }
}
