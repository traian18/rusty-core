//! Durable session-store contract and serializable restore state.
//!
//! This module defines the [`SessionStore`] trait — the single durability
//! boundary every runtime mutation crosses — plus the serializable payloads
//! that cross it: [`DurableSessionEvent`], [`DurableSessionSnapshot`], and
//! the [`StoredSession`] reconstruction returned by
//! [`SessionStore::load_session`].
//!
//! # RC-300 contract additions
//!
//! The trait gained three RC-300 primitives used by the authoritative commit
//! boundary ([`crate::commit::SessionCommitter`]), trailing replay
//! ([`crate::replay`]), and retention/diagnostics ([`crate::retention`],
//! [`crate::diagnostics`]):
//!
//! - [`SessionStore::current_sequence`] — the highest committed session
//!   sequence for a session (the durable sequencer resume point). Returns
//!   `0` for an unknown session so a fresh `SessionCommitter` can resume
//!   from `1` without a "not found" probe.
//! - [`SessionStore::raw_records`] — the unprocessed record stream (events
//!   *and* snapshots, in store order) used by read-only diagnostics and by
//!   cursor/audit tooling that must not rely on the snapshot-cutoff view.
//! - [`SessionStore::prune_events_before`] — an explicit, opt-in retention
//!   operation that removes durable events at or below a sequence. The
//!   default implementation rejects the call so a store that has not opted
//!   in can never silently discard replay/audit history.
//!
//! [`DurableSessionSnapshot`] carries a [`DurableSessionMetadata`] block that
//! records the non-secret host dependencies the snapshot was taken under
//! (workspace identity, integration/credential/tool references) plus
//! compaction lineage, so [`crate::resolver`] can verify a restore against
//! the *current* host instead of silently substituting fakes, and so
//! compaction history is never lost. Secrets never appear in this block.

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use harness_protocol::{
    backend::BackendBinding,
    commands::{AgentError, AgentOperation, AgentStatus},
    events::{AgentEvent, AgentEventEnvelope},
    ids::{AgentId, PermissionId, RunId, SessionId, Timestamp, ToolCallId},
    messages::{AgentMessage, ContentBlock, MessageRole},
    tools::ToolCall,
    usage::AgentBudget,
};

#[derive(Debug, Clone, thiserror::Error)]
pub enum StoreError {
    #[error("session not found: {0}")]
    NotFound(SessionId),
    #[error("durable payload serialization error: {0}")]
    Serialization(Arc<serde_json::Error>),
    #[error("store io error: {0}")]
    Io(Arc<std::io::Error>),
    #[error("store backend error: {0}")]
    Backend(String),
    #[error("invalid store state: {0}")]
    InvalidState(String),
}

impl From<serde_json::Error> for StoreError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialization(Arc::new(error))
    }
}

impl From<std::io::Error> for StoreError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(Arc::new(error))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DurableSessionEvent {
    pub envelope: AgentEventEnvelope,
    pub session_sequence: Option<u64>,
}

impl From<AgentEventEnvelope> for DurableSessionEvent {
    fn from(envelope: AgentEventEnvelope) -> Self {
        Self {
            session_sequence: envelope.session_sequence,
            envelope,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredPendingToolCall {
    pub call: ToolCall,
    pub started_at: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredAgentState {
    pub agent_id: AgentId,
    pub parent_id: Option<AgentId>,
    pub status: AgentStatus,
    pub current_operation: Option<AgentOperation>,
    pub system_prompt: String,
    /// Session-level default execution params (model, max_tokens,
    /// temperature, reasoning, ...). Additive field — old snapshots without
    /// it restore to `ExecutionParams::default()`, matching the pre-M4
    /// behavior of never overriding provider defaults.
    #[serde(default)]
    pub execution_params: harness_protocol::backend::ExecutionParams,
    pub messages: Vec<AgentMessage>,
    pub active_run: Option<RunId>,
    pub pending_tools: HashMap<ToolCallId, StoredPendingToolCall>,
    pub pending_permissions: HashMap<PermissionId, ToolCallId>,
    pub children: Vec<AgentId>,
    pub last_error: Option<AgentError>,
    pub transition_sequence: u64,
    pub depth: u32,
    pub backend: BackendBinding,
    #[serde(default)]
    pub backend_config: serde_json::Value,
    pub budget: AgentBudget,
    pub capabilities: serde_json::Value,
    pub usage: serde_json::Value,
}

/// Non-secret durable metadata captured with a snapshot (RC-302/RC-304).
///
/// This block records what the session was **authorized against** at
/// snapshot time so a later restore can verify the same host dependencies
/// still exist. It deliberately contains only *references* (identities and
/// policy IDs) — never credentials, secret configuration, or bearer tokens.
/// A missing dependency discovered during restore produces a typed
/// [`crate::resolver::MissingDependency`] instead of a silent substitution.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DurableSessionMetadata {
    /// Canonical identity of the workspace the session was bound to, if any
    /// (e.g. the canonicalized root path). `None` means the session had no
    /// workspace binding and restore must not invent one.
    #[serde(default)]
    pub workspace_identity: Option<String>,
    /// Integration family IDs referenced by the snapshot's agents.
    #[serde(default)]
    pub integration_references: Vec<String>,
    /// Credential-profile references the session was authorized under.
    #[serde(default)]
    pub credential_profiles: Vec<String>,
    /// Tool-policy IDs the session's tool capabilities were governed by.
    #[serde(default)]
    pub tool_policy_ids: Vec<String>,
    /// Whether this snapshot was produced by a compaction step.
    #[serde(default)]
    pub compacted: bool,
    /// Monotonic compaction generation; `0` for a plain checkpoint.
    #[serde(default)]
    pub compaction_generation: u64,
}

/// A durable restore checkpoint (RC-302).
///
/// Snapshots are versioned ([`schema_version`](Self::schema_version), see
/// [`crate::version::SCHEMA_VERSION`]) so a newer snapshot is rejected
/// up-front by an older build and older snapshots can be migrated forward.
/// `metadata` records the non-secret host dependencies the checkpoint was
/// taken under (see [`DurableSessionMetadata`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DurableSessionSnapshot {
    pub session_id: SessionId,
    pub root_agent_id: AgentId,
    pub agents: Vec<StoredAgentState>,
    pub session_sequence: u64,
    pub timestamp: Timestamp,
    /// Snapshot schema version (RC-305). Absent in pre-RC-300 checkpoints,
    /// which deserialize as `0` and are migrated by
    /// [`crate::version::migrate_snapshot`].
    #[serde(default)]
    pub schema_version: u64,
    /// Non-secret durable metadata captured with this checkpoint (RC-304).
    #[serde(default)]
    pub metadata: DurableSessionMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub session_id: SessionId,
    pub title: String,
    pub backend_name: Option<String>,
    pub backend_config: serde_json::Value,
    pub updated_at: Timestamp,
    pub restorable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredSession {
    pub session_id: SessionId,
    pub snapshot: Option<DurableSessionSnapshot>,
    pub events: Vec<DurableSessionEvent>,
}

/// A single raw record in a session's durable stream, in store order.
///
/// Unlike [`StoredSession`] (which applies the snapshot cutoff),
/// [`SessionStore::raw_records`] returns the unprocessed append stream — the
/// input that read-only diagnostics and repair tooling operate on. Snapshot
/// records appear in the position they were appended.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RawRecord {
    /// A durable event appended to the history.
    Event(DurableSessionEvent),
    /// A restore checkpoint (replaces any earlier one for the session).
    Snapshot(DurableSessionSnapshot),
}

pub fn summarize_session(stored: &StoredSession) -> Option<SessionSummary> {
    let snapshot = stored.snapshot.as_ref();
    let root = snapshot.and_then(|snapshot| {
        snapshot
            .agents
            .iter()
            .find(|agent| agent.agent_id == snapshot.root_agent_id)
    });
    let title = root
        .and_then(|agent| {
            agent
                .messages
                .iter()
                .find(|message| message.role == MessageRole::User)
        })
        .and_then(|message| {
            message.content.iter().find_map(|block| match block {
                ContentBlock::Text { text } => Some(text),
                _ => None,
            })
        })
        .map(|text| {
            let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
            let mut chars = compact.chars();
            let title = chars.by_ref().take(48).collect::<String>();
            if chars.next().is_some() {
                format!("{title}…")
            } else {
                title
            }
        })
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| {
            let id = stored.session_id.to_string();
            format!("Session {}", id.chars().take(8).collect::<String>())
        });
    let updated_at = stored
        .events
        .iter()
        .map(|event| event.envelope.timestamp)
        .chain(snapshot.map(|snapshot| snapshot.timestamp))
        .max()?;

    Some(SessionSummary {
        session_id: stored.session_id,
        title,
        backend_name: root.map(|agent| agent.backend.descriptor.name.clone()),
        backend_config: root
            .map(|agent| agent.backend_config.clone())
            .unwrap_or(serde_json::Value::Null),
        updated_at,
        restorable: snapshot.is_some(),
    })
}

#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn list_sessions(&self) -> Result<Vec<SessionSummary>, StoreError> {
        Ok(Vec::new())
    }

    async fn load_session(&self, id: SessionId) -> Result<StoredSession, StoreError>;

    /// Default implementation only — real backends should override this.
    ///
    /// Built on `load_session`, which trims events already folded into a
    /// snapshot from its trailing view; that's correct for `load_session`
    /// itself but wrong here, since a caller asking "everything since
    /// sequence N" needs those events too whenever `since_seq` is below the
    /// snapshot's cutoff (e.g. right after a terminal run's checkpoint). A
    /// store backed by a real append-only log should override this to query
    /// that log directly by sequence, ignoring the snapshot cutoff
    /// entirely — see `JsonlSessionStore`/`SqliteSessionStore`/`MemoryStore`
    /// for the pattern. Only a store with no independent raw log (nothing
    /// beyond `load_session` to query) should rely on this default.
    async fn events_since(
        &self,
        id: SessionId,
        since_seq: u64,
    ) -> Result<Vec<DurableSessionEvent>, StoreError> {
        let stored = self.load_session(id).await?;
        Ok(stored
            .events
            .into_iter()
            .filter(|event| event.session_sequence.is_some_and(|seq| seq > since_seq))
            .collect())
    }

    async fn append(&self, event: DurableSessionEvent) -> Result<(), StoreError>;
    async fn save_snapshot(&self, snapshot: DurableSessionSnapshot) -> Result<(), StoreError>;

    /// Returns the highest committed session sequence for `id`.
    ///
    /// This is the durable sequencer resume point used by
    /// [`crate::commit::SessionCommitter`] after a restart. An unknown
    /// session reports `0` (not an error), so a fresh committer can resume
    /// from sequence `1` without probing for existence first.
    ///
    /// The default implementation derives the value from
    /// [`load_session`](Self::load_session): the snapshot's sequence plus the
    /// trailing events' maximum. Stores with an index override this with a
    /// bounded query.
    async fn current_sequence(&self, id: SessionId) -> Result<u64, StoreError> {
        match self.load_session(id).await {
            Ok(stored) => {
                let snapshot_max = stored
                    .snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.session_sequence)
                    .unwrap_or(0);
                let event_max = stored
                    .events
                    .iter()
                    .filter_map(|event| event.session_sequence)
                    .max()
                    .unwrap_or(0);
                Ok(snapshot_max.max(event_max))
            }
            Err(StoreError::NotFound(_)) => Ok(0),
            Err(error) => Err(error),
        }
    }

    /// Returns the unprocessed record stream for `id` (events and snapshots
    /// in store order), without applying the snapshot cutoff.
    ///
    /// Read-only diagnostics and audit tooling use this; it never mutates
    /// anything. The default implementation is not available for every store
    /// and reports [`StoreError::InvalidState`].
    async fn raw_records(&self, _id: SessionId) -> Result<Vec<RawRecord>, StoreError> {
        Err(StoreError::InvalidState(
            "this store does not expose raw records".into(),
        ))
    }

    /// Explicitly removes durable events with `session_sequence <= sequence`.
    ///
    /// Retention is an opt-in maintenance operation (RC-305): the default
    /// implementation rejects it so a store that has not consciously opted
    /// in can never silently destroy replay/audit history. Stores that
    /// implement pruning must document what replay/audit prerequisites they
    /// preserve (typically: a snapshot at or above `sequence` must exist
    /// first). Returns the number of removed events.
    async fn prune_events_before(&self, _id: SessionId, _sequence: u64) -> Result<u64, StoreError> {
        Err(StoreError::InvalidState(
            "event pruning is not supported by this store".into(),
        ))
    }
}

pub fn is_durable(event: &AgentEvent) -> bool {
    matches!(
        event,
        AgentEvent::AssistantMessageCompleted { .. }
            | AgentEvent::ToolCallStarted { .. }
            | AgentEvent::ToolCallCompleted { .. }
            | AgentEvent::ChildAgentSpawned { .. }
            | AgentEvent::ChildAgentCompleted { .. }
            | AgentEvent::PermissionRequested { .. }
            | AgentEvent::UsageUpdated { .. }
            | AgentEvent::Failed { .. }
            | AgentEvent::Completed { .. }
            | AgentEvent::StateChanged { .. }
            | AgentEvent::RunStarted { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_text_is_ephemeral() {
        assert!(!is_durable(&AgentEvent::AssistantTextDelta {
            message_id: harness_protocol::ids::MessageId::new(),
            delta: "partial".into(),
        }));
    }

    #[test]
    fn snapshot_metadata_round_trips() {
        let metadata = DurableSessionMetadata {
            workspace_identity: Some("/srv/app".into()),
            integration_references: vec!["anthropic".into()],
            credential_profiles: vec!["anthropic:default".into()],
            tool_policy_ids: vec!["policy-host".into()],
            compacted: true,
            compaction_generation: 3,
        };
        let json = serde_json::to_string(&metadata).expect("serialize metadata");
        let decoded: DurableSessionMetadata =
            serde_json::from_str(&json).expect("deserialize metadata");
        assert_eq!(decoded, metadata);
    }

    #[test]
    fn legacy_snapshot_deserializes_with_version_zero_and_empty_metadata() {
        let json = serde_json::json!({
            "session_id": SessionId::new(),
            "root_agent_id": AgentId::new(),
            "agents": [],
            "session_sequence": 4,
            "timestamp": Timestamp::now(),
        });
        let snapshot: DurableSessionSnapshot =
            serde_json::from_value(json).expect("legacy snapshot without version fields");
        assert_eq!(
            snapshot.schema_version, 0,
            "pre-RC-300 checkpoints are version 0"
        );
        assert_eq!(
            snapshot.metadata,
            DurableSessionMetadata::default(),
            "legacy checkpoints carry no dependency metadata"
        );
    }
}
