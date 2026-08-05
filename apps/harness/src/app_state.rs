use std::collections::{HashMap, HashSet, VecDeque};

use harness_protocol::{
    events::{AgentEvent, AgentEventEnvelope},
    ids::{AgentId, EventId, MessageId, PermissionId, SessionId, ToolCallId},
    messages::{AgentMessage, ContentBlock, MessageRole},
    usage::AgentUsageSnapshot,
};

use crate::{
    model::{PermissionDisplayDecision, ToolCallState, TranscriptBlock},
    providers::{fallback_options, ProviderOption, SessionSelection},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEntry {
    pub id: SessionId,
    pub title: String,
    pub provider: String,
    pub model: String,
    pub restorable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModalState {
    Commands { selected: usize },
    Provider { selected: usize },
    Account { provider: usize },
    Model {
        provider: usize,
        selected: usize,
        value: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModalResult {
    None,
    StartSession(SessionSelection),
}

#[derive(Default)]
pub struct AppState {
    pub input: String,
    pub status: String,
    pub transcript: Vec<TranscriptBlock>,
    pub pending_permissions: VecDeque<PermissionId>,
    pub usage: Option<AgentUsageSnapshot>,
    pub should_quit: bool,
    pub scroll: u16,
    pub auto_follow: bool,
    pub modal: Option<ModalState>,
    pub sessions: Vec<SessionEntry>,
    pub active_session: Option<SessionId>,
    pub provider: String,
    pub model: String,
    pub error_banner: Option<String>,
    pub providers: Vec<ProviderOption>,
    pub context_inspector_open: bool,
    pub context: Option<harness_engine::ContextInspection>,
    seen_events: HashSet<EventId>,
    messages: HashMap<MessageId, usize>,
    tools: HashMap<ToolCallId, usize>,
    permissions: HashMap<PermissionId, usize>,
    children: HashMap<AgentId, usize>,
}

impl AppState {
    pub fn welcome(selection: SessionSelection) -> Self {
        Self {
            status: "Choose a provider".to_owned(),
            auto_follow: true,
            provider: selection.provider,
            model: selection.model,
            providers: fallback_options(),
            ..Self::default()
        }
    }

    pub fn from_snapshot(
        status: impl std::fmt::Debug,
        session_id: SessionId,
        selection: SessionSelection,
    ) -> Self {
        let entry = SessionEntry {
            id: session_id,
            title: "New conversation".to_owned(),
            provider: selection.provider.clone(),
            model: selection.model.clone(),
            restorable: true,
        };
        Self {
            status: format!("{status:?}"),
            auto_follow: true,
            sessions: vec![entry],
            active_session: Some(session_id),
            provider: selection.provider,
            model: selection.model,
            providers: fallback_options(),
            ..Self::default()
        }
    }

    pub fn hydrate_messages(&mut self, agent_id: AgentId, messages: &[AgentMessage]) {
        self.transcript.clear();
        for message in messages {
            let text = message
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            if text.is_empty() {
                continue;
            }
            match message.role {
                MessageRole::User => self
                    .transcript
                    .push(TranscriptBlock::UserMessage { text }),
                MessageRole::Assistant => self.transcript.push(
                    TranscriptBlock::AssistantMessage {
                        id: message.id,
                        agent_id,
                        text,
                        reasoning: String::new(),
                        complete: true,
                    },
                ),
                MessageRole::System | MessageRole::Tool => {}
            }
        }
    }

    pub fn open_commands(&mut self) {
        self.modal = Some(ModalState::Commands { selected: 0 });
        self.error_banner = None;
    }

    pub fn open_new_session(&mut self) {
        if self.providers.is_empty() { self.providers = fallback_options(); }
        let selected = self.providers
            .iter()
            .position(|provider| provider.name == self.provider)
            .unwrap_or(0);
        self.modal = Some(ModalState::Provider { selected });
        self.error_banner = None;
    }

    pub fn modal_up(&mut self) {
        match self.modal.as_mut() {
            Some(ModalState::Commands { selected }) | Some(ModalState::Provider { selected }) => {
                *selected = selected.saturating_sub(1);
            }
            Some(ModalState::Model {
                provider,
                selected,
                value,
            }) => {
                *selected = selected.saturating_sub(1);
                if let Some(model) = self
                    .providers
                    .get(*provider)
                    .and_then(|option| option.models.get(*selected))
                {
                    value.clone_from(model);
                }
            }
            _ => {}
        }
    }

    pub fn modal_down(&mut self) {
        match self.modal.as_mut() {
            Some(ModalState::Commands { selected }) => *selected = (*selected + 1).min(2),
            Some(ModalState::Provider { selected }) => {
                *selected = (*selected + 1).min(self.providers.len().saturating_sub(1));
            }
            Some(ModalState::Model {
                provider,
                selected,
                value,
            }) => {
                if let Some(option) = self.providers.get(*provider) {
                    *selected = (*selected + 1).min(option.models.len().saturating_sub(1));
                    if let Some(model) = option.models.get(*selected) {
                        value.clone_from(model);
                    }
                }
            }
            _ => {}
        }
    }

    pub fn modal_insert(&mut self, character: char) {
        if let Some(ModalState::Model {
            selected, value, ..
        }) = self.modal.as_mut()
        {
            if *selected != usize::MAX {
                value.clear();
            }
            *selected = usize::MAX;
            value.push(character);
        }
    }

    pub fn modal_backspace(&mut self) {
        if let Some(ModalState::Model {
            selected, value, ..
        }) = self.modal.as_mut()
        {
            *selected = usize::MAX;
            value.pop();
        }
    }

    pub fn cancel_modal(&mut self) {
        self.modal = None;
    }

    pub fn confirm_modal(&mut self) -> ModalResult {
        match self.modal.take() {
            Some(ModalState::Commands { selected: 0 }) => {
                self.open_new_session();
                ModalResult::None
            }
            Some(ModalState::Commands { selected: 1 }) => {
                self.context_inspector_open = true;
                ModalResult::None
            }
            Some(ModalState::Commands { .. }) => {
                self.should_quit = true;
                ModalResult::None
            }
            Some(ModalState::Provider { selected }) => {
                self.modal = Some(ModalState::Account { provider: selected });
                ModalResult::None
            }
            Some(ModalState::Account { provider }) => {
                let option = &self.providers[provider.min(self.providers.len() - 1)];
                if !option.ready {
                    self.error_banner = Some(option.health_message.clone());
                    self.modal = Some(ModalState::Account { provider });
                    return ModalResult::None;
                }
                let selected = option
                    .models
                    .iter()
                    .position(|model| model == &option.default_model)
                    .unwrap_or(0);
                self.modal = Some(ModalState::Model {
                    provider,
                    selected,
                    value: option.default_model.clone(),
                });
                ModalResult::None
            }
            Some(ModalState::Model {
                provider,
                selected: _,
                value,
            }) => {
                let provider_index = provider.min(self.providers.len() - 1);
                let provider = &self.providers[provider_index];
                let model = value.trim();
                if model.is_empty() {
                    self.modal = Some(ModalState::Model {
                        provider: provider_index,
                        selected: usize::MAX,
                        value,
                    });
                    self.error_banner = Some("Model ID cannot be empty".to_owned());
                    return ModalResult::None;
                }
                ModalResult::StartSession(SessionSelection {
                    provider_id: provider.id.clone(),
                    credential_profile: provider.credential_profile.clone(),
                    integration: provider.integration.clone(),
                    provider: provider.name.clone(),
                    model: model.to_owned(),
                })
            }
            None => ModalResult::None,
        }
    }

    pub fn system_notice(&mut self, text: impl Into<String>) {
        self.notice(text);
    }

    pub fn toggle_context_inspector(&mut self) {
        self.context_inspector_open = !self.context_inspector_open;
    }

    pub fn set_provider_options(&mut self, options: Vec<ProviderOption>) {
        if !options.is_empty() { self.providers = options; }
    }

    pub fn set_start_error(&mut self, message: impl Into<String>) {
        self.error_banner = Some(message.into());
    }

    pub fn submit_user_message(&mut self, text: String) {
        if let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| Some(session.id) == self.active_session)
        {
            if session.title == "New conversation" {
                session.title = text
                    .lines()
                    .next()
                    .unwrap_or("Conversation")
                    .chars()
                    .take(36)
                    .collect();
            }
        }
        self.transcript.push(TranscriptBlock::UserMessage { text });
        self.follow_bottom();
    }

    pub fn active_permission(&self) -> Option<PermissionId> {
        self.pending_permissions.front().copied()
    }

    pub fn resolve_permission(&mut self, id: PermissionId, approved: bool) {
        self.pending_permissions.retain(|pending| *pending != id);
        if let Some(index) = self.permissions.get(&id).copied() {
            if let TranscriptBlock::Permission { decision, .. } = &mut self.transcript[index] {
                *decision = Some(if approved {
                    PermissionDisplayDecision::Approved
                } else {
                    PermissionDisplayDecision::Denied
                });
            }
        }
    }

    pub fn scroll_up(&mut self, amount: u16) {
        self.auto_follow = false;
        self.scroll = self.scroll.saturating_add(amount);
    }

    pub fn scroll_down(&mut self, amount: u16) {
        self.scroll = self.scroll.saturating_sub(amount);
        if self.scroll == 0 {
            self.auto_follow = true;
        }
    }

    pub fn follow_bottom(&mut self) {
        self.auto_follow = true;
        self.scroll = 0;
    }

    pub fn fold_event(&mut self, envelope: AgentEventEnvelope) {
        if !self.seen_events.insert(envelope.event_id) {
            return;
        }

        let agent_id = envelope.agent_id;
        match envelope.event {
            AgentEvent::StateChanged { to, .. } => self.status = format!("{to:?}"),
            AgentEvent::RunStarted { .. } => self.notice("Run started"),
            AgentEvent::BackendRequestStarted { .. } => {}
            AgentEvent::AssistantMessageStarted { message_id } => {
                self.ensure_message(message_id, agent_id);
            }
            AgentEvent::AssistantTextDelta { message_id, delta } => {
                let index = self.ensure_message(message_id, agent_id);
                if let TranscriptBlock::AssistantMessage { text, .. } = &mut self.transcript[index] {
                    text.push_str(&delta);
                }
            }
            AgentEvent::ReasoningDelta { message_id, delta } => {
                let index = self.ensure_message(message_id, agent_id);
                if let TranscriptBlock::AssistantMessage { reasoning, .. } = &mut self.transcript[index] {
                    reasoning.push_str(&delta);
                }
            }
            AgentEvent::AssistantMessageCompleted { message_id } => {
                let index = self.ensure_message(message_id, agent_id);
                if let TranscriptBlock::AssistantMessage { complete, .. } = &mut self.transcript[index] {
                    *complete = true;
                }
            }
            AgentEvent::ToolCallRequested { call } => {
                let index = self.ensure_tool(call.id, agent_id, &call.name);
                if let TranscriptBlock::ToolCall { name, arguments, state, .. } = &mut self.transcript[index] {
                    *name = call.name;
                    *arguments = call.arguments;
                    *state = ToolCallState::Requested;
                }
            }
            AgentEvent::ToolCallStarted { call_id } => {
                let index = self.ensure_tool(call_id, agent_id, "tool");
                if let TranscriptBlock::ToolCall { state, .. } = &mut self.transcript[index] {
                    *state = ToolCallState::Running;
                }
            }
            AgentEvent::ToolCallProgress { call_id, progress } => {
                let index = self.ensure_tool(call_id, agent_id, "tool");
                if let TranscriptBlock::ToolCall { state, .. } = &mut self.transcript[index] {
                    *state = ToolCallState::Progress { status: progress.status, fraction: progress.fraction };
                }
            }
            AgentEvent::ToolCallCompleted { call_id, result } => {
                let index = self.ensure_tool(call_id, agent_id, "tool");
                if let TranscriptBlock::ToolCall { state, .. } = &mut self.transcript[index] {
                    *state = if result.has_error {
                        ToolCallState::Failed { preview: result.output_preview }
                    } else {
                        ToolCallState::Succeeded { preview: result.output_preview }
                    };
                }
            }
            AgentEvent::PermissionRequested { request } => {
                if !self.permissions.contains_key(&request.id) {
                    let index = self.transcript.len();
                    self.permissions.insert(request.id, index);
                    self.pending_permissions.push_back(request.id);
                    self.transcript.push(TranscriptBlock::Permission {
                        id: request.id,
                        tool_call_id: request.tool_call.id,
                        tool_name: request.tool_call.name,
                        decision: None,
                    });
                }
            }
            AgentEvent::UsageUpdated { usage } => self.usage = Some(usage),
            AgentEvent::ChildAgentSpawned { agent_id } => {
                self.ensure_child(agent_id);
            }
            AgentEvent::ChildAgentCompleted { agent_id, outcome } => {
                let index = self.ensure_child(agent_id);
                if let TranscriptBlock::ChildAgent { outcome: child_outcome, .. } = &mut self.transcript[index] {
                    *child_outcome = Some(outcome);
                }
            }
            AgentEvent::Failed { error } => self.transcript.push(TranscriptBlock::Error {
                code: error.code,
                message: error.message,
            }),
            AgentEvent::Completed { outcome } => self.notice(format!("Run completed: {outcome:?}")),
        }

        if self.auto_follow {
            self.follow_bottom();
        }
    }

    fn notice(&mut self, text: impl Into<String>) {
        self.transcript.push(TranscriptBlock::SystemNotice { text: text.into() });
    }

    fn ensure_message(&mut self, id: MessageId, agent_id: AgentId) -> usize {
        if let Some(index) = self.messages.get(&id) {
            return *index;
        }
        let index = self.transcript.len();
        self.transcript.push(TranscriptBlock::AssistantMessage {
            id,
            agent_id,
            text: String::new(),
            reasoning: String::new(),
            complete: false,
        });
        self.messages.insert(id, index);
        index
    }

    fn ensure_tool(&mut self, id: ToolCallId, agent_id: AgentId, name: &str) -> usize {
        if let Some(index) = self.tools.get(&id) {
            return *index;
        }
        let index = self.transcript.len();
        self.transcript.push(TranscriptBlock::ToolCall {
            id,
            agent_id,
            name: name.to_owned(),
            arguments: serde_json::Value::Null,
            state: ToolCallState::Requested,
        });
        self.tools.insert(id, index);
        index
    }

    fn ensure_child(&mut self, agent_id: AgentId) -> usize {
        if let Some(index) = self.children.get(&agent_id) {
            return *index;
        }
        let index = self.transcript.len();
        self.transcript.push(TranscriptBlock::ChildAgent { agent_id, outcome: None });
        self.children.insert(agent_id, index);
        index
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_protocol::{
        events::{AgentEvent, EventVisibility},
        ids::{AgentId, RunId, Timestamp},
    };

    fn envelope(event_id: EventId, agent_id: AgentId, event: AgentEvent) -> AgentEventEnvelope {
        AgentEventEnvelope {
            event_id,
            session_id: SessionId::new(),
            agent_id,
            parent_agent_id: None,
            run_id: Some(RunId::new()),
            agent_sequence: 1,
            session_sequence: Some(1),
            timestamp: Timestamp::now(),
            visibility: EventVisibility::User,
            event,
        }
    }

    #[test]
    fn folds_delta_without_started_event_and_deduplicates() {
        let mut state = AppState::default();
        let agent_id = AgentId::new();
        let message_id = MessageId::new();
        let event_id = EventId::new();

        let event = || {
            envelope(
                event_id,
                agent_id,
                AgentEvent::AssistantTextDelta {
                    message_id,
                    delta: "hello".to_owned(),
                },
            )
        };

        state.fold_event(event());
        state.fold_event(event());

        assert_eq!(state.transcript.len(), 1);
        assert!(matches!(
            &state.transcript[0],
            TranscriptBlock::AssistantMessage {
                id,
                agent_id: actual_agent_id,
                text,
                reasoning,
                complete: false,
            } if *id == message_id
                && *actual_agent_id == agent_id
                && text == "hello"
                && reasoning.is_empty()
        ));
    }
}
