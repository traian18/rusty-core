use harness_protocol::events::AgentEvent;

#[derive(Debug)]
pub(super) enum EventAction {
    StateChanged(String),
    Notice(String),
    UsageUpdated(harness_protocol::usage::AgentUsageSnapshot),
    Failed { code: String, message: String },
    Ignore,
}

pub(super) fn fold_event(event: AgentEvent) -> EventAction {
    match event {
        AgentEvent::StateChanged { to, .. } => EventAction::StateChanged(format!("{to:?}")),
        AgentEvent::RunStarted { .. } => EventAction::Notice("Run started".to_owned()),
        AgentEvent::UsageUpdated { usage } => EventAction::UsageUpdated(usage),
        AgentEvent::Failed { error } => EventAction::Failed {
            code: error.code,
            message: error.message,
        },
        AgentEvent::Completed { outcome } => {
            EventAction::Notice(format!("Run completed: {outcome:?}"))
        }
        AgentEvent::ToolCallDenied { reason, .. } => {
            EventAction::Notice(format!("Tool call denied: {reason}"))
        }
        AgentEvent::ProfileChanged { to, .. } => EventAction::Notice(format!("Profile: {to}")),
        AgentEvent::CompletionGateEvaluated {
            passed: false,
            continuing,
            failed_checks,
            ..
        } => EventAction::Notice(super::runtime_events::completion_gate_notice(
            continuing,
            &failed_checks,
        )),
        _ => EventAction::Ignore,
    }
}
