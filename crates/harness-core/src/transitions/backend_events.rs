//! Reducers for backend-originated execution events.
//!
//! This child module shares the parent transition module's private `Agent`
//! implementation details while keeping event-specific behavior out of the
//! main command reducer.

use super::*;
use harness_protocol::backend::ExecutionResult;

pub(super) fn text_delta(agent: &mut Agent, delta: String) -> Vec<AgentEffect> {
    let from = agent.state.status;
    agent.state.status = AgentStatus::Streaming;
    let message_id = agent.append_assistant_text(&delta);
    let mut effects = Vec::new();
    if from != AgentStatus::Streaming {
        effects.push(Agent::state_changed(from, AgentStatus::Streaming));
    }
    effects.push(AgentEffect::Emit {
        event: AgentEvent::AssistantTextDelta { message_id, delta },
    });
    effects
}

pub(super) fn reasoning_delta(agent: &mut Agent, delta: String) -> Vec<AgentEffect> {
    let message_id = agent.assistant_message_id();
    vec![AgentEffect::Emit {
        event: AgentEvent::ReasoningDelta { message_id, delta },
    }]
}

pub(super) fn tool_started(agent: &mut Agent, call: ToolCall) -> Vec<AgentEffect> {
    let from = agent.state.status;
    agent.state.status = AgentStatus::Executing;
    let call_id = call.id;
    agent.push_tool_use(call.clone());
    let mut effects = Vec::new();
    if from != AgentStatus::Executing {
        effects.push(Agent::state_changed(from, AgentStatus::Executing));
    }
    effects.push(AgentEffect::Emit {
        event: AgentEvent::ToolCallRequested { call },
    });
    effects.push(AgentEffect::Emit {
        event: AgentEvent::ToolCallStarted { call_id },
    });
    effects
}

pub(super) fn tool_completed(
    agent: &mut Agent,
    call_id: ToolCallId,
    result: ToolResultSummary,
) -> Vec<AgentEffect> {
    let from = agent.state.status;
    agent.state.status = AgentStatus::Streaming;
    agent.usage.tool_calls = agent.usage.tool_calls.saturating_add(1);
    let message_id = agent.next_message_id();
    let created_at = agent.next_timestamp();
    agent.state.messages.push(AgentMessage {
        id: message_id,
        role: MessageRole::Tool,
        content: vec![ContentBlock::ToolResult {
            call_id,
            result: result.clone(),
        }],
        created_at,
    });
    let mut effects = Vec::new();
    if from != AgentStatus::Streaming {
        effects.push(Agent::state_changed(from, AgentStatus::Streaming));
    }
    effects.push(AgentEffect::Emit {
        event: AgentEvent::ToolCallCompleted { call_id, result },
    });
    effects
}

pub(super) fn usage_update(agent: &mut Agent, usage: ModelUsage) -> Vec<AgentEffect> {
    let timestamp = agent.next_timestamp().to_rfc3339();
    vec![AgentEffect::Emit {
        event: AgentEvent::UsageUpdated {
            usage: AgentUsageSnapshot {
                agent_id: agent.id.to_string(),
                model: agent.state.execution_params.model.clone(),
                metrics: AgentUsageMetrics {
                    total_runs: agent.usage.runs,
                    total_requests: agent.usage.records.len() as u64,
                    total_tool_calls: agent.usage.tool_calls,
                    total_tokens: usage.total_tokens,
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                    cache_read_tokens: usage.cache_read_tokens,
                    cache_write_tokens: usage.cache_write_tokens,
                    reasoning_tokens: usage.reasoning_tokens,
                    total_cost: None,
                },
                timestamp,
            },
        },
    }]
}

pub(super) fn completed(
    agent: &mut Agent,
    run_id: RunId,
    result: ExecutionResult,
) -> Vec<AgentEffect> {
    agent.state.backend_in_flight = false;
    agent.discard_trailing_empty_assistant_message();
    let is_tool_turn = result.finish_reason == "tool_use";
    agent.usage.records.push(UsageRecord {
        model_usage: result.usage,
        cost: result.cost,
        tool_usage: None,
    });
    if matches!(
        result.finish_reason.as_str(),
        "max_tokens" | "length" | "max_output_tokens"
    ) {
        return agent.fail(
            "OUTPUT_LIMIT_REACHED",
            format!(
                "The model stopped at its output token limit ({}); the response is incomplete.",
                result.finish_reason
            ),
        );
    }
    if is_tool_turn {
        return agent.continue_after_tools();
    }
    agent.propose_completion(run_id, format!("Run completed: {}", result.finish_reason))
}
