
use harness_protocol::backend::{ExecutionEvent, ExecutionRequest};
use harness_protocol::commands::{
    AgentCommand, AgentError, AgentResult, AgentStatus, PermissionDecision, UserInput,
};
use harness_protocol::effects::{AgentEffect, PermissionRequest, ToolRequest};
use harness_protocol::events::{AgentEvent, AgentOutcome};
use harness_protocol::ids::{
    AgentId, MessageId, PermissionId, RequestId, RunId, Timestamp, ToolCallId,
};
use harness_protocol::messages::{AgentMessage, ContentBlock, MessageRole};
use harness_protocol::tools::{PermissionMode, ToolCall, ToolError, ToolResult, ToolResultSummary};
use harness_protocol::usage::{AgentUsageMetrics, AgentUsageSummary, UsageRecord};

use crate::agent::Agent;
use crate::agent_state::PendingToolCall;
use crate::transcript::validate_transcript;

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
        }
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

    fn execution_request(&mut self, run_id: RunId) -> ExecutionRequest {
        let tools = self
            .capabilities
            .tools
            .enabled_descriptors()
            .into_iter()
            .cloned()
            .collect();
        ExecutionRequest {
            request_id: self.next_request_id(),
            run_id,
            system_prompt: self.state.system_prompt.clone(),
            messages: self.state.messages.clone(),
            tools,
            extended_thinking: false,
        }
    }

    fn state_changed(from: AgentStatus, to: AgentStatus) -> AgentEffect {
        AgentEffect::Emit {
            event: AgentEvent::StateChanged { from, to },
        }
    }

    fn fail(&mut self, code: &str, message: String) -> Vec<AgentEffect> {
        let from = self.state.status;
        let error = AgentError {
            message,
            code: code.into(),
            details: None,
        };
        self.state.status = AgentStatus::Failed;
        self.state.active_run = None;
        self.state.last_error = Some(error.clone());
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
        let from = self.state.status;
        let run_id = self.next_run_id();
        let message_id = self.next_message_id();
        let created_at = self.next_timestamp();
        self.state.messages.push(AgentMessage {
            id: message_id,
            role: MessageRole::User,
            content: vec![ContentBlock::Text { text: input.text }],
            created_at,
        });
        self.state.status = AgentStatus::PreparingContext;
        self.state.active_run = Some(run_id);
        let request = self.execution_request(run_id);
        vec![
            Self::state_changed(from, AgentStatus::PreparingContext),
            AgentEffect::ExecuteBackend { request },
            AgentEffect::Emit {
                event: AgentEvent::RunStarted { run_id },
            },
        ]
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
            ExecutionEvent::UsageUpdate { .. } => Vec::new(),
            ExecutionEvent::Completed { result, .. } => {
                let is_tool_turn = result.finish_reason == "tool_use";
                self.usage.records.push(UsageRecord {
                    model_usage: result.usage,
                    cost: result.cost,
                    tool_usage: None,
                });
                if is_tool_turn {
                    return Vec::new();
                }
                let from = self.state.status;
                self.state.status = AgentStatus::Idle;
                self.state.active_run = None;
                let effects = vec![
                    Self::state_changed(from, AgentStatus::Idle),
                    AgentEffect::Emit {
                        event: AgentEvent::Completed {
                            outcome: AgentOutcome::Success,
                        },
                    },
                    AgentEffect::FinishRun {
                        result: AgentResult {
                            summary: format!("Run completed: {}", result.finish_reason),
                            usage: self.terminal_usage_summary(),
                        },
                    },
                ];
                effects
            }
            ExecutionEvent::Error { error, .. } => self.fail("BACKEND_ERROR", format!("{error:?}")),
        }
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
        let message_id = self.next_message_id();
        let created_at = self.next_timestamp();
        self.state.messages.push(AgentMessage {
            id: message_id,
            role: MessageRole::Assistant,
            content: vec![ContentBlock::ToolUse { call: call.clone() }],
            created_at,
        });
        let started_at = self.next_timestamp();
        self.state.pending_tools.insert(
            call_id,
            PendingToolCall {
                call: call.clone(),
                started_at,
            },
        );
        let permission = self
            .capabilities
            .tools
            .tools
            .values()
            .find(|capability| capability.descriptor.name == call.name && capability.policy.enabled)
            .map(|capability| capability.policy.permission.clone());
        let mut effects = vec![AgentEffect::Emit {
            event: AgentEvent::ToolCallRequested { call: call.clone() },
        }];
        match permission {
            Some(PermissionMode::Allow) => {
                self.state.status = AgentStatus::Executing;
                effects.push(Self::state_changed(from, AgentStatus::Executing));
                effects.push(AgentEffect::ExecuteTool {
                    request: ToolRequest {
                        call,
                        permission: PermissionMode::Allow,
                    },
                });
            }
            Some(PermissionMode::Ask) => {
                self.state.status = AgentStatus::WaitingForPermission;
                effects.push(Self::state_changed(
                    from,
                    AgentStatus::WaitingForPermission,
                ));
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
            Some(PermissionMode::Deny) | None => {
                effects.extend(self.tool_failed(call_id, ToolError::PermissionDenied));
            }
        }
        effects
    }

    fn tool_completed(&mut self, call_id: ToolCallId, result: ToolResult) -> Vec<AgentEffect> {
        if self.state.pending_tools.remove(&call_id).is_none() {
            return Vec::new();
        }
        self.state
            .pending_permissions
            .retain(|_, id| *id != call_id);
        self.record_tool_result(
            call_id,
            result.is_error,
            serde_json::to_string(&result.output).unwrap_or_default(),
        )
    }

    fn tool_failed(&mut self, call_id: ToolCallId, error: ToolError) -> Vec<AgentEffect> {
        if self.state.pending_tools.remove(&call_id).is_none() {
            return Vec::new();
        }
        self.state
            .pending_permissions
            .retain(|_, id| *id != call_id);
        self.record_tool_result(call_id, true, format!("{error:?}"))
    }

    fn record_tool_result(
        &mut self,
        call_id: ToolCallId,
        has_error: bool,
        preview: String,
    ) -> Vec<AgentEffect> {
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
        let mut effects = vec![AgentEffect::Emit {
            event: AgentEvent::ToolCallCompleted {
                call_id,
                result: ToolResultSummary {
                    has_error,
                    output_preview: preview,
                },
            },
        }];
        if self.state.pending_tools.is_empty() {
            if let Some(run_id) = self.state.active_run {
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

    fn bridge_usage_summary(
        protocol: &harness_protocol::usage::AgentUsageSummary,
    ) -> crate::usage::AgentUsageSummary {
        fn to_model_usage(
            metrics: &harness_protocol::usage::AgentUsageMetrics,
        ) -> harness_protocol::usage::ModelUsage {
            harness_protocol::usage::ModelUsage {
                total_tokens: metrics.total_tokens,
                ..Default::default()
            }
        }
        crate::usage::AgentUsageSummary {
            self_usage: to_model_usage(&protocol.self_usage),
            descendant_usage: to_model_usage(&protocol.descendant_usage),
            inclusive_usage: to_model_usage(&protocol.inclusive_usage),
        }
    }

    fn terminal_usage_summary(&self) -> AgentUsageSummary {
        let self_usage = self.usage.self_usage();
        AgentUsageSummary {
            self_usage: AgentUsageMetrics {
                total_tokens: self_usage.total_tokens,
                ..Default::default()
            },
            descendant_usage: AgentUsageMetrics::default(),
            inclusive_usage: AgentUsageMetrics {
                total_tokens: self_usage.total_tokens,
                ..Default::default()
            },
        }
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
        if matches!(
            self.state.status,
            AgentStatus::Paused | AgentStatus::Cancelled | AgentStatus::Failed
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
                let request = self.execution_request(run_id);
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
        if let Some(message) = self
            .state
            .messages
            .iter_mut()
            .rev()
            .find(|message| message.role == MessageRole::Assistant)
        {
            if let Some(ContentBlock::Text { text }) = message
                .content
                .iter_mut()
                .find(|block| matches!(block, ContentBlock::Text { .. }))
            {
                text.push_str(delta);
                return message.id;
            }
        }

        let id = self.next_message_id();
        let created_at = self.next_timestamp();
        self.state.messages.push(AgentMessage {
            id,
            role: MessageRole::Assistant,
            content: vec![ContentBlock::Text {
                text: delta.to_owned(),
            }],
            created_at,
        });
        id
    }

    fn assistant_message_id(&mut self) -> MessageId {
        if let Some(id) = self
            .state
            .messages
            .iter()
            .rev()
            .find(|message| message.role == MessageRole::Assistant)
            .map(|message| message.id)
        {
            return id;
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
}
