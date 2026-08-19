//! Durable projection of live [`Agent`] state to and from
//! [`StoredAgentState`] — the shape every checkpoint and restore is built
//! from.
//!
//! This module owns both directions of the projection:
//!
//! * **Live → stored**: [`stored_agent_state`] projects one agent, and
//!   [`build_snapshot`] assembles a full [`DurableSessionSnapshot`] from the
//!   session's live [`AgentProjectionTable`].
//! * **Stored → live**: [`capabilities_from_value`] and
//!   [`usage_from_value`] reconstruct the opaque JSON projections
//!   (`AgentCapabilities`, `UsageLedger`) back into their live core types
//!   during restore.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use harness_core::agent::{Agent, UsageLedger};
use harness_core::capabilities::{AgentCapabilities, WorkspaceCapabilities};
use harness_core::usage::AgentUsageSummary as CoreAgentUsageSummary;
use harness_protocol::backend::BackendCapabilities;
use harness_protocol::ids::{AgentId, SessionId, Timestamp};
use harness_protocol::tools::AgentToolset;
use harness_protocol::usage::UsageRecord;
use harness_session_store::{
    DurableSessionMetadata, DurableSessionSnapshot, StoredAgentState, StoredPendingToolCall,
    SCHEMA_VERSION,
};

use crate::traits::Workspace;

// ---------------------------------------------------------------------------
// AgentProjectionTable
// ---------------------------------------------------------------------------

/// Shared table of per-agent durable projections (RC-302).
///
/// Every [`AgentRunner`](crate::agent_runner::AgentRunner) publishes its
/// [`StoredAgentState`] here after each transition (the same data a
/// snapshot serializes), so
/// [`SessionRuntime::checkpoint`](super::SessionRuntime::checkpoint) and the
/// automatic snapshot hooks always build snapshots from live, truthful
/// agent state — never from a stale construction-time copy.
pub type AgentProjectionTable = Arc<Mutex<HashMap<AgentId, StoredAgentState>>>;

// ---------------------------------------------------------------------------
// Live → stored
// ---------------------------------------------------------------------------

/// Projects a live [`Agent`] into its durable [`StoredAgentState`] form.
///
/// `backend_config` is not carried on the core agent today, so it is stored
/// as `Null`; restore resolves backends through the integration registry
/// with the persisted (non-secret) config when one is available.
pub(crate) fn stored_agent_state(agent: &Agent) -> StoredAgentState {
    StoredAgentState {
        agent_id: agent.id,
        parent_id: agent.parent_id,
        status: agent.state.status,
        current_operation: agent.state.current_operation.clone(),
        system_prompt: agent.state.system_prompt.clone(),
        execution_params: agent.state.execution_params.clone(),
        messages: agent.state.messages.clone(),
        active_run: agent.state.active_run,
        pending_tools: agent
            .state
            .pending_tools
            .iter()
            .map(|(call_id, pending)| {
                (
                    *call_id,
                    StoredPendingToolCall {
                        call: pending.call.clone(),
                        started_at: pending.started_at,
                    },
                )
            })
            .collect(),
        pending_permissions: agent.state.pending_permissions.clone(),
        children: agent.state.children.clone(),
        last_error: agent.state.last_error.clone(),
        transition_sequence: agent.state.transition_sequence,
        depth: agent.state.depth,
        backend: agent.backend.clone(),
        backend_config: serde_json::Value::Null,
        budget: agent.budget.clone(),
        capabilities: serde_json::to_value(&agent.capabilities).unwrap_or(serde_json::Value::Null),
        usage: serde_json::to_value(&agent.usage).unwrap_or(serde_json::Value::Null),
    }
}

/// Builds a versioned snapshot from the live projection table (RC-302/RC-305).
///
/// The snapshot records the workspace identity it was taken under and the
/// integration references of every projected agent (RC-304) — references
/// only, never secrets. The workspace identity uses the same canonical form
/// the restore-time resolver compares against (see
/// [`crate::restore::canonical_workspace_identity`]), so a restore of this
/// snapshot against the same workspace resolves cleanly.
pub(crate) fn build_snapshot(
    session_id: SessionId,
    root_agent_id: AgentId,
    projection: &AgentProjectionTable,
    workspace: &dyn Workspace,
    at_sequence: u64,
    compacted: bool,
    compaction_generation: u64,
) -> DurableSessionSnapshot {
    let agents = projection
        .lock()
        .expect("projection mutex poisoned")
        .values()
        .cloned()
        .collect::<Vec<_>>();
    // RC-304 restore resolves each reference against the *live* integration
    // registry (see `HostRestoreResolver::resolve`), which keys factories by
    // their stable string `id()` (e.g. `"anthropic"`) — not by
    // `BackendReference.integration`, which is an opaque `IntegrationId`
    // (UUID) minted per-session and never itself registered anywhere. A
    // bare `reference.integration.to_string()` can therefore never resolve
    // by direct lookup. Encoding the backend's descriptor name alongside
    // the id (`"{id}::{descriptor_name}"`) lets the resolver additionally
    // try `IntegrationRegistry::id_for_descriptor_name`, the same fallback
    // `restore_session`'s backend-recreation step already relies on for
    // exactly this reason. A plain id with no `::` suffix (e.g. from an
    // older snapshot, or a synthetic one built directly in tests) still
    // resolves via the original direct-id path.
    let mut integration_references: Vec<String> = agents
        .iter()
        .map(|agent| {
            format!(
                "{}::{}",
                agent.backend.reference.integration, agent.backend.descriptor.name
            )
        })
        .collect();
    integration_references.sort_unstable();
    integration_references.dedup();

    DurableSessionSnapshot {
        session_id,
        root_agent_id,
        agents,
        session_sequence: at_sequence,
        timestamp: Timestamp::now(),
        schema_version: SCHEMA_VERSION,
        metadata: DurableSessionMetadata {
            workspace_identity: Some(crate::restore::canonical_workspace_identity(
                workspace.root(),
            )),
            integration_references,
            credential_profiles: Vec::new(),
            tool_policy_ids: Vec::new(),
            compacted,
            compaction_generation,
        },
    }
}

// ---------------------------------------------------------------------------
// Stored → live (snapshot restore)
// ---------------------------------------------------------------------------

/// Restores an [`AgentCapabilities`] from its opaque JSON projection.
///
/// The canonical projection persisted at snapshot time (and produced
/// symmetrically by a snapshot writer) has the shape:
///
/// ```json
/// {
///   "tools": { ... AgentToolset ... },
///   "can_spawn_agents": true,
///   "max_child_depth": 8,
///   "workspace": { "can_read": true, "can_write": false, "can_search": false },
///   "backend": { ... BackendCapabilities ... }
/// }
/// ```
///
/// `AgentToolset` and `BackendCapabilities` are protocol types that implement
/// `Deserialize` themselves; only the wrapper fields are extracted here. A
/// corrupt or missing projection logs an error and falls back to an empty,
/// non-escalating capability set so a restore can still proceed.
pub(crate) fn capabilities_from_value(value: &serde_json::Value) -> AgentCapabilities {
    match try_capabilities(value) {
        Ok(capabilities) => capabilities,
        Err(error) => {
            tracing::error!(
                %error,
                "invalid stored agent capabilities projection; restoring with empty capabilities"
            );
            AgentCapabilities {
                tools: AgentToolset {
                    tools: HashMap::new(),
                },
                can_spawn_agents: false,
                max_child_depth: None,
                workspace: WorkspaceCapabilities {
                    can_read: false,
                    can_write: false,
                    can_search: false,
                },
                backend: BackendCapabilities::default(),
            }
        }
    }
}

fn try_capabilities(value: &serde_json::Value) -> Result<AgentCapabilities, String> {
    let obj = value
        .as_object()
        .ok_or("capabilities projection is not an object")?;
    let get = |key: &str| {
        obj.get(key)
            .cloned()
            .ok_or_else(|| format!("missing field `{key}`"))
    };

    let tools: AgentToolset = serde_json::from_value(get("tools")?)
        .map_err(|error| format!("invalid `tools` projection: {error}"))?;
    let can_spawn_agents = get("can_spawn_agents")?
        .as_bool()
        .ok_or("`can_spawn_agents` is not a boolean")?;
    let max_child_depth = match get("max_child_depth")? {
        serde_json::Value::Null => None,
        value => Some(
            value
                .as_u64()
                .ok_or("`max_child_depth` is not an integer")? as u32,
        ),
    };

    let workspace_obj = get("workspace")?
        .as_object()
        .ok_or("`workspace` is not an object")?
        .clone();
    let workspace_bool = |key: &str| {
        workspace_obj
            .get(key)
            .and_then(|value| value.as_bool())
            .ok_or_else(|| format!("`workspace.{key}` is not a boolean"))
    };
    let workspace = WorkspaceCapabilities {
        can_read: workspace_bool("can_read")?,
        can_write: workspace_bool("can_write")?,
        can_search: workspace_bool("can_search")?,
    };

    let backend: BackendCapabilities = serde_json::from_value(get("backend")?)
        .map_err(|error| format!("invalid `backend` projection: {error}"))?;

    Ok(AgentCapabilities {
        tools,
        can_spawn_agents,
        max_child_depth,
        workspace,
        backend,
    })
}

/// Restores a [`UsageLedger`] from its opaque JSON projection.
///
/// Canonical shape persisted at snapshot time (and produced symmetrically by
/// a snapshot writer):
///
/// ```json
/// {
///   "records": [ ... UsageRecord ... ],
///   "child_usage": {
///     "<agent-id>": {
///       "self_usage": { ... ModelUsage ... },
///       "descendant_usage": { ... ModelUsage ... },
///       "inclusive_usage": { ... ModelUsage ... }
///     }
///   }
/// }
/// ```
///
/// `UsageRecord` and `ModelUsage` are protocol types that implement
/// `Deserialize`; only the wrapper is extracted here. A corrupt or missing
/// projection logs an error and falls back to an empty ledger.
pub(crate) fn usage_from_value(value: &serde_json::Value) -> UsageLedger {
    match try_usage(value) {
        Ok(usage) => usage,
        Err(error) => {
            tracing::error!(
                %error,
                "invalid stored agent usage projection; restoring with an empty ledger"
            );
            UsageLedger::default()
        }
    }
}

fn try_usage(value: &serde_json::Value) -> Result<UsageLedger, String> {
    let obj = value
        .as_object()
        .ok_or("usage projection is not an object")?;

    let records: Vec<UsageRecord> = serde_json::from_value(
        obj.get("records")
            .cloned()
            .unwrap_or_else(|| serde_json::json!([])),
    )
    .map_err(|error| format!("invalid `records` projection: {error}"))?;

    let mut child_usage: HashMap<AgentId, CoreAgentUsageSummary> = HashMap::new();
    if let Some(children) = obj.get("child_usage").and_then(|value| value.as_object()) {
        for (agent_id, summary_value) in children {
            let agent_id = AgentId::from_str(agent_id)
                .map_err(|error| format!("invalid child_usage key {agent_id:?}: {error}"))?;
            child_usage.insert(agent_id, usage_summary_from_value(summary_value)?);
        }
    }

    // M4, additive: older snapshots predate `tool_calls` — default to 0
    // rather than rejecting the whole projection, matching the same
    // graceful-fallback discipline as every other field here.
    let tool_calls = obj
        .get("tool_calls")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);

    // M4/M5, additive: older snapshots predate `runs` — default to 0 for
    // exactly the same reason `tool_calls` does, above.
    let runs = obj
        .get("runs")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);

    Ok(UsageLedger {
        records,
        tool_calls,
        runs,
        child_usage,
    })
}

fn usage_summary_from_value(value: &serde_json::Value) -> Result<CoreAgentUsageSummary, String> {
    let obj = value
        .as_object()
        .ok_or("child usage summary is not an object")?;
    let get = |key: &str| {
        obj.get(key)
            .cloned()
            .ok_or_else(|| format!("missing field `{key}`"))
    };
    Ok(CoreAgentUsageSummary {
        self_usage: serde_json::from_value(get("self_usage")?)
            .map_err(|error| format!("invalid `self_usage` projection: {error}"))?,
        descendant_usage: serde_json::from_value(get("descendant_usage")?)
            .map_err(|error| format!("invalid `descendant_usage` projection: {error}"))?,
        inclusive_usage: serde_json::from_value(get("inclusive_usage")?)
            .map_err(|error| format!("invalid `inclusive_usage` projection: {error}"))?,
    })
}
