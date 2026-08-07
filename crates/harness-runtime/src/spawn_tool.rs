//! M5: the `agent.spawn` tool — a model-facing entry point for spawning a
//! child agent, wired through the exact same `AgentSupervisor`-enforced
//! path that Rust-orchestrated spawning already uses (see
//! `AgentRunner::spawn_agent`, called from both `AgentEffect::SpawnAgent`
//! and this tool's interception in `AgentRunner::execute_tool`). This
//! module only builds a `SpawnAgentSpec` from a model's tool-call
//! arguments and applies the least-privilege delegation defaults described
//! below — it never touches the supervisor, budgets, or capability
//! derivation directly, so a model can never spawn a child that bypasses
//! parent limits any more than existing Rust orchestration code could.

use harness_core::agent::Agent;
use harness_protocol::effects::{BackendPolicy, SpawnAgentSpec, SpawnMode, ToolInheritance, WorkspacePolicy};
use harness_protocol::ids::ToolId;
use harness_protocol::tools::ToolDescriptor;
use harness_protocol::usage::AgentBudget;
use serde::Deserialize;

/// Stable name a model calls this tool by, and the name a host registers it
/// under in an `AgentToolset` (see the module doc for wiring instructions).
pub const AGENT_SPAWN_TOOL_NAME: &str = "agent.spawn";

/// Tool names granted to a spawned child by default when the calling model
/// doesn't specify `tools` explicitly — a conservative, read-only subset of
/// this workspace's own built-in tools (see `crates/tools/*`). Only names
/// the *parent* actually has, delegatable and enabled, are ever granted
/// (`ToolInheritance::Subset` rejects anything else — see
/// `AgentCapabilities::derive_child_capabilities`); this list is a ceiling,
/// not a promise. Mutating/executing tools (`fs.edit`, `shell.exec`) are
/// deliberately excluded from the default and must be requested explicitly
/// by name — the M5 roadmap's "least-privilege default" requirement.
const DEFAULT_DELEGATED_TOOL_NAMES: &[&str] = &[
    "fs.read",
    "workspace.search",
    "git.status",
    "git.diff",
    "git.log",
    "git.show",
    "web.fetch",
];

/// JSON-schema-shaped tool-call arguments for `agent.spawn`. Deserialized
/// directly from the model's tool-call `arguments` value.
#[derive(Debug, Deserialize)]
pub struct SpawnToolArgs {
    /// What the child agent should do — becomes its first `StartRun` prompt.
    pub task: String,
    /// Role/system-prompt framing for the child (e.g. `"code reviewer"`,
    /// `"task auditor"`). Defaults to an empty system prompt when omitted.
    #[serde(default)]
    pub role: Option<String>,
    /// Explicit tool-name allowlist for the child. When omitted, defaults
    /// to `DEFAULT_DELEGATED_TOOL_NAMES` filtered down to what the parent
    /// actually has delegatable+enabled — never the parent's full toolset.
    #[serde(default)]
    pub tools: Option<Vec<String>>,
    /// `"inherit"` | `"read_only"` | `"snapshot"` | `"new_worktree"`.
    /// Defaults to `"read_only"` — a spawned child cannot mutate the
    /// parent's workspace unless explicitly granted a policy that allows
    /// it. See `harness_protocol::effects::WorkspacePolicy`.
    #[serde(default)]
    pub workspace: Option<String>,
    /// `"await"` | `"concurrent"`. Defaults to `"await"`: the spawning
    /// tool call blocks until the child finishes and its result becomes
    /// part of this `ToolResult` — the useful default for a "delegate this
    /// subtask and use its answer" pattern. `"concurrent"` returns as soon
    /// as the child starts, for fire-and-forget background work.
    #[serde(default)]
    pub mode: Option<String>,
    /// Budget overrides for the child. Any field left unset here defaults
    /// to the *parent's own* corresponding budget value (never unset when
    /// the parent has a limit — `AgentSupervisor`'s `validate_child_budget`
    /// treats an unset child field as an escalation attempt if the parent
    /// has a limit set, so "inherit the parent's ceiling by default" is the
    /// only default that's always valid). A model may only tighten these,
    /// never loosen them — a looser request is rejected by the same
    /// validation every other spawn path already goes through.
    #[serde(default)]
    pub budget: Option<AgentBudget>,
    /// Optional model override for the child, e.g. `"claude-haiku-4-5"` to
    /// delegate a cheap/fast subtask to a smaller model than the parent's
    /// own. The child otherwise inherits the parent's backend/provider
    /// unchanged (see `SpawnAgentSpec::execution_params`'s doc comment for
    /// why this does not go through `BackendPolicy::Explicit`) — this can
    /// only select a different model on that same provider, not switch
    /// provider entirely; there is currently no supported way for this tool
    /// to request an explicit different *backend*.
    #[serde(default)]
    pub model: Option<String>,
}

/// Parses and validates raw tool-call arguments into a `SpawnAgentSpec`,
/// applying the delegation-policy defaults described on `SpawnToolArgs`.
///
/// Returns `(spec, warnings)` — `warnings` lists any explicitly requested
/// tool name that could not be granted (not present on the parent, not
/// delegatable, or disabled), so the tool's result can tell the calling
/// model what it actually got instead of silently under-granting.
///
/// This function never consults the supervisor, never enforces budgets
/// itself, and never derives capabilities — it only builds the *request*;
/// `AgentSupervisor::spawn_child` (reached via `AgentRunner::spawn_agent`)
/// is the single place all of that is actually enforced, exactly as for
/// every other spawn path.
pub fn build_spawn_spec(
    arguments: &serde_json::Value,
    parent: &Agent,
) -> Result<(SpawnAgentSpec, Vec<String>), String> {
    let args: SpawnToolArgs = serde_json::from_value(arguments.clone())
        .map_err(|error| format!("invalid agent.spawn arguments: {error}"))?;

    if args.task.trim().is_empty() {
        return Err("agent.spawn requires a non-empty `task`".to_string());
    }

    let workspace = match args.workspace.as_deref() {
        None | Some("read_only") => WorkspacePolicy::ReadOnly,
        Some("inherit") => WorkspacePolicy::Inherit,
        Some("snapshot") => WorkspacePolicy::Snapshot,
        Some("new_worktree") => WorkspacePolicy::NewWorktree,
        Some(other) => {
            return Err(format!(
                "invalid `workspace` value {other:?}; expected one of \
                 \"inherit\" | \"read_only\" | \"snapshot\" | \"new_worktree\""
            ))
        }
    };

    let mode = match args.mode.as_deref() {
        None | Some("await") => SpawnMode::AwaitResult,
        Some("concurrent") => SpawnMode::Concurrent,
        Some(other) => {
            return Err(format!(
                "invalid `mode` value {other:?}; expected \"await\" or \"concurrent\""
            ))
        }
    };

    let (tools, warnings) = resolve_tool_inheritance(args.tools.as_deref(), parent);

    // Default every unset budget field to the parent's own value — the
    // only default that's always valid against `validate_child_budget`
    // (see `SpawnToolArgs::budget`'s doc comment). An explicit override in
    // `args.budget` wins per-field; leaving a field `None` in the override
    // falls back to the parent's value, not to "no limit."
    let budget = match args.budget {
        Some(requested) => merge_budget_defaults(requested, &parent.budget),
        None => parent.budget.clone(),
    };

    let execution_params = harness_protocol::backend::ExecutionParams {
        model: args.model,
        ..Default::default()
    };

    let spec = SpawnAgentSpec {
        role: args.role,
        backend: BackendPolicy::Inherit,
        tools,
        workspace,
        budget,
        mode,
        task: Some(args.task),
        execution_params,
    };

    Ok((spec, warnings))
}

fn merge_budget_defaults(requested: AgentBudget, parent: &AgentBudget) -> AgentBudget {
    AgentBudget {
        max_input_tokens: requested.max_input_tokens.or(parent.max_input_tokens),
        max_output_tokens: requested.max_output_tokens.or(parent.max_output_tokens),
        max_total_tokens: requested.max_total_tokens.or(parent.max_total_tokens),
        max_cost_usd: requested.max_cost_usd.or(parent.max_cost_usd),
        max_requests: requested.max_requests.or(parent.max_requests),
        max_tool_calls: requested.max_tool_calls.or(parent.max_tool_calls),
        max_children: requested.max_children.or(parent.max_children),
        max_depth: requested.max_depth.or(parent.max_depth),
    }
}

/// Resolves requested tool names against the parent's own toolset.
///
/// A requested name not found among the parent's tools — or found but not
/// `delegatable`/not `enabled` — is silently excluded from the grant (never
/// escalated to) and reported back in the returned warning list. When
/// `requested` is `None`, falls back to `DEFAULT_DELEGATED_TOOL_NAMES`
/// filtered the same way, with no warnings (a default that grants nothing
/// because the parent has none of those tools delegatable is not a caller
/// mistake worth flagging).
fn resolve_tool_inheritance(
    requested: Option<&[String]>,
    parent: &Agent,
) -> (ToolInheritance, Vec<String>) {
    let (names, warn_on_miss): (Vec<&str>, bool) = match requested {
        Some(names) => (names.iter().map(String::as_str).collect(), true),
        None => (DEFAULT_DELEGATED_TOOL_NAMES.to_vec(), false),
    };

    let mut ids: Vec<ToolId> = Vec::new();
    let mut warnings = Vec::new();
    for name in names {
        match find_delegatable_tool_id(parent, name) {
            Some(id) => ids.push(id),
            None if warn_on_miss => warnings.push(format!(
                "tool {name:?} was not granted to the spawned child (not present, not \
                 delegatable, or disabled on the parent)"
            )),
            None => {}
        }
    }

    (ToolInheritance::Subset(ids), warnings)
}

fn find_delegatable_tool_id(parent: &Agent, name: &str) -> Option<ToolId> {
    parent
        .capabilities
        .tools
        .tools
        .iter()
        .find(|(_, capability)| {
            capability.descriptor.name == name && capability.delegatable && capability.policy.enabled
        })
        .map(|(id, _)| *id)
}

/// Builds the `ToolDescriptor` a host adds to a session's `AgentToolset` to
/// make `agent.spawn` callable by the model. Not registered in any
/// `ToolRegistry` — `AgentRunner::execute_tool` intercepts this name before
/// ever consulting the registry, so no `ToolExecutor` implementation is
/// needed or used.
///
/// Hosts are expected to gate this with `PermissionMode::Ask` (M5's
/// recommended default — spawning creates new billable model calls and new
/// tool-call surface area the user hasn't reviewed) and `delegatable:
/// false` (a grandchild spawning further descendants is a deliberate,
/// separate decision a host should make explicitly, not something granted
/// automatically by handing a child this tool).
pub fn agent_spawn_tool_descriptor(id: ToolId) -> ToolDescriptor {
    ToolDescriptor {
        id,
        name: AGENT_SPAWN_TOOL_NAME.to_string(),
        description: "Delegate a subtask to a new child agent. Use this to break work into \
            independently-scoped pieces (e.g. a focused research task, a code-review pass) \
            rather than doing everything in this conversation. The child starts with a \
            least-privilege, read-only tool set and a read-only view of the workspace unless \
            you explicitly request otherwise."
            .to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "What the child agent should do — its first prompt."
                },
                "role": {
                    "type": "string",
                    "description": "Optional role/system-prompt framing for the child, e.g. \"code reviewer\"."
                },
                "tools": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional explicit tool-name allowlist for the child. Omit for a conservative read-only default; only tools you already have, marked delegatable, can be granted."
                },
                "workspace": {
                    "type": "string",
                    "enum": ["inherit", "read_only", "snapshot", "new_worktree"],
                    "description": "Workspace policy for the child. Defaults to \"read_only\"."
                },
                "mode": {
                    "type": "string",
                    "enum": ["await", "concurrent"],
                    "description": "\"await\" (default) blocks until the child finishes and returns its result here. \"concurrent\" returns immediately; the child continues in the background."
                },
                "budget": {
                    "type": "object",
                    "description": "Optional budget overrides for the child (tightens, never loosens, whatever budget you yourself have)."
                },
                "model": {
                    "type": "string",
                    "description": "Optional model override for the child, e.g. a smaller/cheaper model for a simple subtask. Selects a different model on your own provider only — there is no way to request a different provider/backend through this tool."
                }
            },
            "required": ["task"]
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use harness_protocol::backend::{BackendBinding, BackendCapabilities, BackendDescriptor, BackendReference};
    use harness_protocol::ids::{AgentId, BackendId, ConfigurationId, IntegrationId, SessionId};
    use harness_protocol::tools::{AgentToolset, PermissionMode, ToolCapability, ToolPolicy};

    use super::*;
    use harness_core::capabilities::{AgentCapabilities, WorkspaceCapabilities};

    fn tool_capability(name: &str, delegatable: bool, enabled: bool) -> (ToolId, ToolCapability) {
        let id = ToolId::new();
        (
            id,
            ToolCapability {
                descriptor: ToolDescriptor {
                    id,
                    name: name.to_string(),
                    description: "test tool".to_string(),
                    input_schema: serde_json::json!({"type": "object"}),
                },
                policy: ToolPolicy {
                    permission: PermissionMode::Allow,
                    enabled,
                },
                delegatable,
            },
        )
    }

    fn parent_agent(tools: Vec<(ToolId, ToolCapability)>, budget: AgentBudget) -> Agent {
        Agent::new(
            AgentId::new(),
            SessionId::new(),
            None,
            0,
            "parent system prompt".into(),
            BackendBinding {
                reference: BackendReference {
                    integration: IntegrationId::new(),
                    configuration: ConfigurationId::new(),
                    model: None,
                },
                descriptor: BackendDescriptor {
                    id: BackendId::new(),
                    name: "fake".into(),
                    description: "fake".into(),
                    capabilities: BackendCapabilities::default(),
                },
            },
            AgentCapabilities {
                tools: AgentToolset { tools: tools.into_iter().collect::<HashMap<_, _>>() },
                can_spawn_agents: true,
                max_child_depth: Some(3),
                workspace: WorkspaceCapabilities { can_read: true, can_write: false, can_search: true },
                backend: BackendCapabilities::default(),
            },
            budget,
        )
    }

    #[test]
    fn missing_task_is_rejected() {
        let parent = parent_agent(vec![], AgentBudget::default());
        let result = build_spawn_spec(&serde_json::json!({}), &parent);
        assert!(result.is_err());
    }

    #[test]
    fn blank_task_is_rejected() {
        let parent = parent_agent(vec![], AgentBudget::default());
        let result = build_spawn_spec(&serde_json::json!({"task": "   "}), &parent);
        assert!(result.is_err());
    }

    #[test]
    fn defaults_are_least_privilege_read_only() {
        let read_tool = tool_capability("fs.read", true, true);
        let edit_tool = tool_capability("fs.edit", true, true);
        let parent = parent_agent(vec![read_tool.clone(), edit_tool], AgentBudget::default());

        let (spec, warnings) =
            build_spawn_spec(&serde_json::json!({"task": "look around"}), &parent).unwrap();

        assert!(warnings.is_empty());
        assert_eq!(spec.workspace, WorkspacePolicy::ReadOnly);
        assert_eq!(spec.mode, SpawnMode::AwaitResult);
        match spec.tools {
            ToolInheritance::Subset(ids) => {
                assert_eq!(ids, vec![read_tool.0], "fs.edit must not be in the default grant");
            }
            other => panic!("expected Subset, got {other:?}"),
        }
    }

    #[test]
    fn explicit_tool_request_is_honored_when_delegatable() {
        let edit_tool = tool_capability("fs.edit", true, true);
        let parent = parent_agent(vec![edit_tool.clone()], AgentBudget::default());

        let (spec, warnings) = build_spawn_spec(
            &serde_json::json!({"task": "fix the bug", "tools": ["fs.edit"]}),
            &parent,
        )
        .unwrap();

        assert!(warnings.is_empty());
        match spec.tools {
            ToolInheritance::Subset(ids) => assert_eq!(ids, vec![edit_tool.0]),
            other => panic!("expected Subset, got {other:?}"),
        }
    }

    /// M5: an explicit `model` argument becomes an `execution_params.model`
    /// override on the built spec, applied to the child's `AgentState` at
    /// construction by `AgentSupervisor::spawn_child` — see
    /// `SpawnAgentSpec::execution_params`'s doc comment for why this goes
    /// through `ExecutionParams` rather than `BackendPolicy::Explicit`.
    #[test]
    fn model_override_becomes_an_execution_params_override() {
        let parent = parent_agent(vec![], AgentBudget::default());
        let (spec, _warnings) = build_spawn_spec(
            &serde_json::json!({"task": "summarize this file", "model": "claude-haiku-4-5"}),
            &parent,
        )
        .unwrap();

        assert_eq!(spec.execution_params.model.as_deref(), Some("claude-haiku-4-5"));
        assert!(
            matches!(spec.backend, BackendPolicy::Inherit),
            "a model override must not switch the child to a different backend policy"
        );
    }

    /// Omitting `model` entirely must leave `execution_params` at its
    /// default (no override) — the pre-existing behavior for every spawn
    /// that doesn't ask for a model override.
    #[test]
    fn no_model_argument_leaves_execution_params_unset() {
        let parent = parent_agent(vec![], AgentBudget::default());
        let (spec, _warnings) =
            build_spawn_spec(&serde_json::json!({"task": "summarize this file"}), &parent).unwrap();

        assert_eq!(spec.execution_params.model, None);
    }

    #[test]
    fn non_delegatable_tool_is_never_granted_even_if_requested() {
        let locked_tool = tool_capability("shell.exec", false, true);
        let parent = parent_agent(vec![locked_tool], AgentBudget::default());

        let (spec, warnings) = build_spawn_spec(
            &serde_json::json!({"task": "run a command", "tools": ["shell.exec"]}),
            &parent,
        )
        .unwrap();

        assert_eq!(warnings.len(), 1, "the ungranted request must be reported, not silent");
        match spec.tools {
            ToolInheritance::Subset(ids) => assert!(ids.is_empty(), "non-delegatable tool must never be granted"),
            other => panic!("expected Subset, got {other:?}"),
        }
    }

    #[test]
    fn requesting_a_tool_the_parent_does_not_have_is_never_granted() {
        let parent = parent_agent(vec![], AgentBudget::default());
        let (spec, warnings) = build_spawn_spec(
            &serde_json::json!({"task": "do something", "tools": ["fs.edit"]}),
            &parent,
        )
        .unwrap();
        assert_eq!(warnings.len(), 1);
        match spec.tools {
            ToolInheritance::Subset(ids) => assert!(ids.is_empty()),
            other => panic!("expected Subset, got {other:?}"),
        }
    }

    #[test]
    fn unset_budget_defaults_to_the_parents_own_ceiling_not_unlimited() {
        let parent_budget = AgentBudget {
            max_requests: Some(10),
            max_cost_usd: Some(rust_decimal_macros::dec!(5.00)),
            ..Default::default()
        };
        let parent = parent_agent(vec![], parent_budget.clone());

        let (spec, _) = build_spawn_spec(&serde_json::json!({"task": "go"}), &parent).unwrap();

        assert_eq!(spec.budget.max_requests, parent_budget.max_requests);
        assert_eq!(spec.budget.max_cost_usd, parent_budget.max_cost_usd);
    }

    #[test]
    fn explicit_tighter_budget_override_is_preserved() {
        let parent_budget = AgentBudget { max_requests: Some(10), ..Default::default() };
        let parent = parent_agent(vec![], parent_budget);

        let (spec, _) = build_spawn_spec(
            &serde_json::json!({"task": "go", "budget": {"max_requests": 3}}),
            &parent,
        )
        .unwrap();

        assert_eq!(spec.budget.max_requests, Some(3));
    }

    #[test]
    fn invalid_workspace_value_is_rejected() {
        let parent = parent_agent(vec![], AgentBudget::default());
        let result = build_spawn_spec(
            &serde_json::json!({"task": "go", "workspace": "not-a-real-policy"}),
            &parent,
        );
        assert!(result.is_err());
    }

    #[test]
    fn concurrent_mode_parses_correctly() {
        let parent = parent_agent(vec![], AgentBudget::default());
        let (spec, _) = build_spawn_spec(
            &serde_json::json!({"task": "go", "mode": "concurrent"}),
            &parent,
        )
        .unwrap();
        assert_eq!(spec.mode, SpawnMode::Concurrent);
    }

    #[test]
    fn descriptor_has_the_stable_tool_name_and_requires_task() {
        let descriptor = agent_spawn_tool_descriptor(ToolId::new());
        assert_eq!(descriptor.name, AGENT_SPAWN_TOOL_NAME);
        assert_eq!(descriptor.input_schema["required"], serde_json::json!(["task"]));
    }
}
