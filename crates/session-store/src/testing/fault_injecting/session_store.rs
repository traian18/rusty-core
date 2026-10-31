//! [`SessionStore`] for [`FaultInjectingStore`].

use async_trait::async_trait;
use rusty_protocol::ids::SessionId;

use super::FaultInjectingStore;
use crate::store::{
    DurableSessionEvent, DurableSessionSnapshot, RawRecord, SessionStore, StoreError, StoredSession,
};

#[async_trait]
impl SessionStore for FaultInjectingStore {
    async fn load_session(&self, id: SessionId) -> Result<StoredSession, StoreError> {
        if let Some(error) = self.check("load_session") {
            return Err(error);
        }
        self.inner.load_session(id).await
    }

    async fn current_sequence(&self, id: SessionId) -> Result<u64, StoreError> {
        if let Some(error) = self.check("current_sequence") {
            return Err(error);
        }
        self.inner.current_sequence(id).await
    }

    async fn raw_records(&self, id: SessionId) -> Result<Vec<RawRecord>, StoreError> {
        if let Some(error) = self.check("raw_records") {
            return Err(error);
        }
        self.inner.raw_records(id).await
    }

    async fn append(&self, event: DurableSessionEvent) -> Result<(), StoreError> {
        if let Some(error) = self.check("append") {
            return Err(error);
        }
        self.inner.append(event).await
    }

    async fn save_snapshot(&self, snapshot: DurableSessionSnapshot) -> Result<(), StoreError> {
        if let Some(error) = self.check("save_snapshot") {
            return Err(error);
        }
        self.inner.save_snapshot(snapshot).await
    }

    async fn list_sessions(&self) -> Result<Vec<crate::store::SessionSummary>, StoreError> {
        if let Some(error) = self.check("list_sessions") {
            return Err(error);
        }
        self.inner.list_sessions().await
    }

    async fn events_since(
        &self,
        id: SessionId,
        since_seq: u64,
    ) -> Result<Vec<DurableSessionEvent>, StoreError> {
        if let Some(error) = self.check("events_since") {
            return Err(error);
        }
        self.inner.events_since(id, since_seq).await
    }

    async fn prune_events_before(&self, id: SessionId, sequence: u64) -> Result<u64, StoreError> {
        if let Some(error) = self.check("prune_events_before") {
            return Err(error);
        }
        self.inner.prune_events_before(id, sequence).await
    }
}
