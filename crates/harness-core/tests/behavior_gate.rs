//! The completion gate in the deterministic state machine: declarative
//! checks, the evaluator round-trip, continuation with feedback,
//! exhaustion, and cancellation.

use std::collections::HashMap;

use harness_core::agent::Agent;
use harness_core::capabilities::{AgentCapabilities, WorkspaceCapabilities};
use harness_protocol::backend::{
    BackendBinding, BackendCapabilities, BackendDescriptor, BackendReference, ExecutionEvent,
    ExecutionResult,
};
use harness_protocol::commands::{
    AgentCommand, AgentStatus, CheckVerdict, UserInput, VerdictOutcome,
};
use harness_protocol::effects::{AgentEffect, CompletionEvaluationRequest};
use harness_protocol::events::{AgentEvent, AgentOutcome};
use harness_protocol::ids::{
    AgentId, BackendId, ConfigurationId, IntegrationId, RequestId, RunId, SessionId, ToolCallId,
    ToolId,
};
use harness_protocol::messages::{ContentBlock, MessageRole};
use harness_protocol::tools::{
    AgentToolset, PermissionMode, ToolCall, ToolCapability, ToolDescriptor, ToolPolicy, ToolResult,
};
use harness_protocol::usage::{AgentBudget, Cost, ModelUsage};
use serde_json::{json, Value};

fn agent(gate: Value, limits: Value) -> Agent {
    agent_with_tools(&["fs.edit", "run_tests"], gate, limits)
}

/// An agent offered exactly `names` as tools.
fn agent_with_tools(names: &[&str], gate: Value, limits: Value) -> Agent {
    let tools = names
        .iter()
        .copied()
        .map(|name| {
            let id = ToolId::new();
            (
                id,
                ToolCapability {
                    descriptor: ToolDescriptor {
                        id,
                        name: name.into(),
                        description: name.into(),
                        input_schema: json!({"type": "object"}),
                    },
                    policy: ToolPolicy {
                        permission: PermissionMode::Allow,
                        enabled: true,
                    },
                    delegatable: true,
                },
            )
        })
        .collect::<HashMap<_, _>>();
    let mut agent = Agent::new(
        AgentId::new(),
        SessionId::new(),
        None,
        0,
        String::new(),
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
            tools: AgentToolset { tools },
            can_spawn_agents: false,
            max_child_depth: None,
            workspace: WorkspaceCapabilities {
                can_read: true,
                can_write: true,
                can_search: false,
            },
            backend: BackendCapabilities::default(),
        },
        AgentBudget::default(),
    );
    agent.apply(AgentCommand::SetBehaviorProfile {
        library: Vec::new(),
        allow_commands: false,
        profile: json!({
            "schema_version": 1, "id": "gated", "revision": 1, "name": "Gated",
            "completion_gate": gate,
            "limits": limits
        }),
    });
    agent
}

fn start(agent: &mut Agent) -> RunId {
    let effects = agent.apply(AgentCommand::StartRun {
        input: UserInput {
            text: "fix the bug".into(),
            attachments: vec![],
        },
    });
    effects
        .iter()
        .find_map(|effect| match effect {
            AgentEffect::ExecuteBackend { request } => Some(request.run_id),
            _ => None,
        })
        .expect("first request")
}

/// The model answers in text and ends its turn (a proposed final answer).
fn answer(agent: &mut Agent, run_id: RunId, text: &str) -> Vec<AgentEffect> {
    let request_id = RequestId::new();
    agent.apply(AgentCommand::BackendEvent {
        run_id,
        event: ExecutionEvent::TextDelta {
            request_id,
            delta: text.into(),
        },
    });
    agent.apply(AgentCommand::BackendEvent {
        run_id,
        event: ExecutionEvent::Completed {
            request_id,
            result: ExecutionResult {
                request_id,
                usage: ModelUsage::default(),
                cost: Cost::default(),
                finish_reason: "end_turn".into(),
            },
        },
    })
}

/// The model calls `tool`, the tool succeeds, and the loop continues.
fn use_tool(agent: &mut Agent, run_id: RunId, tool: &str) -> Vec<AgentEffect> {
    use_tool_result(agent, run_id, tool, false)
}

/// The model calls `tool` and its result is an error when `is_error`.
fn use_tool_result(
    agent: &mut Agent,
    run_id: RunId,
    tool: &str,
    is_error: bool,
) -> Vec<AgentEffect> {
    let call = ToolCall {
        id: ToolCallId::new(),
        name: tool.into(),
        arguments: json!({}),
    };
    let call_id = call.id;
    let request_id = RequestId::new();
    agent.apply(AgentCommand::BackendEvent {
        run_id,
        event: ExecutionEvent::ToolCallRequested { request_id, call },
    });
    agent.apply(AgentCommand::BackendEvent {
        run_id,
        event: ExecutionEvent::Completed {
            request_id,
            result: ExecutionResult {
                request_id,
                usage: ModelUsage::default(),
                cost: Cost::default(),
                finish_reason: "tool_use".into(),
            },
        },
    });
    agent.apply(AgentCommand::ToolCompleted {
        call_id,
        result: ToolResult {
            call_id,
            output: json!("ok"),
            is_error,
        },
    })
}

fn gate_event(effects: &[AgentEffect]) -> Option<(bool, bool, Vec<String>)> {
    effects.iter().find_map(|effect| match effect {
        AgentEffect::Emit {
            event:
                AgentEvent::CompletionGateEvaluated {
                    passed,
                    continuing,
                    failed_checks,
                    ..
                },
        } => Some((*passed, *continuing, failed_checks.clone())),
        _ => None,
    })
}

fn completed(effects: &[AgentEffect]) -> bool {
    effects.iter().any(|effect| {
        matches!(
            effect,
            AgentEffect::Emit {
                event: AgentEvent::Completed {
                    outcome: AgentOutcome::Success
                }
            }
        )
    })
}

fn continues(effects: &[AgentEffect]) -> bool {
    effects
        .iter()
        .any(|effect| matches!(effect, AgentEffect::ExecuteBackend { .. }))
}

fn evaluation(effects: &[AgentEffect]) -> Option<CompletionEvaluationRequest> {
    effects.iter().find_map(|effect| match effect {
        AgentEffect::EvaluateCompletion { request } => Some(request.clone()),
        _ => None,
    })
}

fn last_user_text(agent: &Agent) -> String {
    let message = agent
        .state
        .messages
        .iter()
        .rev()
        .find(|message| message.role == MessageRole::User)
        .unwrap();
    match &message.content[0] {
        ContentBlock::Text { text } => text.clone(),
        other => panic!("unexpected {other:?}"),
    }
}

fn verdict(id: &str, outcome: VerdictOutcome) -> CheckVerdict {
    CheckVerdict {
        id: id.into(),
        outcome,
    }
}

fn tested_gate() -> Value {
    json!({
        "checks": [{
            "id": "tested",
            // "Every edit is followed by tests": not (edited, then zero test
            // runs). Holds when nothing was edited.
            "require": { "not": { "since_last_call": { "of": "fs.edit", "called": "run_tests", "eq": 0 } } },
            "feedback": "You changed code after the last test run. Run run_tests."
        }]
    })
}

#[test]
fn a_failing_requirement_sends_feedback_back_and_the_loop_continues() {
    let mut agent = agent(tested_gate(), json!({}));
    let run_id = start(&mut agent);
    use_tool(&mut agent, run_id, "fs.edit");

    let effects = answer(&mut agent, run_id, "Done!");
    assert_eq!(
        gate_event(&effects),
        Some((false, true, vec!["tested".into()]))
    );
    assert!(continues(&effects), "the model is asked again");
    assert!(!completed(&effects));
    assert!(
        evaluation(&effects).is_none(),
        "declarative checks need no I/O"
    );
    let feedback = last_user_text(&agent);
    assert!(feedback.starts_with("<system-reminder source=\"gate:gated@1\">"));
    assert!(feedback.contains("[tested] You changed code after the last test run."));

    use_tool(&mut agent, run_id, "run_tests");
    let effects = answer(&mut agent, run_id, "Done, tests pass.");
    assert_eq!(gate_event(&effects), Some((true, false, vec![])));
    assert!(completed(&effects));
    assert_eq!(agent.state.status, AgentStatus::Idle);
}

#[test]
fn a_requirement_that_does_not_apply_passes() {
    let mut agent = agent(tested_gate(), json!({}));
    let run_id = start(&mut agent);
    let effects = answer(&mut agent, run_id, "No code changes were needed.");
    assert_eq!(gate_event(&effects), Some((true, false, vec![])));
    assert!(completed(&effects));
}

fn evaluator_gate(extra: Value) -> Value {
    let mut check = json!({
        "id": "judge",
        "evaluator": { "type": "model", "instructions": "Is the bug fixed?", "transcript_messages": 2 }
    });
    for (key, value) in extra.as_object().unwrap() {
        check[key] = value.clone();
    }
    json!({ "checks": [check], "max_continuations": 1 })
}

#[test]
fn evaluators_run_in_the_runtime_and_their_verdict_decides() {
    let mut agent = agent(evaluator_gate(json!({})), json!({}));
    let run_id = start(&mut agent);

    let effects = answer(&mut agent, run_id, "Fixed it.");
    assert_eq!(agent.state.status, AgentStatus::Verifying);
    let request = evaluation(&effects).expect("evaluation requested");
    assert_eq!(request.run_id, run_id);
    assert_eq!(request.attempt, 1);
    assert_eq!(request.final_response, "Fixed it.");
    assert_eq!(request.transcript.len(), 2);
    assert_eq!(request.checks[0].id, "judge");
    assert_eq!(request.checks[0].evaluator["type"], "model");
    assert!(!completed(&effects));

    let effects = agent.apply(AgentCommand::CompletionEvaluated {
        usage: Vec::new(),
        run_id,
        attempt: 1,
        verdicts: vec![verdict("judge", VerdictOutcome::Pass)],
    });
    assert!(completed(&effects));
    assert_eq!(gate_event(&effects), Some((true, false, vec![])));
}

#[test]
fn evaluator_feedback_is_sent_back_and_stale_verdicts_are_ignored() {
    let mut agent = agent(evaluator_gate(json!({})), json!({}));
    let run_id = start(&mut agent);
    answer(&mut agent, run_id, "Fixed it.");

    assert!(agent
        .apply(AgentCommand::CompletionEvaluated {
            usage: Vec::new(),
            run_id,
            attempt: 7,
            verdicts: vec![verdict("judge", VerdictOutcome::Pass)],
        })
        .is_empty());
    assert!(agent
        .apply(AgentCommand::CompletionEvaluated {
            usage: Vec::new(),
            run_id: RunId::new(),
            attempt: 1,
            verdicts: vec![verdict("judge", VerdictOutcome::Pass)],
        })
        .is_empty());
    assert_eq!(agent.state.status, AgentStatus::Verifying);

    let effects = agent.apply(AgentCommand::CompletionEvaluated {
        usage: Vec::new(),
        run_id,
        attempt: 1,
        verdicts: vec![verdict(
            "judge",
            VerdictOutcome::Fail {
                feedback: "The null case is not handled.".into(),
            },
        )],
    });
    assert!(continues(&effects));
    assert!(last_user_text(&agent).contains("[judge] The null case is not handled."));
    assert_eq!(agent.state.status, AgentStatus::WaitingForBackend);

    // The next proposal is attempt 2; a late verdict for attempt 1 is stale.
    let effects = answer(&mut agent, run_id, "Handled null too.");
    assert_eq!(evaluation(&effects).unwrap().attempt, 2);
    assert!(agent
        .apply(AgentCommand::CompletionEvaluated {
            usage: Vec::new(),
            run_id,
            attempt: 1,
            verdicts: vec![verdict("judge", VerdictOutcome::Pass)],
        })
        .is_empty());
}

#[test]
fn evaluator_errors_follow_the_error_policy() {
    let error = || {
        vec![verdict(
            "judge",
            VerdictOutcome::Error {
                message: "backend down".into(),
            },
        )]
    };

    let mut strict = agent(evaluator_gate(json!({})), json!({}));
    let run_id = start(&mut strict);
    answer(&mut strict, run_id, "Fixed.");
    let effects = strict.apply(AgentCommand::CompletionEvaluated {
        usage: Vec::new(),
        run_id,
        attempt: 1,
        verdicts: error(),
    });
    assert!(continues(&effects));
    assert!(last_user_text(&strict).contains("could not be evaluated: backend down"));

    let mut lenient = agent(evaluator_gate(json!({ "error_policy": "pass" })), json!({}));
    let run_id = start(&mut lenient);
    answer(&mut lenient, run_id, "Fixed.");
    let effects = lenient.apply(AgentCommand::CompletionEvaluated {
        usage: Vec::new(),
        run_id,
        attempt: 1,
        verdicts: error(),
    });
    assert!(completed(&effects));
}

#[test]
fn an_exhausted_gate_accepts_by_default_and_can_be_told_to_fail() {
    let reject = |agent: &mut Agent, run_id, attempt| {
        agent.apply(AgentCommand::CompletionEvaluated {
            usage: Vec::new(),
            run_id,
            attempt,
            verdicts: vec![verdict(
                "judge",
                VerdictOutcome::Fail {
                    feedback: "no".into(),
                },
            )],
        })
    };

    let mut accepting = agent(evaluator_gate(json!({})), json!({}));
    let run_id = start(&mut accepting);
    answer(&mut accepting, run_id, "one");
    assert!(continues(&reject(&mut accepting, run_id, 1)));
    answer(&mut accepting, run_id, "two");
    let effects = reject(&mut accepting, run_id, 2);
    assert_eq!(
        gate_event(&effects),
        Some((false, false, vec!["judge".into()]))
    );
    assert!(completed(&effects), "accepted, reported as not passed");

    let mut gate = evaluator_gate(json!({}));
    gate["on_exhausted"] = json!("fail");
    let mut failing = agent(gate, json!({}));
    let run_id = start(&mut failing);
    answer(&mut failing, run_id, "one");
    reject(&mut failing, run_id, 1);
    answer(&mut failing, run_id, "two");
    let effects = reject(&mut failing, run_id, 2);
    assert_eq!(failing.state.status, AgentStatus::Failed);
    assert!(effects.iter().any(|effect| matches!(effect,
        AgentEffect::Emit { event: AgentEvent::Failed { error } } if error.code == "GATE_EXHAUSTED")));
}

#[test]
fn a_rejection_on_the_final_turn_cannot_continue() {
    let mut agent = agent(tested_gate(), json!({ "max_turns": 2 }));
    let run_id = start(&mut agent);
    use_tool(&mut agent, run_id, "fs.edit");
    let effects = answer(&mut agent, run_id, "Done");
    assert_eq!(
        gate_event(&effects),
        Some((false, false, vec!["tested".into()])),
        "no turn is left to address the feedback"
    );
    assert!(completed(&effects));
}

/// "Nothing was edited, or a check passed after the last edit."
fn passing_check_gate() -> Value {
    json!({
        "checks": [{
            "id": "check-passed",
            "require": { "any": [
                { "calls": { "tool": "fs.edit", "eq": 0 } },
                { "since_last_call": { "of": "fs.edit", "called": "run_check", "outcome": "succeeded", "gte": 1 } }
            ] },
            "feedback": "No check has passed since your last edit. Run run_check."
        }],
        "max_continuations": 6
    })
}

#[test]
fn a_gate_can_require_a_passing_check_since_the_last_edit() {
    let mut agent = agent_with_tools(&["fs.edit", "run_check"], passing_check_gate(), json!({}));
    let run_id = start(&mut agent);
    use_tool(&mut agent, run_id, "fs.edit");
    use_tool_result(&mut agent, run_id, "run_check", true);

    let effects = answer(&mut agent, run_id, "Done");
    assert_eq!(
        gate_event(&effects),
        Some((false, true, vec!["check-passed".into()])),
        "a check that ran and failed is not a passing check"
    );
    assert!(continues(&effects));
    assert!(
        last_user_text(&agent).contains("[check-passed] No check has passed since your last edit.")
    );

    use_tool_result(&mut agent, run_id, "run_check", false);
    let effects = answer(&mut agent, run_id, "Done, the check passes.");
    assert_eq!(gate_event(&effects), Some((true, false, vec![])));
    assert!(completed(&effects));
}

#[test]
fn an_edit_after_a_passing_check_needs_another_passing_check() {
    let mut agent = agent_with_tools(&["fs.edit", "run_check"], passing_check_gate(), json!({}));
    let run_id = start(&mut agent);
    use_tool(&mut agent, run_id, "fs.edit");
    use_tool_result(&mut agent, run_id, "run_check", false);
    use_tool(&mut agent, run_id, "fs.edit");

    let effects = answer(&mut agent, run_id, "Done");
    assert_eq!(
        gate_event(&effects),
        Some((false, true, vec!["check-passed".into()])),
        "the pass came before the last edit"
    );
    use_tool_result(&mut agent, run_id, "run_check", false);
    assert!(completed(&answer(&mut agent, run_id, "Done")));
}

/// Nothing edited; or no way to run a check was offered; or a check ran after the last edit.
fn check_ran_or_impossible_gate() -> Value {
    json!({
        "checks": [{
            "id": "checked",
            "require": { "any": [
                { "calls": { "tool": "fs.edit", "eq": 0 } },
                { "not": { "tool_offered": "run_check" } },
                { "since_last_call": { "of": "fs.edit", "called": "run_check", "gte": 1 } }
            ] },
            "feedback": "Run run_check."
        }]
    })
}

#[test]
fn a_gate_does_not_demand_a_tool_the_agent_was_never_offered() {
    let mut without = agent_with_tools(&["fs.edit"], check_ran_or_impossible_gate(), json!({}));
    let run_id = start(&mut without);
    use_tool(&mut without, run_id, "fs.edit");
    let effects = answer(&mut without, run_id, "Done");
    assert_eq!(
        gate_event(&effects),
        Some((true, false, vec![])),
        "there was no way to run a check, so none is required"
    );
    assert!(completed(&effects));

    let mut with = agent_with_tools(
        &["fs.edit", "run_check"],
        check_ran_or_impossible_gate(),
        json!({}),
    );
    let run_id = start(&mut with);
    use_tool(&mut with, run_id, "fs.edit");
    let effects = answer(&mut with, run_id, "Done");
    assert_eq!(
        gate_event(&effects),
        Some((false, true, vec!["checked".into()]))
    );
    // A check that ran and failed still counts as having run one: this gate asks for an attempt.
    use_tool_result(&mut with, run_id, "run_check", true);
    let effects = answer(&mut with, run_id, "The check fails; here is why.");
    assert_eq!(gate_event(&effects), Some((true, false, vec![])));
}

#[test]
fn a_forced_final_turn_does_not_make_the_gate_think_no_tools_were_offered() {
    // Turn 2 is the final turn and offers no tools, but run_check was offered on turn 1.
    let mut agent = agent_with_tools(
        &["fs.edit", "run_check"],
        check_ran_or_impossible_gate(),
        json!({ "max_turns": 2 }),
    );
    let run_id = start(&mut agent);
    use_tool(&mut agent, run_id, "fs.edit");
    let effects = answer(&mut agent, run_id, "Done");
    assert_eq!(
        gate_event(&effects),
        Some((false, false, vec!["checked".into()])),
        "run_check was available, so the missing check still counts against the run"
    );
}

#[test]
fn cancel_stops_verification_and_pause_waits_for_it() {
    let mut agent = agent(evaluator_gate(json!({})), json!({}));
    let run_id = start(&mut agent);
    answer(&mut agent, run_id, "Fixed.");

    assert!(agent.apply(AgentCommand::Pause).is_empty());
    assert_eq!(agent.state.status, AgentStatus::Verifying);

    let effects = agent.apply(AgentCommand::Cancel);
    assert!(effects.iter().any(
        |effect| matches!(effect, AgentEffect::CancelEvaluation { run_id: id } if *id == run_id)
    ));
    assert_eq!(agent.state.status, AgentStatus::Cancelled);
    assert!(agent
        .apply(AgentCommand::CompletionEvaluated {
            usage: Vec::new(),
            run_id,
            attempt: 1,
            verdicts: vec![verdict("judge", VerdictOutcome::Pass)],
        })
        .is_empty());
}

fn finish_gate_flag(effects: &[AgentEffect]) -> Option<Option<bool>> {
    effects.iter().find_map(|effect| match effect {
        AgentEffect::FinishRun { result } => Some(result.gate_passed),
        _ => None,
    })
}

#[test]
fn the_run_result_reports_whether_the_gate_passed() {
    let mut passing = agent(tested_gate(), json!({}));
    let run_id = start(&mut passing);
    assert_eq!(
        finish_gate_flag(&answer(&mut passing, run_id, "ok")),
        Some(Some(true))
    );

    let mut unpassed = agent(evaluator_gate(json!({})), json!({}));
    unpassed.apply(AgentCommand::SetBehaviorProfile {
        library: Vec::new(),
        allow_commands: false,
        profile: json!({
            "schema_version": 1, "id": "gated", "revision": 2, "name": "Gated",
            "completion_gate": {
                "checks": [{ "id": "never", "require": { "turn": { "gte": 99 } }, "feedback": "no" }],
                "max_continuations": 0
            }
        }),
    });
    let run_id = start(&mut unpassed);
    assert_eq!(
        finish_gate_flag(&answer(&mut unpassed, run_id, "done")),
        Some(Some(false))
    );

    let mut ungated = agent(json!(null), json!({}));
    let run_id = start(&mut ungated);
    assert_eq!(
        finish_gate_flag(&answer(&mut ungated, run_id, "done")),
        Some(None)
    );
}

#[test]
fn evaluator_model_usage_is_counted_even_for_stale_verdicts() {
    let record = || harness_protocol::usage::UsageRecord {
        model_usage: ModelUsage {
            total_tokens: harness_protocol::usage::UsageValue::new(Some(50)),
            ..Default::default()
        },
        cost: Cost::default(),
        tool_usage: None,
    };
    let mut agent = agent(evaluator_gate(json!({})), json!({}));
    let run_id = start(&mut agent);
    answer(&mut agent, run_id, "Fixed.");
    let before = agent.usage.records.len();

    let effects = agent.apply(AgentCommand::CompletionEvaluated {
        run_id,
        attempt: 9,
        verdicts: vec![],
        usage: vec![record()],
    });
    assert_eq!(agent.usage.records.len(), before + 1);
    assert!(effects.iter().any(|effect| matches!(effect,
        AgentEffect::Emit { event: AgentEvent::UsageUpdated { usage } }
            if usage.metrics.total_requests == (before + 1) as u64)));
    assert_eq!(
        agent.state.status,
        AgentStatus::Verifying,
        "the stale verdict is ignored"
    );

    agent.apply(AgentCommand::CompletionEvaluated {
        run_id,
        attempt: 1,
        verdicts: vec![verdict("judge", VerdictOutcome::Pass)],
        usage: vec![record()],
    });
    assert_eq!(agent.usage.records.len(), before + 2);
}
