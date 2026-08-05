//! Durable session-store contract and serializable restore state.

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DurableSessionSnapshot {
    pub session_id: SessionId,
    pub root_agent_id: AgentId,
    pub agents: Vec<StoredAgentState>,
    pub session_sequence: u64,
    pub timestamp: Timestamp,
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
}
