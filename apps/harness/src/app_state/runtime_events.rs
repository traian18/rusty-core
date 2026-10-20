use harness_protocol::events::AgentEvent;

pub(super) fn log_runtime_event(event: &AgentEvent) -> Option<String> {
    match event {
        AgentEvent::ToolCallDenied {
            call_id, reason, ..
        } => Some(format!("tool call denied {call_id:?}: {reason}")),
        AgentEvent::ProfileChanged { from, to } => Some(format!("profile {from} → {to}")),
        AgentEvent::CompletionGateEvaluated {
            attempt,
            passed,
            continuing,
            failed_checks,
        } => Some(format!(
            "completion gate #{attempt}: {} {}",
            if *passed { "passed" } else { "failed" },
            if *continuing {
                format!("(sent back: {})", failed_checks.join(", "))
            } else {
                String::new()
            }
        )),
        _ => None,
    }
}

pub(super) fn completion_gate_notice(continuing: bool, failed_checks: &[String]) -> String {
    let checks = failed_checks.join(", ");
    if continuing {
        format!("Completion rejected ({checks}); continuing")
    } else {
        format!("Completion gate not passed ({checks})")
    }
}
