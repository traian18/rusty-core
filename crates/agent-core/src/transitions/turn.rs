//! Preparing one turn: the plan, the model request and execution settings.

use rusty_protocol::backend::ExecutionRequest;
use rusty_protocol::commands::AgentStatus;
use rusty_protocol::effects::AgentEffect;
use rusty_protocol::ids::RunId;
use rusty_protocol::messages::{AgentMessage, ContentBlock, MessageRole};

use crate::agent::Agent;
use crate::behavior::{system_reminder, RuleEvent, TurnPlan};

/// Append harness text to the end of an outgoing request without touching
/// the canonical transcript. Every provider sends a tool result's
/// `output_preview`, but some drop text blocks inside tool messages, so a
/// request ending in tool results carries the text on the last result.
pub(super) fn append_to_request_tail(messages: &mut [AgentMessage], text: &str) {
    let Some(last) = messages.last_mut() else {
        return;
    };
    if last.role == MessageRole::Tool {
        if let Some(ContentBlock::ToolResult { result, .. }) = last
            .content
            .iter_mut()
            .rev()
            .find(|block| matches!(block, ContentBlock::ToolResult { .. }))
        {
            result.output_preview.push_str("\n\n");
            result.output_preview.push_str(text);
            return;
        }
    }
    last.content.push(ContentBlock::Text {
        text: text.to_string(),
    });
}

impl Agent {
    /// Count a new model request against the profile's limits. Returns the
    /// failure effects when the model kept calling tools after its final turn.
    ///
    /// On success, returns the effects of `BeforeModelRequest` rules.
    pub(super) fn begin_turn(&mut self) -> Result<Vec<AgentEffect>, Vec<AgentEffect>> {
        let mut effects = Vec::new();
        if let Some((profile, library)) = self.state.pending_profile.take() {
            effects.extend(self.apply_switch(profile, library));
            if self.state.status == AgentStatus::Failed {
                return Err(effects);
            }
        }
        if let Some(from) = self.state.behavior.entered_from.take() {
            match self.enter_profile(from) {
                Ok(entered) => effects.extend(entered),
                Err(failure) => {
                    effects.extend(failure);
                    return Err(effects);
                }
            }
        }
        if let TurnPlan::Exceeded = self.state.behavior.begin_turn() {
            return Err(self.fail(
                "BEHAVIOR_LIMIT_EXCEEDED",
                format!(
                    "the model requested tools after its final turn under profile {}",
                    self.state.behavior.reference()
                ),
            ));
        }
        let outcome = self
            .state
            .behavior
            .evaluate(RuleEvent::BeforeModelRequest, None);
        effects.extend(self.apply_rule_outcome(&outcome, None));
        if let Some(stop) = &outcome.stop {
            effects.extend(self.behavior_stop(stop));
            return Err(effects);
        }
        Ok(effects)
    }

    /// Applies a partial `ExecutionParams` update over the session-level
    /// default. Pure state mutation — no effects, no run-lifecycle impact.
    /// Takes effect starting with the next run this agent starts.
    pub(super) fn configure_execution(
        &mut self,
        params: rusty_protocol::backend::ExecutionParams,
    ) -> Vec<AgentEffect> {
        self.state.execution_params = self.state.execution_params.merge_over(&params);
        Vec::new()
    }

    /// Build the request for the next model call. The behavior profile
    /// shapes it here — tools, descriptions, parameters, system prompt, and
    /// the final-turn prompt — without touching the canonical transcript.
    pub(super) fn execution_request(&mut self, run_id: RunId) -> ExecutionRequest {
        self.build_request(run_id, false)
    }

    /// `reissue` rebuilds the request for a turn already counted (resume
    /// after pause): it repeats that turn's injected context instead of
    /// consuming new pending context.
    pub(super) fn build_request(&mut self, run_id: RunId, reissue: bool) -> ExecutionRequest {
        self.state.backend_in_flight = true;
        let profile = self.state.behavior.profile.clone();
        let final_turn = self.state.behavior.run.final_turn;
        let tools = if final_turn {
            Vec::new()
        } else {
            self.capabilities
                .tools
                .enabled_descriptors()
                .into_iter()
                .filter(|descriptor| profile.allows_tool(&descriptor.name))
                .map(|descriptor| {
                    let mut descriptor = descriptor.clone();
                    descriptor.description =
                        profile.tool_description(&descriptor.name, &descriptor.description);
                    descriptor
                })
                .collect()
        };
        // Remember what the agent was really offered, for `tool_offered`. A
        // forced final turn offers nothing and says nothing about the toolset.
        if !final_turn {
            let offered = tools.iter().map(|tool| tool.name.clone()).collect();
            self.state.behavior.note_offered(offered);
        }
        let params = profile.execution_params(&self.state.execution_params);
        let extended_thinking = params.extended_thinking.unwrap_or(false);
        let system_prompt =
            profile.system_prompt(&self.state.system_prompt, params.model.as_deref());
        let mut messages = self.state.messages.clone();
        let run = &mut self.state.behavior.run;
        if !reissue {
            run.last_request = std::mem::take(&mut run.pending_request);
        }
        for text in run.last_request.clone() {
            append_to_request_tail(&mut messages, &text);
        }
        if final_turn {
            if let Some(prompt) = &profile.profile.limits.final_turn_prompt {
                append_to_request_tail(
                    &mut messages,
                    &system_reminder(&format!("profile:{}", profile.reference()), prompt),
                );
            }
        }
        ExecutionRequest {
            request_id: self.next_request_id(),
            run_id,
            system_prompt,
            messages,
            tools,
            extended_thinking,
            params,
        }
    }
}
