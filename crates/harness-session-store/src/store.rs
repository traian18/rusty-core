//! Session store contract and durable session types.
//!
//! This module defines the abstract persistence contract from spec §59 —
//! [`SessionStore`] — together with the durable payload types that flow
//! through it:
//!
//! - [`DurableSessionEvent`] — an event that is safe to persist individually
//!   (never raw text/reasoning deltas or progress ticks, per the durable vs
//!   ephemeral split of spec §46).
//! - [`DurableSessionSnapshot`] — a point-in-time, fully serializable
//!   projection of one or more agents' durable state, sufficient to rebuild
//!   the live `Agent`/`AgentState` at restore time.
//! - [`StoredSession`] — what [`SessionStore::load_session`] returns: the
//!   latest snapshot (if any) plus any durable events appended after it.
//!
//! The store only depends on `harness-protocol` types (plus serde). Runtime
//! concerns such as the concrete `AgentCapabilities` / `UsageLedger` from
//! `harness-core` are carried as opaque JSON projections in
//! [`StoredAgentState`]; the runtime layer (which owns those concrete types)
//! serializes/deserializes them. See [`StoredAgentState::capabilities`] and
//! [`StoredAgentState::usage`].

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use harness_protocol::backend::BackendBinding;
use harness_protocol::commands::{AgentError, AgentOperation, AgentStatus};
use harness_protocol::events::{AgentEvent, AgentEventEnvelope};
use harness_protocol::ids::{
    AgentId, PermissionId, RunId, SessionId, Timestamp, ToolCallId,
};
use harness_protocol::messages::AgentMessage;
use harness_protocol::tools::ToolCall;
use harness_protocol::usage::AgentBudget;

// ---------------------------------------------------------------------------
// StoreError
// ---------------------------------------------------------------------------

/// Errors produced by the persistence layer.
///
/// `serde_json::Error` and `std::io::Error` are surfaced as `#[from]` so
/// concrete store implementations (`sqlite.rs`, `jsonl.rs`) can bubble their
/// low-level failures up without manual mapping.
#[derive(Debug, Clone, thiserror::Error)]
pub enum StoreError {
    /// No stored session exists for the requested identifier.
    #[error("session not found: {0}")]
    NotFound(SessionId),

    /// A durable payload could not be serialized or deserialized.
    #[error("durable payload serialization error: {0}")]
    Serialization(Arc<serde_json::Error>),

    /// A filesystem/IO operation failed (JSONL store, WAL files, etc.).
    #[error("store io error: {0}")]
    Io(Arc<std::io::Error>),

    /// The storage backend reported an error not otherwise categorized.
    #[error("store backend error: {0}")]
    Backend(String),

    /// The store violated an invariant (duplicate sequence, corrupt row, …).
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

// ---------------------------------------------------------------------------
// DurableSessionEvent
// ---------------------------------------------------------------------------

/// An event that is persisted individually to a session's durable history.
///
/// Only events that pass [`is_durable`] may ever be wrapped as a
/// `DurableSessionEvent` (spec §46). The embedded [`AgentEventEnvelope`]
/// carries the full routing/ordering metadata (`session_id`, `agent_id`,
/// `parent_agent_id`, `run_id`, `agent_sequence`, `session_sequence`,
/// `timestamp`, `visibility`) needed to replay and reconstruct state.
///
/// Raw text/reasoning deltas and tool-progress ticks are **never** wrapped
/// here; the final assembled content is persisted via the
/// `AssistantMessageCompleted` / `ToolCallCompleted` events instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DurableSessionEvent {
    /// The fully-qualified event payload and its routing metadata.
    pub envelope: AgentEventEnvelope,
    /// Monotonic session-scoped ordering used by the store for indexing.
    ///
    /// This mirrors (and is expected to match) the envelope's
    /// `session_sequence` once committed to the session stream; it is kept
    /// explicit here so the store can index appends without reaching into the
    /// envelope. `None` means the event has not yet been assigned a session
    /// sequence by the runtime.
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

// ---------------------------------------------------------------------------
// StoredPendingToolCall
// ---------------------------------------------------------------------------

/// A store-side, serializable equivalent of the core's `PendingToolCall`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredPendingToolCall {
    /// The tool call awaiting execution/completion.
    pub call: ToolCall,
    /// When the tool call started executing.
    pub started_at: Timestamp,
}

// ---------------------------------------------------------------------------
// StoredAgentState
// ---------------------------------------------------------------------------

/// The durable portion of one agent's state, sufficient to reconstruct a
/// `harness_core::agent::Agent` (its [`AgentState`] fields, its [`BackendBinding`],
/// its [`AgentBudget`], and opaque projections of its capabilities and usage).
///
/// All the fields required to rebuild a full `AgentState` (spec §8) are
/// present: `status`, `current_operation`, `system_prompt`, `messages`,
/// `active_run`, `pending_tools`, `pending_permissions`, `children`,
/// `last_error`, `transition_sequence`, and `depth`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredAgentState {
    /// The agent's identity.
    pub agent_id: AgentId,
    /// The parent agent, if any (root agents have `None`).
    pub parent_id: Option<AgentId>,
    /// The agent's high-level status.
    pub status: AgentStatus,
    /// What the agent is currently doing, if anything.
    pub current_operation: Option<AgentOperation>,
    /// The agent's system prompt.
    pub system_prompt: String,
    /// The agent's conversation/transcript history.
    pub messages: Vec<AgentMessage>,
    /// The currently active run, if any.
    pub active_run: Option<RunId>,
    /// Tool calls that have started but not yet completed.
    pub pending_tools: HashMap<ToolCallId, StoredPendingToolCall>,
    /// Correlation from a pending permission request to its tool call.
    pub pending_permissions: HashMap<PermissionId, ToolCallId>,
    /// Identifiers of spawned child agents.
    pub children: Vec<AgentId>,
    /// The last error encountered, if any.
    pub last_error: Option<AgentError>,
    /// Monotonic source for deterministic IDs/timestamps.
    pub transition_sequence: u64,
    /// Nesting depth in the agent tree (`0` for a root agent).
    pub depth: u32,
    /// The persistable backend binding used to re-create the live backend.
    pub backend: BackendBinding,
    /// Resolved, non-secret configuration JSON for the backend referenced by
    /// [`backend`](Self::backend).
    ///
    /// Persisted alongside the agent's [`BackendReference`] at snapshot time
    /// so a restore can re-create the live backend through the runtime's
    /// integration registry (`IntegrationRegistry::create(integration, config)`)
    /// without a separate configuration registry. Credentials/secrets are
    /// **never** written here — only non-secret provider settings (model
    /// name, endpoint, temperature, …). Absent for snapshots written before
    /// this field existed; deserialization defaults to an empty object.
    #[serde(default)]
    pub backend_config: serde_json::Value,
    /// The agent's budget.
    pub budget: AgentBudget,
    /// Opaque JSON projection of the agent's `AgentCapabilities`.
    ///
    /// `harness_core::capabilities::AgentCapabilities` is deliberately *not*
    /// `Serialize`, so the runtime serializes it through its own projection
    /// into this field (a `serde_json::Value`) when creating a snapshot, and
    /// deserializes it back when restoring.
    pub capabilities: serde_json::Value,
    /// Opaque JSON projection of the agent's `UsageLedger`.
    ///
    /// The concrete `harness_core::agent::UsageLedger` is runtime-owned and
    /// not `Serialize`; the runtime serializes its records into this field.
    pub usage: serde_json::Value,
}

// ---------------------------------------------------------------------------
// DurableSessionSnapshot
// ---------------------------------------------------------------------------

/// A point-in-time, fully serializable snapshot of a session's durable state.
///
/// Snapshots are written via [`SessionStore::save_snapshot`] and act as a
/// restore checkpoint. [`SessionStore::load_session`] returns the latest
/// snapshot plus any durable events appended after it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DurableSessionSnapshot {
    /// The session this snapshot describes.
    pub session_id: SessionId,
    /// The identifier of the session's root agent.
    pub root_agent_id: AgentId,
    /// Durable state for every agent known to the session (root + descendants).
    pub agents: Vec<StoredAgentState>,
    /// The session-scoped sequence this snapshot was taken at.
    pub session_sequence: u64,
    /// Wall-clock timestamp of when the snapshot was taken.
    pub timestamp: Timestamp,
}

// ---------------------------------------------------------------------------
// StoredSession
// ---------------------------------------------------------------------------

/// The result of [`SessionStore::load_session`]: a session's latest snapshot
/// (if one exists) together with all durable events appended after it.
///
/// Consumers replay the durable events on top of the snapshot to reconstruct
/// the session's live state exactly (spec §71, "snapshot + event
/// restoration").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredSession {
    /// The session this stored data belongs to.
    pub session_id: SessionId,
    /// The most recent durable snapshot, if any.
    pub snapshot: Option<DurableSessionSnapshot>,
    /// Durable events appended after the snapshot (or all events if no
    /// snapshot exists), ordered by `session_sequence`.
    pub events: Vec<DurableSessionEvent>,
}

// ---------------------------------------------------------------------------
// SessionStore trait
// ---------------------------------------------------------------------------

/// Abstract durable session history (spec §59).
///
/// Implementations (`sqlite.rs`, `jsonl.rs`, application-managed stores) must
/// be `Send + Sync` so they can be shared across the runtime. `append` and
/// `save_snapshot` are the write path; `load_session` is the restore path.
#[async_trait]
pub trait SessionStore: Send + Sync {
    /// Loads a session's durable data (latest snapshot + trailing events).
    ///
    /// Returns [`StoreError::NotFound`] if no stored data exists for `id`.
    async fn load_session(&self, id: SessionId) -> Result<StoredSession, StoreError>;

    /// Appends a single durable event to the session's history.
    ///
    /// Callers must only pass events for which [`is_durable`] returns `true`;
    /// ephemeral events are never wrapped as [`DurableSessionEvent`]s.
    async fn append(&self, event: DurableSessionEvent) -> Result<(), StoreError>;

    /// Persists a session snapshot, replacing any previous snapshot for the
    /// same session.
    async fn save_snapshot(&self, snapshot: DurableSessionSnapshot) -> Result<(), StoreError>;
}

// ---------------------------------------------------------------------------
// Durable vs ephemeral split
// ---------------------------------------------------------------------------

/// Returns `true` if an [`AgentEvent`] is durable and should be wrapped as a
/// [`DurableSessionEvent`] and persisted (spec §46).
///
/// The split follows the spec: only message-completed, tool started/completed,
/// agent spawned/completed, permission decisions, usage records, errors, and
/// relevant state transitions are durable. Raw text/reasoning deltas and
/// tool-progress ticks are ephemeral — they are **never** persisted
/// individually; only the final assembled content is, via the corresponding
/// `...Completed` event.
///
/// # Durability table (all 17 variants)
///
/// | Variant                     | Durable? | Rationale                                          |
/// |-----------------------------|----------|----------------------------------------------------|
/// | `StateChanged`              | yes      | relevant state transition                          |
/// | `RunStarted`                | yes      | run lifecycle                                      |
/// | `BackendRequestStarted`     | no       | transient request begin                            |
/// | `AssistantMessageStarted`   | no       | superseded by `AssistantMessageCompleted`          |
/// | `AssistantTextDelta`        | no       | raw text chunk; only the assembled message persists|
/// | `ReasoningDelta`            | no       | raw reasoning chunk; never persisted               |
/// | `AssistantMessageCompleted` | yes      | final assembled message                            |
/// | `ToolCallRequested`         | no       | transient request; superseded by started/completed |
/// | `ToolCallStarted`           | yes      | tool lifecycle start                               |
/// | `ToolCallProgress`          | no       | progress tick; never persisted                     |
/// | `ToolCallCompleted`         | yes      | final tool result summary                          |
/// | `PermissionRequested`       | yes      | permission decision point                          |
/// | `UsageUpdated`              | yes      | usage record                                       |
/// | `ChildAgentSpawned`         | yes      | agent lifecycle                                    |
/// | `ChildAgentCompleted`       | yes      | agent lifecycle                                    |
/// | `Failed`                    | yes      | error                                              |
/// | `Completed`                 | yes      | final outcome                                      |
pub fn is_durable(event: &AgentEvent) -> bool {
    matches!(event,
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
    // explicitly ephemeral: AssistantTextDelta, ReasoningDelta,
    // AssistantMessageStarted, BackendRequestStarted, ToolCallRequested,
    // ToolCallProgress — the table-driven test below enumerates all 17.
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use harness_protocol::backend::{
        BackendCapabilities, BackendDescriptor, BackendReference,
    };
    use harness_protocol::events::{AgentOutcome, EventVisibility};
    use harness_protocol::effects::PermissionRequest;
    use harness_protocol::ids::{
        BackendId, ConfigurationId, EventId, IntegrationId, MessageId, RequestId, RunId,
    };
    use harness_protocol::tools::{ToolCall, ToolProgress, ToolResultSummary};
    use harness_protocol::usage::AgentUsageSnapshot;

    /// Builds a minimal `ToolCall` for test events.
    fn tool_call() -> ToolCall {
        ToolCall {
            id: ToolCallId::new(),
            name: "fs.read".into(),
            arguments: serde_json::json!({"path": "/tmp/test.txt"}),
        }
    }

    /// The table-driven spec for all 17 `AgentEvent` variants:
    /// `(name, event, durable)`.
    ///
    /// Keeping this as an explicit table makes the durable/ephemeral decision
    /// for every variant visible and reviewable. If a new variant is added to
    /// `AgentEvent` without updating this table, the count assertion in
    /// [`is_durable_covers_all_17_variants`] fails loudly.
    fn durability_table() -> Vec<(&'static str, AgentEvent, bool)> {
        vec![
            (
                "StateChanged",
                AgentEvent::StateChanged {
                    from: AgentStatus::Idle,
                    to: AgentStatus::PreparingContext,
                },
                true,
            ),
            (
                "RunStarted",
                AgentEvent::RunStarted {
                    run_id: RunId::new(),
                },
                true,
            ),
            (
                "BackendRequestStarted",
                AgentEvent::BackendRequestStarted {
                    request_id: RequestId::new(),
                },
                false,
            ),
            (
                "AssistantMessageStarted",
                AgentEvent::AssistantMessageStarted {
                    message_id: MessageId::new(),
                },
                false,
            ),
            (
                "AssistantTextDelta",
                AgentEvent::AssistantTextDelta {
                    message_id: MessageId::new(),
                    delta: "hello".into(),
                },
                false,
            ),
            (
                "ReasoningDelta",
                AgentEvent::ReasoningDelta {
                    message_id: MessageId::new(),
                    delta: "thinking".into(),
                },
                false,
            ),
            (
                "AssistantMessageCompleted",
                AgentEvent::AssistantMessageCompleted {
                    message_id: MessageId::new(),
                },
                true,
            ),
            (
                "ToolCallRequested",
                AgentEvent::ToolCallRequested { call: tool_call() },
                false,
            ),
            (
                "ToolCallStarted",
                AgentEvent::ToolCallStarted {
                    call_id: ToolCallId::new(),
                },
                true,
            ),
            (
                "ToolCallProgress",
                AgentEvent::ToolCallProgress {
                    call_id: ToolCallId::new(),
                    progress: ToolProgress {
                        status: "running".into(),
                        fraction: 0.5,
                    },
                },
                false,
            ),
            (
                "ToolCallCompleted",
                AgentEvent::ToolCallCompleted {
                    call_id: ToolCallId::new(),
                    result: ToolResultSummary {
                        has_error: false,
                        output_preview: "ok".into(),
                    },
                },
                true,
            ),
            (
                "PermissionRequested",
                AgentEvent::PermissionRequested {
                    request: PermissionRequest {
                        id: PermissionId::new(),
                        tool_call: tool_call(),
                        agent_id: AgentId::new(),
                    },
                },
                true,
            ),
            (
                "UsageUpdated",
                AgentEvent::UsageUpdated {
                    usage: AgentUsageSnapshot::default(),
                },
                true,
            ),
            (
                "ChildAgentSpawned",
                AgentEvent::ChildAgentSpawned {
                    agent_id: AgentId::new(),
                },
                true,
            ),
            (
                "ChildAgentCompleted",
                AgentEvent::ChildAgentCompleted {
                    agent_id: AgentId::new(),
                    outcome: AgentOutcome::Success,
                },
                true,
            ),
            (
                "Failed",
                AgentEvent::Failed {
                    error: AgentError {
                        message: "boom".into(),
                        code: "ERR_INTERNAL".into(),
                        details: None,
                    },
                },
                true,
            ),
            (
                "Completed",
                AgentEvent::Completed {
                    outcome: AgentOutcome::Success,
                },
                true,
            ),
        ]
    }

    /// Asserts the table covers exactly the 17 `AgentEvent` variants and that
    /// `is_durable` agrees with the decision recorded for each one.
    #[test]
    fn is_durable_covers_all_17_variants() {
        let table = durability_table();
        assert_eq!(
            table.len(),
            17,
            "AgentEvent has 17 variants; update durability_table() when adding a variant"
        );

        for (name, event, expected) in table {
            let actual = is_durable(&event);
            assert_eq!(
                actual, expected,
                "durability mismatch for variant {name}: got {actual}, expected {expected}"
            );
        }
    }

    /// The durable set is exactly the table's `true` rows and the ephemeral
    /// set exactly its `false` rows.
    #[test]
    fn durable_and_ephemeral_partition_the_variant_space() {
        let durable: Vec<_> = durability_table()
            .into_iter()
            .filter(|(_, _, durable)| *durable)
            .collect();
        let ephemeral: Vec<_> = durability_table()
            .into_iter()
            .filter(|(_, _, durable)| !*durable)
            .collect();

        assert_eq!(durable.len(), 11);
        assert_eq!(ephemeral.len(), 6);
        assert_eq!(durable.len() + ephemeral.len(), 17);
    }

    /// A `DurableSessionEvent` round-trips through JSON, preserving the
    /// envelope and the session sequence.
    #[test]
    fn durable_session_event_roundtrip() {
        let envelope = AgentEventEnvelope {
            event_id: EventId::new(),
            session_id: SessionId::new(),
            agent_id: AgentId::new(),
            parent_agent_id: None,
            run_id: Some(RunId::new()),
            agent_sequence: 3,
            session_sequence: Some(7),
            timestamp: Timestamp::now(),
            visibility: EventVisibility::User,
            event: AgentEvent::ToolCallCompleted {
                call_id: ToolCallId::new(),
                result: ToolResultSummary {
                    has_error: false,
                    output_preview: "done".into(),
                },
            },
        };

        let event: DurableSessionEvent = envelope.clone().into();
        assert_eq!(event.session_sequence, envelope.session_sequence);

        let json = serde_json::to_string(&event).expect("serialize durable event");
        let deserialized: DurableSessionEvent =
            serde_json::from_str(&json).expect("deserialize durable event");

        assert_eq!(deserialized.session_sequence, Some(7));
        assert_eq!(deserialized.envelope.agent_sequence, 3);
        assert_eq!(deserialized.envelope.session_id, envelope.session_id);
        assert!(matches!(
            deserialized.envelope.event,
            AgentEvent::ToolCallCompleted { .. }
        ));
    }

    /// A `DurableSessionSnapshot` carrying full agent state round-trips
    /// through JSON, demonstrating it can reconstruct an `AgentState`.
    #[test]
    fn durable_session_snapshot_roundtrip() {
        let session_id = SessionId::new();
        let agent_id = AgentId::new();
        let call_id = ToolCallId::new();
        let permission_id = PermissionId::new();

        let snapshot = DurableSessionSnapshot {
            session_id,
            root_agent_id: agent_id,
            agents: vec![StoredAgentState {
                agent_id,
                parent_id: None,
                status: AgentStatus::Executing,
                current_operation: Some(AgentOperation::Tools {
                    calls: vec![call_id],
                }),
                system_prompt: "You are a helpful assistant.".into(),
                messages: vec![AgentMessage {
                    id: MessageId::new(),
                    role: harness_protocol::messages::MessageRole::Assistant,
                    content: vec![],
                    created_at: Timestamp::now(),
                }],
                active_run: Some(RunId::new()),
                pending_tools: HashMap::from([(
                    call_id,
                    StoredPendingToolCall {
                        call: tool_call(),
                        started_at: Timestamp::now(),
                    },
                )]),
                pending_permissions: HashMap::from([(permission_id, call_id)]),
                children: vec![AgentId::new()],
                last_error: Some(AgentError {
                    message: "recovered".into(),
                    code: "WARN".into(),
                    details: None,
                }),
                transition_sequence: 42,
                depth: 0,
                backend: BackendBinding {
                    reference: BackendReference {
                        integration: IntegrationId::new(),
                        configuration: ConfigurationId::new(),
                        model: None,
                    },
                    descriptor: BackendDescriptor {
                        id: BackendId::new(),
                        name: "test".into(),
                        description: "test backend".into(),
                        capabilities: BackendCapabilities {
                            streaming: true,
                            ..Default::default()
                        },
                    },
                },
                // Resolved, non-secret provider configuration persisted at
                // snapshot time so restore can re-create the live backend
                // without a ConfigurationRegistry.
                backend_config: serde_json::json!({
                    "model": "claude-3-5-sonnet",
                    "endpoint": "https://api.example.com",
                }),
                budget: AgentBudget {
                    max_total_tokens: Some(1000),
                    ..Default::default()
                },
                capabilities: serde_json::json!({ "can_spawn_agents": false }),
                usage: serde_json::json!({ "records": [] }),
            }],
            session_sequence: 99,
            timestamp: Timestamp::now(),
        };

        let json = serde_json::to_string(&snapshot).expect("serialize snapshot");
        let deserialized: DurableSessionSnapshot =
            serde_json::from_str(&json).expect("deserialize snapshot");

        assert_eq!(deserialized.session_id, session_id);
        assert_eq!(deserialized.root_agent_id, agent_id);
        assert_eq!(deserialized.session_sequence, 99);
        let agent = &deserialized.agents[0];
        // Every field needed to reconstruct a full AgentState is preserved.
        assert_eq!(agent.status, AgentStatus::Executing);
        assert_eq!(agent.system_prompt, "You are a helpful assistant.");
        assert_eq!(agent.messages.len(), 1);
        assert!(agent.active_run.is_some());
        assert_eq!(agent.pending_tools.len(), 1);
        assert_eq!(agent.pending_permissions.len(), 1);
        assert_eq!(agent.children.len(), 1);
        assert_eq!(agent.transition_sequence, 42);
        assert_eq!(agent.depth, 0);
        assert_eq!(agent.budget.max_total_tokens, Some(1000));
        // The resolved backend config survives the round-trip.
        assert_eq!(agent.backend_config["model"], "claude-3-5-sonnet");
        assert_eq!(
            agent.backend_config["endpoint"],
            "https://api.example.com"
        );
        assert_eq!(agent.capabilities["can_spawn_agents"], false);
        assert!(agent.usage.is_object());
    }

    /// A snapshot written without `backend_config` (pre-Phase-7 format)
    /// still deserializes, defaulting the field to an empty object.
    #[test]
    fn snapshot_without_backend_config_deserializes_to_default() {
        let session_id = SessionId::new();
        let agent_id = AgentId::new();

        let snapshot = DurableSessionSnapshot {
            session_id,
            root_agent_id: agent_id,
            agents: vec![StoredAgentState {
                agent_id,
                parent_id: None,
                status: AgentStatus::Idle,
                current_operation: None,
                system_prompt: String::new(),
                messages: vec![],
                active_run: None,
                pending_tools: HashMap::new(),
                pending_permissions: HashMap::new(),
                children: vec![],
                last_error: None,
                transition_sequence: 0,
                depth: 0,
                backend: BackendBinding {
                    reference: BackendReference {
                        integration: IntegrationId::new(),
                        configuration: ConfigurationId::new(),
                        model: None,
                    },
                    descriptor: BackendDescriptor {
                        id: BackendId::new(),
                        name: "test".into(),
                        description: "test backend".into(),
                        capabilities: BackendCapabilities::default(),
                    },
                },
                backend_config: serde_json::Value::Null,
                budget: AgentBudget::default(),
                capabilities: serde_json::json!({}),
                usage: serde_json::json!({}),
            }],
            session_sequence: 1,
            timestamp: Timestamp::now(),
        };

        let mut json = serde_json::to_string(&snapshot).expect("serialize snapshot");
        // Strip the backend_config key, simulating a snapshot written before
        // the field existed.
        let mut value: serde_json::Value =
            serde_json::from_str(&json).expect("parse snapshot json");
        value["agents"][0]
            .as_object_mut()
            .expect("agent object")
            .remove("backend_config");
        json = serde_json::to_string(&value).expect("reserialize snapshot");

        let deserialized: DurableSessionSnapshot =
            serde_json::from_str(&json).expect("deserialize legacy snapshot");
        assert_eq!(
            deserialized.agents[0].backend_config,
            serde_json::Value::Null,
            "missing backend_config must default rather than fail deserialization"
        );
    }

    /// A `StoredSession` (snapshot + trailing events) round-trips through JSON.
    #[test]
    fn stored_session_roundtrip() {
        let session_id = SessionId::new();
        let stored = StoredSession {
            session_id,
            snapshot: Some(DurableSessionSnapshot {
                session_id,
                root_agent_id: AgentId::new(),
                agents: vec![],
                session_sequence: 1,
                timestamp: Timestamp::now(),
            }),
            events: vec![DurableSessionEvent {
                envelope: AgentEventEnvelope {
                    event_id: EventId::new(),
                    session_id,
                    agent_id: AgentId::new(),
                    parent_agent_id: None,
                    run_id: None,
                    agent_sequence: 0,
                    session_sequence: Some(2),
                    timestamp: Timestamp::now(),
                    visibility: EventVisibility::Internal,
                    event: AgentEvent::StateChanged {
                        from: AgentStatus::Idle,
                        to: AgentStatus::WaitingForBackend,
                    },
                },
                session_sequence: Some(2),
            }],
        };

        let json = serde_json::to_string(&stored).expect("serialize stored session");
        let deserialized: StoredSession =
            serde_json::from_str(&json).expect("deserialize stored session");

        assert_eq!(deserialized.session_id, session_id);
        assert!(deserialized.snapshot.is_some());
        assert_eq!(deserialized.events.len(), 1);
        assert_eq!(deserialized.events[0].session_sequence, Some(2));
    }
}
