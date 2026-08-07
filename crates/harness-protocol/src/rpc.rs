//! Transport-neutral JSON RPC contract shared by harness transports and SDKs.
//!
//! Protocol v2 uses internally tagged command/response enums. Variant names and
//! payload fields are therefore stable across languages and do not depend on
//! serde's externally-tagged enum representation.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::admission::{AdmissionResult, MutationMetadata};
use crate::commands::{PermissionDecision, UserInput};
use crate::events::AgentEventEnvelope;
use crate::ids::{AgentId, PermissionId, SessionId, Timestamp};
use crate::tools::AgentToolset;
use crate::usage::{AgentUsageSnapshot, SessionUsageSnapshot};

pub const PROTOCOL_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RequestCorrelationId(pub u64);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcRequest {
    pub id: RequestCorrelationId,
    pub session_id: Option<SessionId>,
    pub body: RpcRequestBody,
}

/// Mutations are explicitly separated from reads. Each mutation carries a
/// client-generated command id and optional optimistic revision.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum MutationCommand {
    Prompt(UserInput),
    Steer(UserInput),
    FollowUp(UserInput),
    Cancel,
    ResolvePermission {
        id: PermissionId,
        decision: PermissionDecision,
    },
    CloseSession,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum RpcRequestBody {
    Hello {
        protocol_version: u32,
    },
    CreateSession {
        workspace_root: PathBuf,
        integration: String,
        integration_config: serde_json::Value,
        toolset: AgentToolset,
    },
    /// Idempotent, revision-aware session mutation.
    Mutate {
        metadata: MutationMetadata,
        command: MutationCommand,
    },
    ListSessions,
    /// Restore/open a durable session using host-supplied non-secret bindings.
    RestoreSession {
        session_id: SessionId,
        workspace_root: PathBuf,
        toolset: AgentToolset,
    },
    Snapshot,
    Subscribe {
        since_seq: Option<u64>,
    },
    /// M6: operational health/diagnostics + a Prometheus text metrics
    /// snapshot, over the same RPC transports every other request uses —
    /// deliberately not a separate HTTP `/metrics` listener, so exposing
    /// this needs no new port/surface beyond what a host already accepts.
    /// Not session-scoped (`session_id` is ignored, like `Hello`/
    /// `ListSessions`).
    GetDiagnostics {
        /// When `true`, additionally scans the durable store for
        /// consistency issues (`harness_session_store::diagnose_store`) and
        /// includes a summary count. Off by default because a full store
        /// scan reads every session's record stream — cheap for a handful
        /// of sessions, not something a health-check-frequency caller
        /// should request on every call against a large store.
        #[serde(default)]
        include_store_scan: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcResponse {
    pub id: Option<RequestCorrelationId>,
    pub body: RpcResponseBody,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RpcErrorCategory {
    Protocol,
    Validation,
    NotFound,
    Conflict,
    Capacity,
    Lifecycle,
    Persistence,
    Integration,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: String,
    pub category: RpcErrorCategory,
    pub retryable: bool,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<crate::ids::RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_request_id: Option<String>,
}

impl RpcError {
    pub fn new(
        code: impl Into<String>,
        category: RpcErrorCategory,
        retryable: bool,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            category,
            retryable,
            message: message.into(),
            details: None,
            trace_id: None,
            run_id: None,
            provider_request_id: None,
        }
    }

    pub fn protocol(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(code, RpcErrorCategory::Protocol, false, message)
    }

    pub fn not_found(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(code, RpcErrorCategory::NotFound, false, message)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummaryWire {
    pub session_id: SessionId,
    pub title: String,
    pub backend_name: Option<String>,
    pub updated_at: Timestamp,
    pub restorable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum RpcResponseBody {
    Hello {
        protocol_version: u32,
        capabilities: ProtocolCapabilities,
    },
    SessionCreated {
        session_id: SessionId,
    },
    SessionRestored {
        session_id: SessionId,
        session_revision: u64,
    },
    SessionsListed {
        sessions: Vec<SessionSummaryWire>,
    },
    Admission {
        metadata: MutationMetadata,
        result: AdmissionResult,
        session_revision: u64,
    },
    Snapshot(SessionSnapshotWire),
    Event(AgentEventEnvelope),
    /// A durable/live subscription gap. Clients must reconnect from
    /// `last_delivered_sequence` or request a snapshot when the cursor has
    /// expired.
    EventGap {
        session_id: SessionId,
        last_delivered_sequence: u64,
        dropped: u64,
        cursor_expired: bool,
    },
    Ack,
    Failure(RpcError),
    Diagnostics(DiagnosticsSnapshot),
}

/// One permit kind's point-in-time capacity/utilization — the wire
/// projection of `harness_runtime::scheduler::PermitSnapshot`, kept as a
/// separate type here so `harness-protocol` never depends on
/// `harness-runtime`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermitDiagnostic {
    pub kind: String,
    pub capacity: usize,
    pub in_use: usize,
}

/// Summary (not per-session detail — see `GetDiagnostics::include_store_scan`)
/// of a durable-store consistency scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreScanSummary {
    pub total_sessions: usize,
    pub unreadable_sessions: usize,
    pub sessions_with_issues: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticsSnapshot {
    pub uptime_secs: u64,
    pub active_sessions: usize,
    pub scheduler: Vec<PermitDiagnostic>,
    /// `None` unless `GetDiagnostics::include_store_scan` was `true`.
    pub store_scan: Option<StoreScanSummary>,
    /// Rendered Prometheus text exposition format — see
    /// `docs/production-readiness-roadmap.md`'s M6 section for why this
    /// rides the RPC contract instead of a dedicated HTTP listener.
    pub metrics_prometheus_text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolCapabilities {
    pub resumable_subscribe: bool,
    pub lifecycle_commands: bool,
    pub typed_errors: bool,
    pub mutation_admission: bool,
    pub session_restore: bool,
    pub event_gap_signals: bool,
    /// False until command admissions are stored durably across daemon restarts.
    pub durable_idempotency: bool,
    pub pause_resume: bool,
}

impl Default for ProtocolCapabilities {
    fn default() -> Self {
        Self {
            resumable_subscribe: true,
            lifecycle_commands: true,
            typed_errors: true,
            mutation_admission: true,
            session_restore: true,
            event_gap_signals: true,
            durable_idempotency: false,
            pause_resume: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshotWire {
    pub session_id: SessionId,
    pub status: SessionStatusWire,
    pub root_agent_id: AgentId,
    pub root_agent_status: AgentUsageSnapshot,
    pub usage: SessionUsageSnapshot,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatusWire {
    Idle,
    Running,
    Completed,
    Cancelled,
    Failed,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admission::CommandId;

    #[test]
    fn v2_mutation_uses_stable_tags_and_round_trips() {
        let session_id = SessionId::new();
        let request = RpcRequest {
            id: RequestCorrelationId(1),
            session_id: Some(session_id),
            body: RpcRequestBody::Mutate {
                metadata: MutationMetadata {
                    command_id: CommandId::new(),
                    session_id,
                    run_id: None,
                    expected_session_revision: Some(3),
                    trace_id: Some("trace-1".into()),
                },
                command: MutationCommand::Cancel,
            },
        };
        let value = serde_json::to_value(&request).expect("serialize");
        assert_eq!(value["body"]["type"], "mutate");
        assert_eq!(value["body"]["payload"]["command"]["type"], "cancel");
        let parsed: RpcRequest = serde_json::from_value(value).expect("deserialize");
        assert!(matches!(parsed.body, RpcRequestBody::Mutate { .. }));
    }

    #[test]
    fn typed_error_round_trips() {
        let response = RpcResponse {
            id: Some(RequestCorrelationId(2)),
            body: RpcResponseBody::Failure(RpcError::new(
                "session.busy",
                RpcErrorCategory::Conflict,
                true,
                "session already has an active run",
            )),
        };
        let value = serde_json::to_value(&response).expect("serialize");
        assert_eq!(value["body"]["type"], "failure");
        assert_eq!(value["body"]["payload"]["code"], "session.busy");
        let parsed: RpcResponse = serde_json::from_value(value).expect("deserialize");
        assert!(matches!(parsed.body, RpcResponseBody::Failure(error) if error.retryable));
    }

    #[test]
    fn event_gap_round_trips() {
        let session_id = SessionId::new();
        let response = RpcResponse {
            id: None,
            body: RpcResponseBody::EventGap {
                session_id,
                last_delivered_sequence: 41,
                dropped: 3,
                cursor_expired: false,
            },
        };
        let value = serde_json::to_value(&response).expect("serialize");
        assert_eq!(value["body"]["type"], "event_gap");
        let parsed: RpcResponse = serde_json::from_value(value).expect("deserialize");
        assert!(matches!(
            parsed.body,
            RpcResponseBody::EventGap {
                session_id: parsed_id,
                last_delivered_sequence: 41,
                dropped: 3,
                cursor_expired: false,
            } if parsed_id == session_id
        ));
    }

    #[test]
    fn capabilities_are_truthful() {
        let capabilities = ProtocolCapabilities::default();
        assert!(capabilities.typed_errors);
        assert!(capabilities.mutation_admission);
        assert!(capabilities.session_restore);
        assert!(capabilities.event_gap_signals);
        assert!(!capabilities.durable_idempotency);
        assert!(!capabilities.pause_resume);
    }
}
