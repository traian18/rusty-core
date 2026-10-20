use std::collections::{HashMap, HashSet};

use harness_protocol::{
    effects::PermissionRequest,
    events::{AgentEvent, AgentOutcome},
    ids::{AgentId, EventId, MessageId, PermissionId, ToolCallId},
    tools::{ToolCall, ToolResultSummary},
};

use crate::model::{PermissionDisplayDecision, ToolCallState, TranscriptBlock};

#[derive(Default)]
pub(super) struct EventTranscript {
    seen_events: HashSet<EventId>,
    messages: HashMap<MessageId, usize>,
    tools: HashMap<ToolCallId, usize>,
    permissions: HashMap<PermissionId, usize>,
    children: HashMap<AgentId, usize>,
}

impl EventTranscript {
    pub(super) fn mark_seen(&mut self, event_id: EventId) -> bool {
        self.seen_events.insert(event_id)
    }

    pub(super) fn fold_conversation_event(
        &mut self,
        transcript: &mut Vec<TranscriptBlock>,
        pending_permissions: &mut std::collections::VecDeque<PermissionId>,
        agent_id: AgentId,
        event: AgentEvent,
    ) -> Option<AgentEvent> {
        if self.fold_tool_event(transcript, agent_id, &event)
            || self.fold_message_event(transcript, pending_permissions, agent_id, &event)
        {
            return None;
        }
        Some(event)
    }

    pub(super) fn clear_indexes(&mut self) {
        self.seen_events.clear();
        self.messages.clear();
        self.tools.clear();
        self.permissions.clear();
        self.children.clear();
    }

    pub(super) fn resolve_permission(
        &mut self,
        transcript: &mut [TranscriptBlock],
        pending_permissions: &mut std::collections::VecDeque<PermissionId>,
        id: PermissionId,
        approved: bool,
    ) {
        pending_permissions.retain(|pending| *pending != id);
        if let Some(index) = self.permissions.get(&id).copied() {
            if let Some(TranscriptBlock::Permission { decision, .. }) = transcript.get_mut(index) {
                *decision = Some(if approved {
                    PermissionDisplayDecision::Approved
                } else {
                    PermissionDisplayDecision::Denied
                });
            }
        }
    }

    fn fold_tool_event(
        &mut self,
        transcript: &mut Vec<TranscriptBlock>,
        agent_id: AgentId,
        event: &AgentEvent,
    ) -> bool {
        match event {
            AgentEvent::ToolCallRequested { call } => {
                self.fold_tool_requested(transcript, agent_id, call.clone())
            }
            AgentEvent::ToolCallStarted { call_id } => {
                self.set_tool_state(transcript, *call_id, agent_id, ToolCallState::Running);
            }
            AgentEvent::ToolCallProgress { call_id, progress } => {
                self.set_tool_state(
                    transcript,
                    *call_id,
                    agent_id,
                    ToolCallState::Progress {
                        status: progress.status.clone(),
                        fraction: progress.fraction,
                    },
                );
            }
            AgentEvent::ToolCallCompleted { call_id, result } => {
                self.fold_tool_completed(transcript, *call_id, agent_id, result.clone());
            }
            _ => return false,
        }
        true
    }

    fn fold_message_event(
        &mut self,
        transcript: &mut Vec<TranscriptBlock>,
        pending_permissions: &mut std::collections::VecDeque<PermissionId>,
        agent_id: AgentId,
        event: &AgentEvent,
    ) -> bool {
        match event {
            AgentEvent::AssistantMessageStarted { message_id } => {
                self.ensure_message(transcript, *message_id, agent_id);
            }
            AgentEvent::AssistantTextDelta { message_id, delta } => {
                self.fold_message(transcript, *message_id, agent_id, delta, false);
            }
            AgentEvent::ReasoningDelta { message_id, delta } => {
                self.fold_message(transcript, *message_id, agent_id, delta, true);
            }
            AgentEvent::AssistantMessageCompleted { message_id } => {
                let index = self.ensure_message(transcript, *message_id, agent_id);
                if let TranscriptBlock::AssistantMessage { complete, .. } = &mut transcript[index] {
                    *complete = true;
                }
            }
            AgentEvent::PermissionRequested { request } => {
                self.fold_permission_requested(transcript, pending_permissions, request.clone());
            }
            AgentEvent::ChildAgentSpawned { agent_id, .. } => {
                self.ensure_child(transcript, *agent_id);
            }
            AgentEvent::ChildAgentCompleted { agent_id, outcome } => {
                self.complete_child(transcript, *agent_id, *outcome);
            }
            _ => return false,
        }
        true
    }

    fn fold_message(
        &mut self,
        transcript: &mut Vec<TranscriptBlock>,
        message_id: MessageId,
        agent_id: AgentId,
        delta: &str,
        is_reasoning: bool,
    ) {
        let index = self.ensure_message(transcript, message_id, agent_id);
        if let TranscriptBlock::AssistantMessage {
            text, reasoning, ..
        } = &mut transcript[index]
        {
            let target = if is_reasoning { reasoning } else { text };
            target.push_str(delta);
        }
    }

    fn fold_tool_requested(
        &mut self,
        transcript: &mut Vec<TranscriptBlock>,
        agent_id: AgentId,
        call: ToolCall,
    ) {
        let index = self.ensure_tool(transcript, call.id, agent_id, &call.name);
        if let TranscriptBlock::ToolCall {
            name,
            arguments,
            state,
            ..
        } = &mut transcript[index]
        {
            *name = call.name;
            *arguments = call.arguments;
            *state = ToolCallState::Requested;
        }
    }

    fn set_tool_state(
        &mut self,
        transcript: &mut Vec<TranscriptBlock>,
        call_id: ToolCallId,
        agent_id: AgentId,
        state: ToolCallState,
    ) {
        let index = self.ensure_tool(transcript, call_id, agent_id, "tool");
        if let TranscriptBlock::ToolCall { state: current, .. } = &mut transcript[index] {
            *current = state;
        }
    }

    fn fold_tool_completed(
        &mut self,
        transcript: &mut Vec<TranscriptBlock>,
        call_id: ToolCallId,
        agent_id: AgentId,
        result: ToolResultSummary,
    ) {
        let state = if result.has_error {
            ToolCallState::Failed {
                preview: result.output_preview,
            }
        } else {
            ToolCallState::Succeeded {
                preview: result.output_preview,
            }
        };
        self.set_tool_state(transcript, call_id, agent_id, state);
    }

    fn fold_permission_requested(
        &mut self,
        transcript: &mut Vec<TranscriptBlock>,
        pending_permissions: &mut std::collections::VecDeque<PermissionId>,
        request: PermissionRequest,
    ) {
        if self.permissions.contains_key(&request.id) {
            return;
        }
        let index = transcript.len();
        self.permissions.insert(request.id, index);
        pending_permissions.push_back(request.id);
        transcript.push(TranscriptBlock::Permission {
            id: request.id,
            tool_call_id: request.tool_call.id,
            tool_name: request.tool_call.name,
            decision: None,
        });
    }

    fn complete_child(
        &mut self,
        transcript: &mut Vec<TranscriptBlock>,
        agent_id: AgentId,
        outcome: AgentOutcome,
    ) {
        let index = self.ensure_child(transcript, agent_id);
        if let TranscriptBlock::ChildAgent {
            outcome: child_outcome,
            ..
        } = &mut transcript[index]
        {
            *child_outcome = Some(outcome);
        }
    }

    fn ensure_message(
        &mut self,
        transcript: &mut Vec<TranscriptBlock>,
        id: MessageId,
        agent_id: AgentId,
    ) -> usize {
        if let Some(index) = self.messages.get(&id) {
            return *index;
        }
        let index = transcript.len();
        transcript.push(TranscriptBlock::AssistantMessage {
            id,
            agent_id,
            text: String::new(),
            reasoning: String::new(),
            complete: false,
        });
        self.messages.insert(id, index);
        index
    }

    fn ensure_tool(
        &mut self,
        transcript: &mut Vec<TranscriptBlock>,
        id: ToolCallId,
        agent_id: AgentId,
        name: &str,
    ) -> usize {
        if let Some(index) = self.tools.get(&id) {
            return *index;
        }
        let index = transcript.len();
        transcript.push(TranscriptBlock::ToolCall {
            id,
            agent_id,
            name: name.to_owned(),
            arguments: serde_json::Value::Null,
            state: ToolCallState::Requested,
        });
        self.tools.insert(id, index);
        index
    }

    fn ensure_child(&mut self, transcript: &mut Vec<TranscriptBlock>, agent_id: AgentId) -> usize {
        if let Some(index) = self.children.get(&agent_id) {
            return *index;
        }
        let index = transcript.len();
        transcript.push(TranscriptBlock::ChildAgent {
            agent_id,
            outcome: None,
        });
        self.children.insert(agent_id, index);
        index
    }
}
