use harness_protocol::backend::{ExecutionEvent, ExecutionRequest};
use harness_protocol::commands::{
    AgentCommand, AgentError, AgentResult, AgentStatus, Attachment, CheckVerdict,
    PermissionDecision, UserInput, VerdictOutcome,
};
use harness_protocol::effects::{
    AgentEffect, CompletionCheckSpec, CompletionEvaluationRequest, PermissionRequest, ToolRequest,
};
use harness_protocol::events::{AgentEvent, AgentOutcome};
use harness_protocol::ids::{
    AgentId, MessageId, PermissionId, RequestId, RunId, Timestamp, ToolCallId,
};
use harness_protocol::messages::{AgentMessage, ContentBlock, MessageRole};
use harness_protocol::tools::{PermissionMode, ToolCall, ToolError, ToolResult, ToolResultSummary};
use harness_protocol::usage::{
    AgentUsageMetrics, AgentUsageSnapshot, AgentUsageSummary, UsageRecord,
};

use crate::agent::Agent;
use crate::agent_state::PendingToolCall;
use crate::behavior::{
    compile, merge_patch, system_reminder, BehaviorProfile, CompiledProfile, EnteredFrom,
    ErrorPolicy, EvaluatorSpec, OnExhausted, Placement, ProfileRef, ProfileRegistry, RuleEvent,
    RuleOutcome, RuleStop, ToolDecision, ToolEvent, TurnPlan, MAX_SWITCHES_PER_RUN,
};
use crate::transcript::validate_transcript;

/// M3: caps how large a single assistant message's assembled text can grow
/// across a run's streamed deltas. Without this, a runaway or adversarial
/// backend stream grows `AgentState.messages` — held for the run's (and,
/// once persisted, the session's) entire lifetime — without bound.
const MAX_ASSISTANT_TEXT_BYTES: usize = 4 * 1024 * 1024;

/// Recent transcript messages handed to `command` gate evaluators.
const COMMAND_TRANSCRIPT_MESSAGES: usize = 20;

/// Appends `delta` to `text`, stopping (with a one-time truncation marker)
/// once `text` would exceed [`MAX_ASSISTANT_TEXT_BYTES`]. Once truncated,
/// further deltas are silently dropped from the assembled message — the
/// live per-delta `AssistantTextDelta` event still carries the full delta
/// (that event stream is bounded separately, by the runtime's broadcast
/// channel capacity), only the durable, run-lifetime-held assembled text is
/// capped here.
const TRUNCATION_MARKER: &str = "\n... (truncated, exceeds assistant message size limit)";

/// M4: caps a single attachment's raw byte size, mirroring the M3 precedent
/// set by `fs.read`'s 10MB cap. Unlike text truncation, silently truncating
/// image bytes would just produce a corrupt, undecodable image — so an
/// oversized attachment fails the run outright (via `Agent::fail`, the same
/// path `validate_transcript` already uses) rather than being silently
/// mangled or unboundedly accepted.
const MAX_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;

fn validate_attachments(attachments: &[Attachment]) -> Result<(), String> {
    for (index, attachment) in attachments.iter().enumerate() {
        if attachment.data.len() > MAX_ATTACHMENT_BYTES {
            return Err(format!(
                "attachment {index} ({} bytes, {}) exceeds the {MAX_ATTACHMENT_BYTES}-byte limit",
                attachment.data.len(),
                attachment.mime_type
            ));
        }
    }
    Ok(())
}

/// Converts one user-supplied [`Attachment`] into a transcript
/// [`ContentBlock`]. Image MIME types become a real `ContentBlock::Image`
/// (forwarded to providers that support it — see
/// `GenericModelBackend::check_capabilities`, which rejects an image-bearing
/// request against a provider that doesn't). Anything else becomes a
/// visible text note rather than being silently dropped: this harness does
/// not yet have a wire format for non-image attachments (e.g. arbitrary
/// files), and the M4 requirement is "no silent loss," not "every
/// attachment kind is fully supported."
fn attachment_to_content_block(attachment: Attachment) -> ContentBlock {
    if attachment.mime_type.starts_with("image/") {
        ContentBlock::Image {
            mime_type: attachment.mime_type,
            data: attachment.data,
        }
    } else {
        ContentBlock::Text {
            text: format!(
                "[attachment: {} bytes, {} — non-image attachments are not yet forwarded as model input]",
                attachment.data.len(),
                attachment.mime_type
            ),
        }
    }
}

fn push_bounded(text: &mut String, delta: &str) {
    if delta.is_empty() {
        return;
    }
    if text.ends_with(TRUNCATION_MARKER) {
        // Already truncated by an earlier delta; further deltas are
        // silently dropped from the assembled message rather than
        // re-appending the marker every time.
        return;
    }
    if text.len() >= MAX_ASSISTANT_TEXT_BYTES {
        text.push_str(TRUNCATION_MARKER);
        return;
    }
    let remaining = MAX_ASSISTANT_TEXT_BYTES - text.len();
    if delta.len() <= remaining {
        text.push_str(delta);
        return;
    }
    // Truncate at a UTF-8 char boundary so we never slice through a
    // multi-byte codepoint.
    let mut cut = remaining;
    while cut > 0 && !delta.is_char_boundary(cut) {
        cut -= 1;
    }
    text.push_str(&delta[..cut]);
    text.push_str(TRUNCATION_MARKER);
}

/// Compile a profile and its switch library, and check that every switch
/// target reachable from the profile resolves within the library.
fn install_bundle(
    profile: serde_json::Value,
    library: Vec<serde_json::Value>,
) -> Option<(
    std::sync::Arc<CompiledProfile>,
    std::sync::Arc<ProfileRegistry>,
)> {
    let profile = compile(serde_json::from_value::<BehaviorProfile>(profile).ok()?).ok()?;
    let documents = library
        .into_iter()
        .map(serde_json::from_value::<BehaviorProfile>)
        .chain(std::iter::once(Ok(profile.profile.clone())))
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let registry = ProfileRegistry::from_library(documents).ok()?;
    let profile = std::sync::Arc::new(profile);
    registry.resolve_closure(&profile).ok()?;
    Some((profile, std::sync::Arc::new(registry)))
}

/// Append harness text to the end of an outgoing request without touching
/// the canonical transcript. Every provider sends a tool result's
/// `output_preview`, but some drop text blocks inside tool messages, so a
/// request ending in tool results carries the text on the last result.
fn append_to_request_tail(messages: &mut [AgentMessage], text: &str) {
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

/// Consecutive timed-out tool calls after which a run fails with `TOOL_TIMEOUT`.
pub const MAX_CONSECUTIVE_TOOL_TIMEOUTS: u32 = 3;

impl Agent {
    pub fn apply(&mut self, command: AgentCommand) -> Vec<AgentEffect> {
        match command {
            AgentCommand::StartRun { input } => self.start_or_queue(input),
            AgentCommand::Steer { input } | AgentCommand::FollowUp { input } => {
                self.start_or_queue(input)
            }
            AgentCommand::StartNextQueuedRun => self.start_next_queued_run(),
            AgentCommand::BackendEvent { run_id, event } => self.backend_event(run_id, event),
            AgentCommand::ToolCompleted { call_id, result } => self.tool_completed(call_id, result),
            AgentCommand::ToolFailed { call_id, error } => self.tool_failed(call_id, error),
            AgentCommand::PermissionResolved { id, decision } => {
                self.permission_resolved(id, decision)
            }
            AgentCommand::SpawnChild { spec } => vec![AgentEffect::SpawnAgent { spec }],
            AgentCommand::ChildSpawned { agent_id, awaiting } => {
                self.child_spawned(agent_id, awaiting)
            }
            AgentCommand::ChildCompleted { agent_id, result } => {
                self.child_completed(agent_id, result)
            }
            AgentCommand::ChildFailed { agent_id, error } => self.child_failed(agent_id, error),
            AgentCommand::Cancel => self.cancel(),
            AgentCommand::Pause => self.pause(),
            AgentCommand::Resume => self.resume(),
            AgentCommand::ConfigureExecution { params } => self.configure_execution(params),
            AgentCommand::SetBehaviorProfile {
                profile,
                library,
                allow_commands,
            } => self.set_behavior_profile(profile, library, allow_commands),
            AgentCommand::CompletionEvaluated {
                run_id,
                attempt,
                verdicts,
                usage,
            } => self.completion_evaluated(run_id, attempt, verdicts, usage),
        }
    }

    /// Install a profile and its switch library. Hosts validate both
    /// against their registry before sending, so an invalid document or an
    /// unresolvable switch target here is a host bug; the command is ignored
    /// rather than half-applied. An idle agent switches at once; during a
    /// run the switch applies before the next model request.
    fn set_behavior_profile(
        &mut self,
        profile: serde_json::Value,
        library: Vec<serde_json::Value>,
        allow_commands: bool,
    ) -> Vec<AgentEffect> {
        self.state.behavior.commands_trusted = allow_commands;
        let Some((compiled, library)) = install_bundle(profile, library) else {
            debug_assert!(false, "SetBehaviorProfile received an invalid bundle");
            return Vec::new();
        };
        if self.state.active_run.is_some() {
            self.state.pending_profile = Some((compiled, library));
            Vec::new()
        } else {
            self.apply_switch(compiled, library)
        }
    }

    /// Make `profile` active. Counters start fresh and the switch is
    /// announced (and `ProfileEntered` rules fire) before the next request.
    fn apply_switch(
        &mut self,
        profile: std::sync::Arc<crate::behavior::CompiledProfile>,
        library: std::sync::Arc<crate::behavior::ProfileRegistry>,
    ) -> Vec<AgentEffect> {
        let from = self.state.behavior.reference();
        if self.state.behavior.profile.content_hash == profile.content_hash {
            self.state.behavior.library = library;
            return Vec::new();
        }
        self.state.behavior = self.state.behavior.switched_to(profile, library);
        let mut effects = vec![AgentEffect::Emit {
            event: AgentEvent::ProfileChanged {
                from: from.to_string(),
                to: self.state.behavior.reference().to_string(),
            },
        }];
        if self.state.active_run.is_some()
            && self.state.behavior.run.switches > MAX_SWITCHES_PER_RUN
        {
            effects.extend(self.fail(
                "BEHAVIOR_SWITCH_LOOP",
                format!(
                    "more than {MAX_SWITCHES_PER_RUN} profile switches in one run (last to {})",
                    self.state.behavior.reference()
                ),
            ));
        }
        effects
    }

    /// Tell the model its mode changed (only when there is earlier
    /// conversation to reinterpret) and fire `ProfileEntered` rules.
    fn enter_profile(&mut self, from: EnteredFrom) -> Result<Vec<AgentEffect>, Vec<AgentEffect>> {
        let behavior = &self.state.behavior;
        let has_history = self
            .state
            .messages
            .iter()
            .any(|message| message.role == MessageRole::Assistant);
        if has_history {
            let from_name = from.name.clone();
            let mut tools: Vec<String> = self
                .capabilities
                .tools
                .enabled_descriptors()
                .into_iter()
                .map(|descriptor| descriptor.name.clone())
                .filter(|name| behavior.profile.allows_tool(name))
                .collect();
            tools.sort();
            let text = format!(
                "Your operating mode changed from {from_name} to {}. Tools available now: {}. \
                 Follow the instructions for this mode from here on.",
                behavior.profile.profile.name,
                if tools.is_empty() {
                    "none".to_string()
                } else {
                    tools.join(", ")
                }
            );
            let source = format!("profile:{}", behavior.reference());
            let reminder = system_reminder(&source, &text);
            let chars = reminder.chars().count() as u64;
            self.state.behavior.run.pending_request.push(reminder);
            let mut effects = vec![AgentEffect::Emit {
                event: AgentEvent::ContextInjected {
                    source,
                    placement: Placement::NextRequest.name().into(),
                    chars,
                },
            }];
            effects.extend(self.entered_rules(&from.profile)?);
            return Ok(effects);
        }
        self.entered_rules(&from.profile)
    }

    fn entered_rules(&mut self, from: &ProfileRef) -> Result<Vec<AgentEffect>, Vec<AgentEffect>> {
        let outcome = self.state.behavior.evaluate_entered(from.id.as_str());
        let mut effects = self.apply_rule_outcome(&outcome, None);
        if let Some(stop) = &outcome.stop {
            effects.extend(self.behavior_stop(stop));
            return Err(effects);
        }
        Ok(effects)
    }

    /// Record a rule evaluation: emit what fired, and route injected
    /// context to where its placement says. `with_result` text is held for
    /// `call_id` until its result is recorded.
    fn apply_rule_outcome(
        &mut self,
        outcome: &RuleOutcome,
        call_id: Option<ToolCallId>,
    ) -> Vec<AgentEffect> {
        let profile = self.state.behavior.reference().to_string();
        let mut effects: Vec<AgentEffect> = outcome
            .fired
            .iter()
            .map(|fired| AgentEffect::Emit {
                event: AgentEvent::BehaviorRuleFired {
                    profile: profile.clone(),
                    rule_id: fired.rule_id.clone(),
                    event: fired.event.name().into(),
                    action: fired.action.into(),
                },
            })
            .collect();
        for injection in &outcome.injections {
            match (injection.placement, call_id) {
                (Placement::WithResult, Some(call_id)) => self
                    .state
                    .behavior
                    .run
                    .pending_results
                    .push((call_id, injection.text.clone())),
                (Placement::Persistent, _) => {
                    if let Some(message) = self
                        .state
                        .messages
                        .last_mut()
                        .filter(|message| message.role == MessageRole::User)
                    {
                        message.content.push(ContentBlock::Text {
                            text: injection.text.clone(),
                        });
                    }
                }
                _ => self
                    .state
                    .behavior
                    .run
                    .pending_request
                    .push(injection.text.clone()),
            }
            effects.push(AgentEffect::Emit {
                event: AgentEvent::ContextInjected {
                    source: format!("profile:{profile} rule:{}", injection.rule_id),
                    placement: injection.placement.name().into(),
                    chars: injection.text.chars().count() as u64,
                },
            });
        }
        if let Some((_, target)) = &outcome.switch {
            match self.state.behavior.resolve_switch(target) {
                Some(next) => {
                    let library = self.state.behavior.library.clone();
                    self.state.pending_profile = Some((next, library));
                }
                // Installed bundles are closed over their switch targets.
                None => debug_assert!(false, "switch target {target} missing from the library"),
            }
        }
        effects
    }

    fn behavior_stop(&mut self, stop: &RuleStop) -> Vec<AgentEffect> {
        self.fail(
            "BEHAVIOR_STOP",
            format!("stopped by rule {}: {}", stop.rule_id, stop.reason),
        )
    }

    /// Count a new model request against the profile's limits. Returns the
    /// failure effects when the model kept calling tools after its final turn.
    ///
    /// On success, returns the effects of `BeforeModelRequest` rules.
    fn begin_turn(&mut self) -> Result<Vec<AgentEffect>, Vec<AgentEffect>> {
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
    fn configure_execution(
        &mut self,
        params: harness_protocol::backend::ExecutionParams,
    ) -> Vec<AgentEffect> {
        self.state.execution_params = self.state.execution_params.merge_over(&params);
        Vec::new()
    }

    fn take_sequence(&mut self) -> u64 {
        let current = self.state.transition_sequence;
        self.state.transition_sequence = current.saturating_add(1);
        current
    }

    fn next_run_id(&mut self) -> RunId {
        RunId::derived(self.id.as_uuid(), self.take_sequence(), 1)
    }

    fn next_request_id(&mut self) -> RequestId {
        RequestId::derived(self.id.as_uuid(), self.take_sequence(), 2)
    }

    fn next_message_id(&mut self) -> MessageId {
        MessageId::derived(self.id.as_uuid(), self.take_sequence(), 3)
    }

    fn next_permission_id(&mut self) -> PermissionId {
        PermissionId::derived(self.id.as_uuid(), self.take_sequence(), 4)
    }

    fn next_timestamp(&mut self) -> Timestamp {
        Timestamp::from_sequence(self.take_sequence())
    }

    /// Build the request for the next model call. The behavior profile
    /// shapes it here — tools, descriptions, parameters, system prompt, and
    /// the final-turn prompt — without touching the canonical transcript.
    fn execution_request(&mut self, run_id: RunId) -> ExecutionRequest {
        self.build_request(run_id, false)
    }

    /// `reissue` rebuilds the request for a turn already counted (resume
    /// after pause): it repeats that turn's injected context instead of
    /// consuming new pending context.
    fn build_request(&mut self, run_id: RunId, reissue: bool) -> ExecutionRequest {
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

    fn state_changed(from: AgentStatus, to: AgentStatus) -> AgentEffect {
        AgentEffect::Emit {
            event: AgentEvent::StateChanged { from, to },
        }
    }

    fn fail(&mut self, code: &str, message: String) -> Vec<AgentEffect> {
        self.discard_trailing_empty_assistant_message();
        let from = self.state.status;
        let error = AgentError {
            message,
            code: code.into(),
            details: None,
        };
        self.state.status = AgentStatus::Failed;
        self.state.active_run = None;
        self.state.backend_in_flight = false;
        self.state.last_error = Some(error.clone());
        self.usage.runs = self.usage.runs.saturating_add(1);
        vec![
            Self::state_changed(from, AgentStatus::Failed),
            AgentEffect::Emit {
                event: AgentEvent::Failed {
                    error: error.clone(),
                },
            },
            AgentEffect::FinishRun {
                result: AgentResult {
                    summary: error.message,
                    usage: self.terminal_usage_summary(),
                    gate_passed: None,
                },
            },
        ]
    }

    fn start_or_queue(&mut self, input: UserInput) -> Vec<AgentEffect> {
        if self.state.active_run.is_some() {
            self.state.queued_inputs.push_back(input);
            return Vec::new();
        }
        self.start_run(input)
    }

    fn start_run(&mut self, input: UserInput) -> Vec<AgentEffect> {
        if let Err(error) = validate_transcript(&self.state.messages) {
            return self.fail("INVALID_TRANSCRIPT", error.to_string());
        }
        if let Err(error) = validate_attachments(&input.attachments) {
            return self.fail("ATTACHMENT_TOO_LARGE", error);
        }
        let mut effects = Vec::new();
        // A switch requested during the previous run applies to this one
        // from its start, so `RunStart` rules run under it too.
        if let Some((profile, library)) = self.state.pending_profile.take() {
            effects.extend(self.apply_switch(profile, library));
        }
        self.state.behavior.reset_run();
        let from = self.state.status;
        let run_id = self.next_run_id();
        let message_id = self.next_message_id();
        let created_at = self.next_timestamp();
        let mut content = vec![ContentBlock::Text { text: input.text }];
        content.extend(
            input
                .attachments
                .into_iter()
                .map(attachment_to_content_block),
        );
        self.state.messages.push(AgentMessage {
            id: message_id,
            role: MessageRole::User,
            content,
            created_at,
        });
        self.state.status = AgentStatus::PreparingContext;
        self.state.active_run = Some(run_id);
        let outcome = self.state.behavior.evaluate(RuleEvent::RunStart, None);
        effects.extend(self.apply_rule_outcome(&outcome, None));
        if let Some(stop) = &outcome.stop {
            effects.extend(self.behavior_stop(stop));
            return effects;
        }
        match self.begin_turn() {
            Ok(turn_effects) => effects.extend(turn_effects),
            Err(failure) => {
                effects.extend(failure);
                return effects;
            }
        }
        let request = self.execution_request(run_id);
        effects.push(Self::state_changed(from, AgentStatus::PreparingContext));
        effects.push(AgentEffect::ExecuteBackend { request });
        effects.push(AgentEffect::Emit {
            event: AgentEvent::RunStarted { run_id },
        });
        effects
    }

    fn backend_event(&mut self, run_id: RunId, event: ExecutionEvent) -> Vec<AgentEffect> {
        if self.state.active_run != Some(run_id) {
            return Vec::new();
        }
        match event {
            ExecutionEvent::TextDelta { delta, .. } => {
                let from = self.state.status;
                self.state.status = AgentStatus::Streaming;
                let message_id = self.append_assistant_text(&delta);
                let mut effects = Vec::new();
                if from != AgentStatus::Streaming {
                    effects.push(Self::state_changed(from, AgentStatus::Streaming));
                }
                effects.push(AgentEffect::Emit {
                    event: AgentEvent::AssistantTextDelta { message_id, delta },
                });
                effects
            }
            ExecutionEvent::ReasoningDelta { delta, .. } => {
                let message_id = self.assistant_message_id();
                vec![AgentEffect::Emit {
                    event: AgentEvent::ReasoningDelta { message_id, delta },
                }]
            }
            ExecutionEvent::ToolCallRequested { call, .. } => self.tool_requested(call),
            ExecutionEvent::ToolCallStarted { call, .. } => {
                let from = self.state.status;
                self.state.status = AgentStatus::Executing;
                let call_id = call.id;
                self.push_tool_use(call.clone());
                let mut effects = Vec::new();
                if from != AgentStatus::Executing {
                    effects.push(Self::state_changed(from, AgentStatus::Executing));
                }
                effects.push(AgentEffect::Emit {
                    event: AgentEvent::ToolCallRequested { call },
                });
                effects.push(AgentEffect::Emit {
                    event: AgentEvent::ToolCallStarted { call_id },
                });
                effects
            }
            ExecutionEvent::ToolCallCompleted {
                call_id, result, ..
            } => {
                let from = self.state.status;
                self.state.status = AgentStatus::Streaming;
                self.usage.tool_calls = self.usage.tool_calls.saturating_add(1);
                let message_id = self.next_message_id();
                let created_at = self.next_timestamp();
                self.state.messages.push(AgentMessage {
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
                    effects.push(Self::state_changed(from, AgentStatus::Streaming));
                }
                effects.push(AgentEffect::Emit {
                    event: AgentEvent::ToolCallCompleted { call_id, result },
                });
                effects
            }
            ExecutionEvent::UsageUpdate { usage, .. } => {
                let timestamp = self.next_timestamp().to_rfc3339();
                vec![AgentEffect::Emit {
                    event: AgentEvent::UsageUpdated {
                        usage: AgentUsageSnapshot {
                            agent_id: self.id.to_string(),
                            model: self.state.execution_params.model.clone(),
                            metrics: AgentUsageMetrics {
                                total_runs: self.usage.runs,
                                total_requests: self.usage.records.len() as u64,
                                total_tool_calls: self.usage.tool_calls,
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
            ExecutionEvent::Completed { result, .. } => {
                self.state.backend_in_flight = false;
                self.discard_trailing_empty_assistant_message();
                let is_tool_turn = result.finish_reason == "tool_use";
                self.usage.records.push(UsageRecord {
                    model_usage: result.usage,
                    cost: result.cost,
                    tool_usage: None,
                });
                if matches!(
                    result.finish_reason.as_str(),
                    "max_tokens" | "length" | "max_output_tokens"
                ) {
                    return self.fail(
                        "OUTPUT_LIMIT_REACHED",
                        format!("The model stopped at its output token limit ({}); the response is incomplete.", result.finish_reason),
                    );
                }
                if is_tool_turn {
                    return self.continue_after_tools();
                }
                self.propose_completion(run_id, format!("Run completed: {}", result.finish_reason))
            }
            ExecutionEvent::Error { error, .. } => self.fail("BACKEND_ERROR", format!("{error:?}")),
        }
    }

    fn finish_success(&mut self, summary: String, gate_passed: Option<bool>) -> Vec<AgentEffect> {
        let from = self.state.status;
        self.state.status = AgentStatus::Idle;
        self.state.active_run = None;
        self.usage.runs = self.usage.runs.saturating_add(1);
        vec![
            Self::state_changed(from, AgentStatus::Idle),
            AgentEffect::Emit {
                event: AgentEvent::Completed {
                    outcome: AgentOutcome::Success,
                },
            },
            AgentEffect::FinishRun {
                result: AgentResult {
                    summary,
                    usage: self.terminal_usage_summary(),
                    gate_passed,
                },
            },
        ]
    }

    /// The model produced a turn with no tool calls: its proposed final
    /// answer. Without a completion gate the run finishes. With one,
    /// declarative checks run now; evaluator checks are handed to the
    /// runtime and the agent waits in `Verifying`.
    fn propose_completion(&mut self, run_id: RunId, summary: String) -> Vec<AgentEffect> {
        let profile = self.state.behavior.profile.clone();
        let Some(gate) = &profile.profile.completion_gate else {
            return self.finish_success(summary, None);
        };
        self.state.behavior.run.gate_attempts += 1;
        let failures: Vec<(String, String)> = gate
            .checks
            .iter()
            .filter_map(|check| {
                let condition = check.require.as_ref()?;
                (!self.state.behavior.condition_holds(condition))
                    .then(|| (check.id.clone(), check.feedback.clone().unwrap_or_default()))
            })
            .collect();
        let evaluators: Vec<CompletionCheckSpec> = gate
            .checks
            .iter()
            .filter_map(|check| {
                check
                    .evaluator
                    .as_ref()
                    .map(|evaluator| CompletionCheckSpec {
                        id: check.id.clone(),
                        evaluator: serde_json::to_value(evaluator).expect("evaluators serialize"),
                    })
            })
            .collect();
        // Declarative failures are decided without I/O; evaluators only run
        // once those pass.
        if !failures.is_empty() || evaluators.is_empty() {
            return self.gate_decided(run_id, failures, summary);
        }
        let transcript_messages = gate
            .checks
            .iter()
            .filter_map(|check| match &check.evaluator {
                Some(EvaluatorSpec::Model {
                    transcript_messages,
                    ..
                })
                | Some(EvaluatorSpec::Agent {
                    transcript_messages,
                    ..
                }) => Some(*transcript_messages as usize),
                Some(EvaluatorSpec::Command { .. }) => Some(COMMAND_TRANSCRIPT_MESSAGES),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        let messages = &self.state.messages;
        let transcript = messages[messages.len().saturating_sub(transcript_messages)..].to_vec();
        let final_response = self.final_response_text();
        let from = self.state.status;
        self.state.status = AgentStatus::Verifying;
        vec![
            Self::state_changed(from, AgentStatus::Verifying),
            AgentEffect::EvaluateCompletion {
                request: CompletionEvaluationRequest {
                    run_id,
                    attempt: self.state.behavior.run.gate_attempts,
                    checks: evaluators,
                    final_response,
                    transcript,
                },
            },
        ]
    }

    fn final_response_text(&self) -> String {
        self.state
            .messages
            .iter()
            .rev()
            .find(|message| message.role == MessageRole::Assistant)
            .map(|message| {
                message
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default()
    }

    fn completion_evaluated(
        &mut self,
        run_id: RunId,
        attempt: u32,
        verdicts: Vec<CheckVerdict>,
        usage: Vec<UsageRecord>,
    ) -> Vec<AgentEffect> {
        // Evaluator requests were made either way, so they always count.
        let mut effects = Vec::new();
        if !usage.is_empty() {
            self.usage.records.extend(usage);
            effects.push(AgentEffect::Emit {
                event: AgentEvent::UsageUpdated {
                    usage: AgentUsageSnapshot {
                        agent_id: self.id.to_string(),
                        model: self.state.execution_params.model.clone(),
                        metrics: self.usage.self_metrics(),
                        timestamp: self.next_timestamp().to_rfc3339(),
                    },
                },
            });
        }
        // Stale or unexpected verdicts (cancelled, superseded) are ignored.
        if self.state.status != AgentStatus::Verifying
            || self.state.active_run != Some(run_id)
            || self.state.behavior.run.gate_attempts != attempt
        {
            return effects;
        }
        let profile = self.state.behavior.profile.clone();
        let Some(gate) = &profile.profile.completion_gate else {
            return Vec::new();
        };
        let failures = gate
            .checks
            .iter()
            .filter(|check| check.evaluator.is_some())
            .filter_map(|check| {
                let outcome = verdicts
                    .iter()
                    .find(|verdict| verdict.id == check.id)
                    .map(|verdict| verdict.outcome.clone())
                    .unwrap_or(VerdictOutcome::Error {
                        message: "no verdict was produced".into(),
                    });
                let feedback = match outcome {
                    VerdictOutcome::Pass => return None,
                    VerdictOutcome::Fail { feedback } => feedback,
                    VerdictOutcome::Error { .. } if check.error_policy == ErrorPolicy::Pass => {
                        return None
                    }
                    VerdictOutcome::Error { message } => {
                        format!("the check could not be evaluated: {message}")
                    }
                };
                Some((check.id.clone(), check.feedback.clone().unwrap_or(feedback)))
            })
            .collect();
        let summary = "Run completed: end_turn".to_string();
        effects.extend(self.gate_decided(run_id, failures, summary));
        effects
    }

    /// Apply the gate's decision: finish, send the rejection back and
    /// continue, or handle exhaustion.
    fn gate_decided(
        &mut self,
        run_id: RunId,
        failures: Vec<(String, String)>,
        summary: String,
    ) -> Vec<AgentEffect> {
        let profile = self.state.behavior.profile.clone();
        let gate = profile
            .profile
            .completion_gate
            .as_ref()
            .expect("only called with a gate");
        let attempt = self.state.behavior.run.gate_attempts;
        let failed_checks: Vec<String> = failures.iter().map(|(id, _)| id.clone()).collect();
        let evaluated = |passed: bool, continuing: bool| AgentEffect::Emit {
            event: AgentEvent::CompletionGateEvaluated {
                attempt,
                passed,
                continuing,
                failed_checks: failed_checks.clone(),
            },
        };
        if failures.is_empty() {
            let mut effects = vec![evaluated(true, false)];
            effects.extend(self.finish_success(summary, Some(true)));
            return effects;
        }
        let run = &self.state.behavior.run;
        let can_continue = run.gate_continuations < gate.max_continuations && !run.final_turn;
        if !can_continue {
            let mut effects = vec![evaluated(false, false)];
            match gate.on_exhausted.unwrap_or(OnExhausted::Accept) {
                OnExhausted::Accept => effects.extend(self.finish_success(summary, Some(false))),
                OnExhausted::Fail => effects.extend(self.fail(
                    "GATE_EXHAUSTED",
                    format!(
                        "completion gate still failing after {} continuation(s): {}",
                        run.gate_continuations,
                        failed_checks.join(", ")
                    ),
                )),
            }
            return effects;
        }

        self.state.behavior.run.gate_continuations += 1;
        let mut text = String::from(
            "Your answer was not accepted yet. Address the following before finishing:",
        );
        for (id, feedback) in &failures {
            text.push_str(&format!("\n- [{id}] {feedback}"));
        }
        let message_id = self.next_message_id();
        let created_at = self.next_timestamp();
        self.state.messages.push(AgentMessage {
            id: message_id,
            role: MessageRole::User,
            content: vec![ContentBlock::Text {
                text: system_reminder(&format!("gate:{}", profile.reference()), &text),
            }],
            created_at,
        });
        let mut effects = vec![evaluated(false, true)];
        match self.begin_turn() {
            Ok(turn_effects) => effects.extend(turn_effects),
            Err(failure) => {
                effects.extend(failure);
                return effects;
            }
        }
        let from = self.state.status;
        self.state.status = AgentStatus::WaitingForBackend;
        let request = self.execution_request(run_id);
        effects.push(Self::state_changed(from, AgentStatus::WaitingForBackend));
        effects.push(AgentEffect::ExecuteBackend { request });
        effects
    }

    fn start_next_queued_run(&mut self) -> Vec<AgentEffect> {
        self.state
            .queued_inputs
            .pop_front()
            .map(|input| self.start_run(input))
            .unwrap_or_default()
    }

    fn tool_requested(&mut self, call: ToolCall) -> Vec<AgentEffect> {
        let from = self.state.status;
        let call_id = call.id;
        self.push_tool_use(call.clone());
        let started_at = self.next_timestamp();
        self.state.pending_tools.insert(
            call_id,
            PendingToolCall {
                call: call.clone(),
                started_at,
            },
        );
        let mut effects = vec![AgentEffect::Emit {
            event: AgentEvent::ToolCallRequested { call: call.clone() },
        }];
        let Some(session_mode) = self
            .capabilities
            .tools
            .tools
            .values()
            .find(|capability| capability.descriptor.name == call.name && capability.policy.enabled)
            .map(|capability| capability.policy.permission.clone())
        else {
            effects.extend(self.tool_failed(call_id, ToolError::PermissionDenied));
            return effects;
        };

        // The profile narrows what the session grants: a tool outside its
        // scope is not executable even if the model names it.
        let profile = self.state.behavior.profile.clone();
        if !profile.allows_tool(&call.name) {
            let reason = format!(
                "Tool {} is not available under profile {}.",
                call.name,
                profile.reference()
            );
            effects.extend(self.deny_tool(call_id, None, reason));
            return effects;
        }

        // Rules see the request next. A `deny` decides; `ask`/`allow` adjust
        // the permission, which a tool override may already have tightened.
        self.state.behavior.note_tool_request(&call);
        let outcome = self.state.behavior.evaluate(
            RuleEvent::PreToolUse,
            Some(ToolEvent {
                call: &call,
                result: None,
            }),
        );
        effects.extend(self.apply_rule_outcome(&outcome, Some(call_id)));
        if let Some(stop) = &outcome.stop {
            effects.extend(self.deny_tool(
                call_id,
                Some(stop.rule_id.clone()),
                stop.reason.clone(),
            ));
            effects.extend(self.behavior_stop(stop));
            return effects;
        }
        let session_denies = matches!(session_mode, PermissionMode::Deny);
        let mut mode = profile.permission(&call.name, session_mode);
        match &outcome.decision {
            Some(ToolDecision::Deny { rule_id, reason }) => {
                effects.extend(self.deny_tool(call_id, Some(rule_id.clone()), reason.clone()));
                return effects;
            }
            Some(ToolDecision::Ask { .. }) if !matches!(mode, PermissionMode::Deny) => {
                mode = PermissionMode::Ask
            }
            Some(ToolDecision::Allow { .. }) if matches!(mode, PermissionMode::Ask) => {
                mode = PermissionMode::Allow
            }
            _ => {}
        }
        if matches!(mode, PermissionMode::Deny) {
            if session_denies {
                effects.extend(self.tool_failed(call_id, ToolError::PermissionDenied));
            } else {
                let reason = format!(
                    "Tool {} is denied by profile {}.",
                    call.name,
                    profile.reference()
                );
                effects.extend(self.deny_tool(call_id, None, reason));
            }
            return effects;
        }
        let call = self.rewrite_arguments(call, &outcome);
        // Calls past the run's tool budget (or on the final turn) are refused.
        if !self.state.behavior.admit_tool_call() {
            let reason = if self.state.behavior.run.final_turn {
                "No tools are available on the final turn. Answer in text.".to_string()
            } else {
                "The tool-call budget for this run is exhausted. Answer with what you have."
                    .to_string()
            };
            effects.extend(self.deny_tool(call_id, None, reason));
            return effects;
        }
        match mode {
            PermissionMode::Allow => {
                self.state.status = AgentStatus::Executing;
                effects.push(Self::state_changed(from, AgentStatus::Executing));
                effects.push(AgentEffect::ExecuteTool {
                    request: ToolRequest {
                        call,
                        permission: PermissionMode::Allow,
                    },
                });
            }
            PermissionMode::Ask => {
                self.state.status = AgentStatus::WaitingForPermission;
                effects.push(Self::state_changed(from, AgentStatus::WaitingForPermission));
                let permission_id = self.next_permission_id();
                self.state
                    .pending_permissions
                    .insert(permission_id, call_id);
                let request = PermissionRequest {
                    id: permission_id,
                    tool_call: call,
                    agent_id: self.id,
                };
                effects.push(AgentEffect::Emit {
                    event: AgentEvent::PermissionRequested {
                        request: request.clone(),
                    },
                });
                effects.push(AgentEffect::RequestPermission { request });
            }
            // Denials returned above.
            PermissionMode::Deny => {}
        }
        effects
    }

    fn tool_completed(&mut self, call_id: ToolCallId, result: ToolResult) -> Vec<AgentEffect> {
        let Some(pending) = self.state.pending_tools.remove(&call_id) else {
            return Vec::new();
        };
        self.state.consecutive_tool_timeouts = 0;
        self.state
            .pending_permissions
            .retain(|_, id| *id != call_id);
        self.record_tool_result(
            pending.call,
            true,
            result.is_error,
            serde_json::to_string(&result.output).unwrap_or_default(),
        )
    }

    fn tool_failed(&mut self, call_id: ToolCallId, error: ToolError) -> Vec<AgentEffect> {
        let Some(pending) = self.state.pending_tools.remove(&call_id) else {
            return Vec::new();
        };
        self.state
            .pending_permissions
            .retain(|_, id| *id != call_id);
        // A refused call never ran; one that failed while running did.
        let executed = !matches!(
            error,
            ToolError::PermissionDenied | ToolError::Denied { .. }
        );
        if matches!(error, ToolError::Timeout) {
            self.state.consecutive_tool_timeouts += 1;
        } else {
            self.state.consecutive_tool_timeouts = 0;
        }
        let preview = match error {
            ToolError::Denied { reason } => format!("Denied: {reason}"),
            ToolError::Timeout => format!(
                "Tool call `{}` timed out and was cancelled. Try a narrower request or a different approach.",
                pending.call.name
            ),
            other => format!("{other:?}"),
        };
        self.record_tool_result(pending.call, executed, true, preview)
    }

    /// Apply `rewrite_args` merge patches. The pending call is replaced, so
    /// approval prompts and execution see the rewritten arguments; the model
    /// is told what ran through a note on the result.
    fn rewrite_arguments(&mut self, mut call: ToolCall, outcome: &RuleOutcome) -> ToolCall {
        if outcome.rewrite_args.is_empty() {
            return call;
        }
        for (_, patch) in &outcome.rewrite_args {
            merge_patch(&mut call.arguments, patch);
        }
        if let Some(pending) = self.state.pending_tools.get_mut(&call.id) {
            pending.call.arguments = call.arguments.clone();
        }
        let rules: Vec<&str> = outcome
            .rewrite_args
            .iter()
            .map(|(rule_id, _)| rule_id.as_str())
            .collect();
        let note = system_reminder(
            &format!(
                "profile:{} rule:{}",
                self.state.behavior.reference(),
                rules.join(",")
            ),
            &format!(
                "The harness adjusted this call's arguments before running it: {}",
                call.arguments
            ),
        );
        self.state
            .behavior
            .run
            .pending_results
            .push((call.id, note));
        call
    }

    /// Refuse a tool call before execution, telling the model why.
    fn deny_tool(
        &mut self,
        call_id: ToolCallId,
        rule_id: Option<String>,
        reason: String,
    ) -> Vec<AgentEffect> {
        let mut effects = vec![AgentEffect::Emit {
            event: AgentEvent::ToolCallDenied {
                call_id,
                rule_id,
                reason: reason.clone(),
            },
        }];
        effects.extend(self.tool_failed(call_id, ToolError::Denied { reason }));
        effects
    }

    fn record_tool_result(
        &mut self,
        call: ToolCall,
        executed: bool,
        has_error: bool,
        mut preview: String,
    ) -> Vec<AgentEffect> {
        let call_id = call.id;
        let mut effects = Vec::new();
        let mut stop = None;
        if executed {
            self.state
                .behavior
                .note_executed_with(&call.name, has_error);
            let event = if has_error {
                RuleEvent::PostToolUseFailure
            } else {
                RuleEvent::PostToolUse
            };
            let outcome = self.state.behavior.evaluate(
                event,
                Some(ToolEvent {
                    call: &call,
                    result: Some(&preview),
                }),
            );
            effects.extend(self.apply_rule_outcome(&outcome, Some(call_id)));
            for (_, rewrite) in &outcome.rewrite_result {
                rewrite.apply(&mut preview);
            }
            stop = outcome.stop;
        }
        let run = &mut self.state.behavior.run;
        run.pending_results.retain(|(pending_call, text)| {
            if *pending_call == call_id {
                preview.push_str("\n\n");
                preview.push_str(text);
                false
            } else {
                true
            }
        });
        self.usage.tool_calls = self.usage.tool_calls.saturating_add(1);
        let message_id = self.next_message_id();
        let created_at = self.next_timestamp();
        self.state.messages.push(AgentMessage {
            id: message_id,
            role: MessageRole::Tool,
            content: vec![ContentBlock::ToolResult {
                call_id,
                result: ToolResultSummary {
                    has_error,
                    output_preview: preview.clone(),
                },
            }],
            created_at,
        });
        effects.push(AgentEffect::Emit {
            event: AgentEvent::ToolCallCompleted {
                call_id,
                result: ToolResultSummary {
                    has_error,
                    output_preview: preview,
                },
            },
        });
        // The result is recorded first so the transcript stays valid.
        if stop.is_none() && self.state.consecutive_tool_timeouts >= MAX_CONSECUTIVE_TOOL_TIMEOUTS {
            let count = self.state.consecutive_tool_timeouts;
            self.state.consecutive_tool_timeouts = 0;
            effects.extend(self.fail(
                "TOOL_TIMEOUT",
                format!("{count} consecutive tool calls timed out"),
            ));
            return effects;
        }
        match stop {
            Some(stop) => effects.extend(self.behavior_stop(&stop)),
            None => effects.extend(self.continue_after_tools()),
        }
        effects
    }

    fn continue_after_tools(&mut self) -> Vec<AgentEffect> {
        let mut effects = Vec::new();
        if self.state.pending_tools.is_empty() && !self.state.backend_in_flight {
            if let Some(run_id) = self.state.active_run {
                match self.begin_turn() {
                    Ok(turn_effects) => effects.extend(turn_effects),
                    Err(failure) => {
                        effects.extend(failure);
                        return effects;
                    }
                }
                let from = self.state.status;
                self.state.status = AgentStatus::WaitingForBackend;
                let request = self.execution_request(run_id);
                if from != AgentStatus::WaitingForBackend {
                    effects.push(Self::state_changed(from, AgentStatus::WaitingForBackend));
                }
                effects.push(AgentEffect::ExecuteBackend { request });
            }
        }
        effects
    }

    fn permission_resolved(
        &mut self,
        id: PermissionId,
        decision: PermissionDecision,
    ) -> Vec<AgentEffect> {
        let Some(call_id) = self.state.pending_permissions.remove(&id) else {
            return Vec::new();
        };
        let Some(call) = self
            .state
            .pending_tools
            .get(&call_id)
            .map(|pending| pending.call.clone())
        else {
            return Vec::new();
        };
        match decision {
            PermissionDecision::Approved => {
                self.state.status = AgentStatus::Executing;
                vec![
                    Self::state_changed(AgentStatus::WaitingForPermission, AgentStatus::Executing),
                    AgentEffect::ExecuteTool {
                        request: ToolRequest {
                            call,
                            permission: PermissionMode::Allow,
                        },
                    },
                ]
            }
            PermissionDecision::Denied => self.tool_failed(call_id, ToolError::PermissionDenied),
        }
    }

    fn child_completed(&mut self, agent_id: AgentId, result: AgentResult) -> Vec<AgentEffect> {
        self.state.children.retain(|child| *child != agent_id);
        self.usage
            .add_child_summary(agent_id, Self::bridge_usage_summary(&result.usage));
        let mut effects = vec![AgentEffect::Emit {
            event: AgentEvent::ChildAgentCompleted {
                agent_id,
                outcome: AgentOutcome::Success,
            },
        }];
        if self.state.children.is_empty() && self.state.status == AgentStatus::WaitingForChildren {
            self.state.status = AgentStatus::Idle;
            effects.push(AgentEffect::Emit {
                event: AgentEvent::StateChanged {
                    from: AgentStatus::WaitingForChildren,
                    to: AgentStatus::Idle,
                },
            });
        }
        effects
    }

    fn child_spawned(&mut self, agent_id: AgentId, awaiting: bool) -> Vec<AgentEffect> {
        if !self.state.children.contains(&agent_id) {
            self.state.children.push(agent_id);
        }
        if !awaiting || self.state.status == AgentStatus::WaitingForChildren {
            return Vec::new();
        }
        let from = self.state.status;
        self.state.status = AgentStatus::WaitingForChildren;
        vec![Self::state_changed(from, AgentStatus::WaitingForChildren)]
    }

    fn child_failed(&mut self, agent_id: AgentId, error: AgentError) -> Vec<AgentEffect> {
        self.state.children.retain(|child| *child != agent_id);
        self.state.last_error = Some(error);
        let mut effects = vec![AgentEffect::Emit {
            event: AgentEvent::ChildAgentCompleted {
                agent_id,
                outcome: AgentOutcome::Failed,
            },
        }];
        if self.state.children.is_empty() && self.state.status == AgentStatus::WaitingForChildren {
            self.state.status = AgentStatus::Idle;
            effects.push(AgentEffect::Emit {
                event: AgentEvent::StateChanged {
                    from: AgentStatus::WaitingForChildren,
                    to: AgentStatus::Idle,
                },
            });
        }
        effects
    }

    /// Lossless conversion from a completed child's reported (protocol-level)
    /// usage summary into the shape this agent's ledger stores its children
    /// under. Both types have identical fields — see
    /// `crate::usage::AgentUsageSummary`'s doc comment for why the type
    /// still exists separately from the protocol one.
    fn bridge_usage_summary(
        protocol: &harness_protocol::usage::AgentUsageSummary,
    ) -> crate::usage::AgentUsageSummary {
        crate::usage::AgentUsageSummary::from(protocol.clone())
    }

    /// Builds this agent's final, reportable usage summary: its own request
    /// count / tool-call count / tokens / cost (`self.usage.self_metrics()`),
    /// combined with every child's already-aggregated inclusive usage
    /// (`self.usage.child_usage`) via `compute_agent_usage_summary`, which
    /// sums children's *inclusive* usage — not `self_usage` — so a
    /// grandchild's activity is counted exactly once even though it reaches
    /// this agent through two levels of `ChildCompleted` bridging.
    fn terminal_usage_summary(&self) -> AgentUsageSummary {
        let self_metrics = self.usage.self_metrics();
        let child_summaries: Vec<crate::usage::AgentUsageSummary> =
            self.usage.child_usage.values().cloned().collect();
        crate::usage::compute_agent_usage_summary(self_metrics, &child_summaries).into()
    }

    fn cancel(&mut self) -> Vec<AgentEffect> {
        let from = self.state.status;
        if matches!(
            from,
            AgentStatus::Idle | AgentStatus::Cancelled | AgentStatus::Failed
        ) && self.state.active_run.is_none()
        {
            return Vec::new();
        }

        let mut effects = Vec::new();
        if let Some(run_id) = self.state.active_run {
            effects.push(AgentEffect::CancelBackend { run_id });
            if from == AgentStatus::Verifying {
                effects.push(AgentEffect::CancelEvaluation { run_id });
            }
        }

        let mut calls: Vec<_> = self.state.pending_tools.keys().copied().collect();
        calls.sort();
        effects.extend(
            calls
                .into_iter()
                .map(|call_id| AgentEffect::CancelTool { call_id }),
        );

        let mut children = self.state.children.clone();
        children.sort();
        effects.extend(
            children
                .into_iter()
                .map(|agent_id| AgentEffect::CancelChild { agent_id }),
        );

        self.state.status = AgentStatus::Cancelled;
        self.state.active_run = None;
        self.state.backend_in_flight = false;
        self.state.pending_tools.clear();
        self.state.pending_permissions.clear();
        self.state.children.clear();
        // `Cancel` is documented and certified (RC-203,
        // `multiple_follow_ups_are_fifo_and_survive_cancellation`) as
        // cancelling only the *current run*: already-queued follow-up/steer
        // input is a separate, already-committed piece of user intent and
        // must remain available for an explicit `StartNextQueuedRun` after
        // the cancel — including across a restore, so a queued follow-up
        // typed just before a crash is not silently lost. Do not clear
        // `queued_inputs` here; a caller that truly wants to abandon queued
        // work (e.g. a full session teardown) should do so explicitly at
        // its own layer instead of narrowing this shared transition.

        effects.push(Self::state_changed(from, AgentStatus::Cancelled));
        effects.push(AgentEffect::Emit {
            event: AgentEvent::Completed {
                outcome: AgentOutcome::Cancelled,
            },
        });
        effects
    }

    fn pause(&mut self) -> Vec<AgentEffect> {
        // A completion evaluation is short and bounded; pausing mid-way would
        // discard its verdict and make resume re-ask the model. It is not
        // interruptible by pause (cancel still stops it).
        if matches!(
            self.state.status,
            AgentStatus::Paused
                | AgentStatus::Cancelled
                | AgentStatus::Failed
                | AgentStatus::Verifying
        ) {
            return Vec::new();
        }
        let from = self.state.status;
        self.state.status = AgentStatus::Paused;
        vec![Self::state_changed(from, AgentStatus::Paused)]
    }

    fn resume(&mut self) -> Vec<AgentEffect> {
        if self.state.status != AgentStatus::Paused {
            return Vec::new();
        }
        match self.state.active_run {
            Some(run_id) => {
                self.state.status = AgentStatus::WaitingForBackend;
                let request = self.build_request(run_id, true);
                vec![
                    Self::state_changed(AgentStatus::Paused, AgentStatus::WaitingForBackend),
                    AgentEffect::ExecuteBackend { request },
                ]
            }
            None => {
                self.state.status = AgentStatus::Idle;
                vec![Self::state_changed(AgentStatus::Paused, AgentStatus::Idle)]
            }
        }
    }

    fn append_assistant_text(&mut self, delta: &str) -> MessageId {
        // Only the transcript's *last* message may absorb the delta. An
        // earlier assistant message -- one before the user turn that started
        // this run, or before a tool result -- belongs to a different turn,
        // and appending to it would splice the new answer into the old one
        // and leave the current user message with no reply after it.
        if let Some(message) = self.trailing_assistant_message() {
            if let Some(ContentBlock::Text { text }) = message
                .content
                .iter_mut()
                .find(|block| matches!(block, ContentBlock::Text { .. }))
            {
                push_bounded(text, delta);
                return message.id;
            }
            // Opened by a reasoning delta and still empty: give it the text
            // rather than leaving a content-less assistant message behind,
            // which providers reject on the next request.
            if !message
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolUse { .. }))
            {
                let mut text = String::new();
                push_bounded(&mut text, delta);
                message.content.push(ContentBlock::Text { text });
                return message.id;
            }
        }

        let id = self.next_message_id();
        let created_at = self.next_timestamp();
        let mut text = String::new();
        push_bounded(&mut text, delta);
        self.state.messages.push(AgentMessage {
            id,
            role: MessageRole::Assistant,
            content: vec![ContentBlock::Text { text }],
            created_at,
        });
        id
    }

    /// The message a reasoning delta belongs to: the assistant message the
    /// current turn is building, or a fresh one if the turn has produced
    /// nothing else yet. The fresh message starts empty; the text or tool
    /// call that follows fills it, and [`Self::discard_trailing_empty_assistant_message`]
    /// removes it if nothing ever does.
    fn assistant_message_id(&mut self) -> MessageId {
        if let Some(message) = self.trailing_assistant_message() {
            return message.id;
        }

        let id = self.next_message_id();
        let created_at = self.next_timestamp();
        self.state.messages.push(AgentMessage {
            id,
            role: MessageRole::Assistant,
            content: Vec::new(),
            created_at,
        });
        id
    }

    /// Records a tool call the model just requested. Reuses the turn's
    /// assistant message when it has no content yet (opened by a reasoning
    /// delta), so a reasoning-then-tool-call turn does not leave an empty
    /// assistant message ahead of the tool call.
    fn push_tool_use(&mut self, call: ToolCall) -> MessageId {
        if let Some(message) = self.trailing_assistant_message() {
            if message.content.is_empty() {
                message.content.push(ContentBlock::ToolUse { call });
                return message.id;
            }
        }
        let message_id = self.next_message_id();
        let created_at = self.next_timestamp();
        self.state.messages.push(AgentMessage {
            id: message_id,
            role: MessageRole::Assistant,
            content: vec![ContentBlock::ToolUse { call }],
            created_at,
        });
        message_id
    }

    fn trailing_assistant_message(&mut self) -> Option<&mut AgentMessage> {
        self.state
            .messages
            .last_mut()
            .filter(|message| message.role == MessageRole::Assistant)
    }

    /// Drops a trailing assistant message that never received content -- a
    /// turn that produced only reasoning, or nothing at all. Every provider
    /// rejects a content-less assistant message on the next request
    /// (Cohere: "must have non-empty content or tool calls"), and it carries
    /// nothing worth keeping.
    fn discard_trailing_empty_assistant_message(&mut self) {
        if self
            .trailing_assistant_message()
            .is_some_and(|message| message.content.is_empty())
        {
            self.state.messages.pop();
        }
    }
}
