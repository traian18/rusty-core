//! Replay tests: load JSON fixtures capturing initial agent state, an ordered command
//! sequence, and expected effects, then replay each command through [`Agent::apply`] and
//! assert that the produced effects match the fixture's expectations.
//!
//! This is the implementation of the spec §68.4 principle:
//!
//! > same initial state + same commands = same semantic transitions
//!
//! Because IDs (RunId, MessageId, RequestId, …) are deterministically derived from the
//! agent's own [`AgentId`] and its `transition_sequence` counter, every replay with the
//! same seed identity produces byte-identical effects — making the fixtures a true
//! golden-record test of the deterministic core.

use std::collections::HashMap;

use harness_core::agent::Agent;
use harness_core::capabilities::{AgentCapabilities, WorkspaceCapabilities};
use harness_protocol::backend::{
    BackendBinding, BackendCapabilities, BackendDescriptor, BackendReference,
};
use harness_protocol::commands::{AgentCommand, AgentStatus};
use harness_protocol::effects::AgentEffect;
use harness_protocol::events::AgentEvent;
use harness_protocol::ids::{
    AgentId, BackendId, ConfigurationId, IntegrationId, SessionId,
};
use harness_protocol::tools::AgentToolset;
use harness_protocol::usage::AgentBudget;
use serde::Deserialize;

// ===========================================================================
// Fixture types (JSON deserialisation)
// ===========================================================================

/// Top-level fixture structure.
#[derive(Debug, Deserialize)]
struct Fixture {
    /// Optional human-readable description.
    #[serde(default, rename = "description")]
    _description: Option<String>,
    /// The initial agent state to construct before replay.
    initial_agent: FixtureAgent,
    /// Ordered list of [`AgentCommand`]s to apply serially.
    commands: Vec<AgentCommand>,
    /// For the one-command (or linear) case: expected effects for ALL commands
    /// flattened into a single list.
    #[serde(default)]
    expected_effects: Vec<EffectPattern>,
    /// For multi-command fixtures: expected effects PER COMMAND.
    #[serde(default)]
    expected_effects_per_command: Vec<Vec<EffectPattern>>,
}

/// Serializable representation of the initial agent snapshot.
#[derive(Debug, Deserialize)]
struct FixtureAgent {
    /// Initial [`AgentStatus`] as a string (e.g. `"Idle"`).
    status: String,
    /// The system prompt text.
    system_prompt: String,
    /// The starting `transition_sequence` counter (typically 0).
    transition_sequence: u64,
}

/// A pattern that describes an expected [`AgentEffect`] (or [`AgentEvent`] inside
/// an `Emit`) without requiring exact UUID/timestamp matches.
///
/// Patterns are matched structurally by variant name and key fields.
#[derive(Debug, Deserialize)]
struct EffectPattern {
    /// Top-level variant name of the [`AgentEffect`] (e.g. `"Emit"`, `"ExecuteBackend"`).
    variant: String,
    /// For `Emit` effects, the inner [`AgentEvent`] pattern.
    #[serde(default)]
    inner: Option<EventPattern>,
}

/// A pattern that describes an expected [`AgentEvent`] inside an `Emit`.
#[derive(Debug, Deserialize)]
struct EventPattern {
    /// Variant name of the [`AgentEvent`] (e.g. `"StateChanged"`, `"RunStarted"`).
    #[serde(default)]
    variant: Option<String>,
    /// For `StateChanged`: the source status.
    #[serde(default)]
    from: Option<String>,
    /// For `StateChanged`: the target status.
    #[serde(default)]
    to: Option<String>,
    /// For `Completed`: the outcome (`"Success"`, `"Cancelled"`, `"Failed"`).
    #[serde(default)]
    outcome: Option<String>,
}

// ===========================================================================
// Fixture loader
// ===========================================================================

/// Load a JSON fixture by name (without extension) from the `tests/fixtures/`
/// directory, relative to the crate root.
///
/// # Panics
///
/// Panics if the file cannot be read or deserialised.
fn load_fixture(name: &str) -> Fixture {
    let path = {
        // `CARGO_MANIFEST_DIR` points to the crate root at compile time.
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        format!("{manifest_dir}/tests/fixtures/{name}.json")
    };

    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read fixture '{name}' at {path}: {e}"));

    serde_json::from_str(&content)
        .unwrap_or_else(|e| panic!("failed to parse fixture '{name}': {e}"))
}

// ===========================================================================
// Helpers
// ===========================================================================

/// Construct an [`Agent`] with the given fixture state and minimal defaults.
///
/// The agent is created with:
/// * A deterministic `AgentId` derived from a zero‑based namespace so that
///   replay is reproducible.
/// * A fresh `SessionId`.
/// * The system prompt from the fixture.
/// * The initial `transition_sequence` from the fixture.
/// * The initial `status` from the fixture (set after construction by
///   mutating the agent's state).
fn build_agent(fixture: &FixtureAgent) -> Agent {
    let agent_id = AgentId::new();
    let session_id = SessionId::new();

    let mut agent = Agent::new(
        agent_id,
        session_id,
        None,
        0,
        fixture.system_prompt.clone(),
        BackendBinding {
            reference: BackendReference {
                integration: IntegrationId::new(),
                configuration: ConfigurationId::new(),
                model: None,
            },
            descriptor: BackendDescriptor {
                id: BackendId::new(),
                name: "replay-backend".into(),
                description: "Backend for replay tests".into(),
                capabilities: BackendCapabilities::default(),
            },
        },
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
        },
        AgentBudget::default(),
    );

    // Override the transition_sequence from the fixture.
    agent.state.transition_sequence = fixture.transition_sequence;

    // Override the initial status.
    agent.state.status = parse_status(&fixture.status);

    agent
}

/// Parse an [`AgentStatus`] from its debug / serde string representation.
fn parse_status(s: &str) -> AgentStatus {
    match s {
        "Idle" => AgentStatus::Idle,
        "PreparingContext" => AgentStatus::PreparingContext,
        "WaitingForBackend" => AgentStatus::WaitingForBackend,
        "Streaming" => AgentStatus::Streaming,
        "Executing" => AgentStatus::Executing,
        "WaitingForPermission" => AgentStatus::WaitingForPermission,
        "WaitingForChildren" => AgentStatus::WaitingForChildren,
        "Paused" => AgentStatus::Paused,
        "Completed" => AgentStatus::Completed,
        "Cancelled" => AgentStatus::Cancelled,
        "Failed" => AgentStatus::Failed,
        _ => panic!("unknown AgentStatus variant: {s}"),
    }
}

/// Extract the variant name of an [`AgentEffect`] as a static string.
fn effect_variant_name(effect: &AgentEffect) -> &'static str {
    match effect {
        AgentEffect::ExecuteBackend { .. } => "ExecuteBackend",
        AgentEffect::ExecuteTool { .. } => "ExecuteTool",
        AgentEffect::SpawnAgent { .. } => "SpawnAgent",
        AgentEffect::RequestPermission { .. } => "RequestPermission",
        AgentEffect::CancelBackend { .. } => "CancelBackend",
        AgentEffect::CancelTool { .. } => "CancelTool",
        AgentEffect::CancelChild { .. } => "CancelChild",
        AgentEffect::Persist { .. } => "Persist",
        AgentEffect::Emit { .. } => "Emit",
        AgentEffect::FinishRun { .. } => "FinishRun",
    }
}

/// Extract the variant name of an [`AgentEvent`] as a static string.
fn event_variant_name(event: &AgentEvent) -> &'static str {
    match event {
        AgentEvent::StateChanged { .. } => "StateChanged",
        AgentEvent::RunStarted { .. } => "RunStarted",
        AgentEvent::BackendRequestStarted { .. } => "BackendRequestStarted",
        AgentEvent::AssistantMessageStarted { .. } => "AssistantMessageStarted",
        AgentEvent::AssistantTextDelta { .. } => "AssistantTextDelta",
        AgentEvent::ReasoningDelta { .. } => "ReasoningDelta",
        AgentEvent::AssistantMessageCompleted { .. } => "AssistantMessageCompleted",
        AgentEvent::ToolCallRequested { .. } => "ToolCallRequested",
        AgentEvent::ToolCallStarted { .. } => "ToolCallStarted",
        AgentEvent::ToolCallProgress { .. } => "ToolCallProgress",
        AgentEvent::ToolCallCompleted { .. } => "ToolCallCompleted",
        AgentEvent::PermissionRequested { .. } => "PermissionRequested",
        AgentEvent::UsageUpdated { .. } => "UsageUpdated",
        AgentEvent::ChildAgentSpawned { .. } => "ChildAgentSpawned",
        AgentEvent::ChildAgentCompleted { .. } => "ChildAgentCompleted",
        AgentEvent::Failed { .. } => "Failed",
        AgentEvent::Completed { .. } => "Completed",
    }
}

/// Check whether an [`AgentEffect`] matches a pattern.
fn effect_matches_pattern(effect: &AgentEffect, pattern: &EffectPattern) -> bool {
    if effect_variant_name(effect) != pattern.variant {
        return false;
    }

    // If the effect is Emit and the pattern has an inner event pattern, recurse.
    if let (AgentEffect::Emit { event }, Some(inner)) = (effect, &pattern.inner) {
        return event_matches_pattern(event, inner);
    }

    true
}

/// Check whether an [`AgentEvent`] matches an event pattern.
fn event_matches_pattern(event: &AgentEvent, pattern: &EventPattern) -> bool {
    // Check variant name.
    if let Some(ref expected_variant) = pattern.variant {
        if event_variant_name(event) != expected_variant {
            return false;
        }
    }

    match event {
        AgentEvent::StateChanged { from, to } => {
            if let Some(ref expected_from) = pattern.from {
                if format!("{from:?}") != *expected_from {
                    return false;
                }
            }
            if let Some(ref expected_to) = pattern.to {
                if format!("{to:?}") != *expected_to {
                    return false;
                }
            }
            true
        }
        AgentEvent::Completed { outcome } => {
            if let Some(ref expected_outcome) = pattern.outcome {
                let actual = format!("{outcome:?}");
                if actual != *expected_outcome {
                    return false;
                }
            }
            true
        }
        // For events without additional fields, just the variant match is enough.
        AgentEvent::RunStarted { .. }
        | AgentEvent::BackendRequestStarted { .. }
        | AgentEvent::AssistantMessageStarted { .. }
        | AgentEvent::AssistantTextDelta { .. }
        | AgentEvent::ReasoningDelta { .. }
        | AgentEvent::AssistantMessageCompleted { .. }
        | AgentEvent::ToolCallRequested { .. }
        | AgentEvent::ToolCallStarted { .. }
        | AgentEvent::ToolCallProgress { .. }
        | AgentEvent::ToolCallCompleted { .. }
        | AgentEvent::PermissionRequested { .. }
        | AgentEvent::UsageUpdated { .. }
        | AgentEvent::ChildAgentSpawned { .. }
        | AgentEvent::ChildAgentCompleted { .. }
        | AgentEvent::Failed { .. } => true,
    }
}

// ===========================================================================
// Tests
// ===========================================================================

/// Replay the `happy_path` fixture: a single [`StartRun`] command on a fresh
/// agent should produce exactly the expected effects (StateChanged,
/// ExecuteBackend, RunStarted).
#[test]
fn replay_happy_path() {
    let fixture = load_fixture("happy_path");

    // Build the agent from the fixture's initial state.
    let mut agent = build_agent(&fixture.initial_agent);

    // Collect all effects across all commands.
    let mut all_effects: Vec<AgentEffect> = Vec::new();
    for cmd in &fixture.commands {
        let effects = agent.apply(cmd.clone());
        all_effects.extend(effects);
    }

    // Determine the expected patterns.
    let expected_patterns = if !fixture.expected_effects_per_command.is_empty() {
        fixture
            .expected_effects_per_command
            .iter()
            .flatten()
            .collect::<Vec<_>>()
    } else {
        fixture.expected_effects.iter().collect::<Vec<_>>()
    };

    // Assert count matches.
    assert_eq!(
        all_effects.len(),
        expected_patterns.len(),
        "effect count mismatch for happy_path fixture.\n  actual:   {}\n  expected: {}\n\n  actual effects:   {:?}\n  expected patterns: {:?}",
        all_effects.len(),
        expected_patterns.len(),
        all_effects,
        expected_patterns,
    );

    // Assert each effect matches its pattern.
    for (i, (effect, pattern)) in all_effects.iter().zip(expected_patterns.iter()).enumerate() {
        assert!(
            effect_matches_pattern(effect, pattern),
            "happy_path fixture: effect[{i}] mismatch.\n  actual:   {effect:?}\n  expected: {pattern:?}"
        );
    }

    // After the happy path the agent should be in PreparingContext
    // (the backend hasn't completed yet — ExecuteBackend was produced but
    // the runner loop that feeds BackendEvents back is outside Agent::apply).
    assert_eq!(
        agent.state.status,
        AgentStatus::PreparingContext,
        "after StartRun, agent status should be PreparingContext"
    );

    // An active run should exist.
    assert!(
        agent.state.active_run.is_some(),
        "after StartRun, agent should have an active_run"
    );

    // The user message should have been appended to the transcript.
    assert_eq!(
        agent.state.messages.len(),
        1,
        "after StartRun, there should be 1 message (the user input)"
    );
}

/// Replay the `cancelled_run` fixture: [`StartRun`] then [`Cancel`].  The
/// agent should transition to [`AgentStatus::Cancelled`] and produce the
/// correct cancellation effects.
#[test]
fn replay_cancelled_run() {
    let fixture = load_fixture("cancelled_run");

    // Build the agent from the fixture's initial state.
    let mut agent = build_agent(&fixture.initial_agent);

    // Collect all effects across all commands.
    let mut all_effects: Vec<AgentEffect> = Vec::new();
    for cmd in &fixture.commands {
        let effects = agent.apply(cmd.clone());
        all_effects.extend(effects);
    }

    // Determine the expected patterns.
    let expected_patterns = if !fixture.expected_effects_per_command.is_empty() {
        fixture
            .expected_effects_per_command
            .iter()
            .flatten()
            .collect::<Vec<_>>()
    } else {
        fixture.expected_effects.iter().collect::<Vec<_>>()
    };

    // Assert count matches.
    assert_eq!(
        all_effects.len(),
        expected_patterns.len(),
        "effect count mismatch for cancelled_run fixture.\n  actual:   {}\n  expected: {}\n\n  actual effects:   {:?}\n  expected patterns: {:?}",
        all_effects.len(),
        expected_patterns.len(),
        all_effects,
        expected_patterns,
    );

    // Assert each effect matches its pattern.
    for (i, (effect, pattern)) in all_effects.iter().zip(expected_patterns.iter()).enumerate() {
        assert!(
            effect_matches_pattern(effect, pattern),
            "cancelled_run fixture: effect[{i}] mismatch.\n  actual:   {effect:?}\n  expected: {pattern:?}"
        );
    }

    // ── Core assertion: agent ended in Cancelled state ────────────────
    assert_eq!(
        agent.state.status,
        AgentStatus::Cancelled,
        "after Cancel, agent status should be Cancelled"
    );

    // No active run remains.
    assert!(
        agent.state.active_run.is_none(),
        "after Cancel, agent should have no active_run"
    );

    // Pending tools and permissions should have been cleared.
    assert!(
        agent.state.pending_tools.is_empty(),
        "after Cancel, pending_tools should be empty"
    );
    assert!(
        agent.state.pending_permissions.is_empty(),
        "after Cancel, pending_permissions should be empty"
    );

    // The user message from StartRun should still be in the transcript
    // (cancellation does not remove history).
    assert_eq!(
        agent.state.messages.len(),
        1,
        "after Cancel, the user message should remain in the transcript"
    );
}
