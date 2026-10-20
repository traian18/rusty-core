use harness_protocol::{
    events::AgentEvent,
    ids::{AgentId, ToolCallId},
    tools::ToolCall,
};

/// Formats a one-line activity-log summary for `event`, or `None` for the
/// two kinds excluded from the log. Streaming deltas are already visible in
/// the curated transcript and are intentionally omitted from the raw log.
pub(super) fn log_summary(event: &AgentEvent) -> Option<String> {
    if matches!(
        event,
        AgentEvent::AssistantTextDelta { .. } | AgentEvent::ReasoningDelta { .. }
    ) {
        return None;
    }
    log_state_event(event)
        .or_else(|| log_message_event(event))
        .or_else(|| log_tool_event(event))
        .or_else(|| log_runtime_event(event))
}

fn log_state_event(event: &AgentEvent) -> Option<String> {
    log_state_transition(event).or_else(|| log_runtime_state_event(event))
}

fn log_state_transition(event: &AgentEvent) -> Option<String> {
    match event {
        AgentEvent::StateChanged { from, to } => Some(format!("state {from:?} → {to:?}")),
        AgentEvent::RunStarted { run_id } => Some(format!("run started {run_id:?}")),
        AgentEvent::BackendRequestStarted { request_id } => {
            Some(format!("backend request started {request_id:?}"))
        }
        AgentEvent::UsageUpdated { usage } => Some(format!("usage updated: {usage:?}")),
        AgentEvent::Failed { error } => Some(format!("FAILED [{}] {}", error.code, error.message)),
        AgentEvent::Completed { outcome } => Some(format!("completed: {outcome:?}")),
        _ => None,
    }
}

fn log_runtime_state_event(event: &AgentEvent) -> Option<String> {
    match event {
        AgentEvent::ToolCallDenied {
            call_id, reason, ..
        } => Some(format!("tool call denied {call_id:?}: {reason}")),
        AgentEvent::ProfileChanged { from, to } => Some(format!("profile {from} → {to}")),
        AgentEvent::ModelRequestPrepared { .. } => Some("model request prepared".to_owned()),
        AgentEvent::CompletionGateEvaluated {
            attempt,
            passed,
            continuing,
            failed_checks,
        } => Some(completion_gate_summary(
            *attempt,
            *passed,
            *continuing,
            failed_checks,
        )),
        _ => None,
    }
}

fn log_message_event(event: &AgentEvent) -> Option<String> {
    match event {
        AgentEvent::AssistantMessageStarted { message_id } => {
            Some(format!("assistant message started {message_id:?}"))
        }
        AgentEvent::AssistantMessageCompleted { message_id } => {
            Some(format!("assistant message completed {message_id:?}"))
        }
        AgentEvent::PermissionRequested { request } => Some(format!(
            "permission requested for {} ({:?})",
            request.tool_call.name, request.id
        )),
        AgentEvent::ChildAgentSpawned {
            agent_id,
            tool_call_id,
        } => Some(child_spawn_summary(*agent_id, *tool_call_id)),
        AgentEvent::ChildAgentCompleted { agent_id, outcome } => {
            Some(format!("child agent {agent_id:?} completed: {outcome:?}"))
        }
        AgentEvent::BehaviorRuleFired {
            profile,
            rule_id,
            event,
            action,
        } => Some(format!(
            "rule {rule_id} ({profile}) fired on {event}: {action}"
        )),
        AgentEvent::ContextInjected {
            source,
            placement,
            chars,
        } => Some(format!(
            "context injected by {source} ({placement}, {chars} chars)"
        )),
        _ => None,
    }
}

fn log_tool_event(event: &AgentEvent) -> Option<String> {
    match event {
        AgentEvent::ToolCallRequested { call } => Some(tool_requested(call)),
        AgentEvent::ToolCallStarted { call_id } => Some(format!("tool call started {call_id:?}")),
        AgentEvent::ToolCallProgress { call_id, progress } => Some(format!(
            "tool call progress {call_id:?}: {} ({:.0}%)",
            progress.status,
            progress.fraction * 100.0
        )),
        AgentEvent::ToolCallCompleted { call_id, result } => Some(format!(
            "tool call {} {call_id:?}: {}",
            tool_result_label(result.has_error),
            result.output_preview
        )),
        _ => None,
    }
}

fn log_runtime_event(event: &AgentEvent) -> Option<String> {
    super::runtime_events::log_runtime_event(event)
}

fn tool_requested(call: &ToolCall) -> String {
    format!("tool call requested: {} {}", call.name, call.arguments)
}

fn tool_result_label(has_error: bool) -> &'static str {
    if has_error {
        "failed"
    } else {
        "completed"
    }
}

fn child_spawn_summary(agent_id: AgentId, tool_call_id: Option<ToolCallId>) -> String {
    match tool_call_id {
        Some(call) => format!("child agent spawned {agent_id:?} by tool call {call:?}"),
        None => format!("child agent spawned {agent_id:?}"),
    }
}

fn completion_gate_summary(
    attempt: u32,
    passed: bool,
    continuing: bool,
    failed_checks: &[String],
) -> String {
    let result = if passed { "passed" } else { "failed" };
    let continuation = if continuing {
        format!("(sent back: {})", failed_checks.join(", "))
    } else {
        String::new()
    };
    format!("completion gate #{attempt}: {result} {continuation}")
}
