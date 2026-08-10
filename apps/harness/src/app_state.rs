use std::collections::{HashMap, HashSet, VecDeque};

use harness_protocol::{
    events::{AgentEvent, AgentEventEnvelope},
    ids::{AgentId, EventId, MessageId, PermissionId, SessionId, ToolCallId},
    messages::{AgentMessage, ContentBlock, MessageRole},
    usage::AgentUsageSnapshot,
};

use crate::{
    model::{LogEntry, PermissionDisplayDecision, ToolCallState, TranscriptBlock},
    providers::{fallback_options, ProviderOption, SessionSelection},
};

/// Caps the activity log's memory growth over a long-running session.
/// Streaming text/reasoning deltas never reach the log (see `fold_event`),
/// so in practice this bound is only ever approached by very long sessions
/// with many state transitions, tool calls, or child agents — not by a
/// single verbose run.
const MAX_LOG_ENTRIES: usize = 500;

/// Formats a one-line activity-log summary for `event`, or `None` for the
/// two kinds excluded from the log. `AssistantTextDelta`/`ReasoningDelta`
/// are the only exclusions — a single streamed response can produce
/// hundreds of those, and their content is already visible, progressively,
/// in the curated transcript (see `AppState::fold_event`'s own handling of
/// them). Everything else is included, on purpose, even event kinds the
/// curated transcript never renders at all (`BackendRequestStarted`,
/// `PermissionRequested`, `UsageUpdated`) — that's the point of a raw log
/// next to a curated one: it can't silently go stale the way a hand-picked
/// `match` does when a new `AgentEvent` variant is added and nobody
/// remembers to teach the transcript about it.
fn log_summary(event: &AgentEvent) -> Option<String> {
    match event {
        AgentEvent::AssistantTextDelta { .. } | AgentEvent::ReasoningDelta { .. } => None,
        AgentEvent::StateChanged { from, to } => Some(format!("state {from:?} → {to:?}")),
        AgentEvent::RunStarted { run_id } => Some(format!("run started {run_id:?}")),
        AgentEvent::BackendRequestStarted { request_id } => {
            Some(format!("backend request started {request_id:?}"))
        }
        AgentEvent::AssistantMessageStarted { message_id } => {
            Some(format!("assistant message started {message_id:?}"))
        }
        AgentEvent::AssistantMessageCompleted { message_id } => {
            Some(format!("assistant message completed {message_id:?}"))
        }
        AgentEvent::ToolCallRequested { call } => Some(format!(
            "tool call requested: {} {}",
            call.name, call.arguments
        )),
        AgentEvent::ToolCallStarted { call_id } => Some(format!("tool call started {call_id:?}")),
        AgentEvent::ToolCallProgress { call_id, progress } => Some(format!(
            "tool call progress {call_id:?}: {} ({:.0}%)",
            progress.status,
            progress.fraction * 100.0
        )),
        AgentEvent::ToolCallCompleted { call_id, result } => Some(format!(
            "tool call {} {call_id:?}: {}",
            if result.has_error {
                "failed"
            } else {
                "completed"
            },
            result.output_preview
        )),
        AgentEvent::PermissionRequested { request } => Some(format!(
            "permission requested for {} ({:?})",
            request.tool_call.name, request.id
        )),
        AgentEvent::UsageUpdated { usage } => Some(format!("usage updated: {usage:?}")),
        AgentEvent::ChildAgentSpawned { agent_id } => {
            Some(format!("child agent spawned {agent_id:?}"))
        }
        AgentEvent::ChildAgentCompleted { agent_id, outcome } => {
            Some(format!("child agent {agent_id:?} completed: {outcome:?}"))
        }
        AgentEvent::Failed { error } => Some(format!("FAILED [{}] {}", error.code, error.message)),
        AgentEvent::Completed { outcome } => Some(format!("completed: {outcome:?}")),
    }
}

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
    Commands {
        selected: usize,
    },
    Provider {
        selected: usize,
    },
    Account {
        provider: usize,
    },
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
    pub log: Vec<LogEntry>,
    pub log_open: bool,
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
                MessageRole::User => self.transcript.push(TranscriptBlock::UserMessage { text }),
                MessageRole::Assistant => self.transcript.push(TranscriptBlock::AssistantMessage {
                    id: message.id,
                    agent_id,
                    text,
                    reasoning: String::new(),
                    complete: true,
                }),
                MessageRole::System | MessageRole::Tool => {}
            }
        }
    }

    pub fn open_commands(&mut self) {
        self.modal = Some(ModalState::Commands { selected: 0 });
        self.error_banner = None;
    }

    pub fn open_new_session(&mut self) {
        if self.providers.is_empty() {
            self.providers = fallback_options();
        }
        let selected = self
            .providers
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
            Some(ModalState::Commands { selected }) => *selected = (*selected + 1).min(3),
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
            Some(ModalState::Commands { selected: 2 }) => {
                self.log_open = true;
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

    pub fn toggle_log(&mut self) {
        self.log_open = !self.log_open;
    }

    pub fn set_provider_options(&mut self, options: Vec<ProviderOption>) {
        if !options.is_empty() {
            self.providers = options;
        }
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

        if let Some(text) = log_summary(&envelope.event) {
            self.log.push(LogEntry {
                sequence: envelope.agent_sequence,
                text,
            });
            if self.log.len() > MAX_LOG_ENTRIES {
                let overflow = self.log.len() - MAX_LOG_ENTRIES;
                self.log.drain(..overflow);
            }
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
                if let TranscriptBlock::AssistantMessage { text, .. } = &mut self.transcript[index]
                {
                    text.push_str(&delta);
                }
            }
            AgentEvent::ReasoningDelta { message_id, delta } => {
                let index = self.ensure_message(message_id, agent_id);
                if let TranscriptBlock::AssistantMessage { reasoning, .. } =
                    &mut self.transcript[index]
                {
                    reasoning.push_str(&delta);
                }
            }
            AgentEvent::AssistantMessageCompleted { message_id } => {
                let index = self.ensure_message(message_id, agent_id);
                if let TranscriptBlock::AssistantMessage { complete, .. } =
                    &mut self.transcript[index]
                {
                    *complete = true;
                }
            }
            AgentEvent::ToolCallRequested { call } => {
                let index = self.ensure_tool(call.id, agent_id, &call.name);
                if let TranscriptBlock::ToolCall {
                    name,
                    arguments,
                    state,
                    ..
                } = &mut self.transcript[index]
                {
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
                    *state = ToolCallState::Progress {
                        status: progress.status,
                        fraction: progress.fraction,
                    };
                }
            }
            AgentEvent::ToolCallCompleted { call_id, result } => {
                let index = self.ensure_tool(call_id, agent_id, "tool");
                if let TranscriptBlock::ToolCall { state, .. } = &mut self.transcript[index] {
                    *state = if result.has_error {
                        ToolCallState::Failed {
                            preview: result.output_preview,
                        }
                    } else {
                        ToolCallState::Succeeded {
                            preview: result.output_preview,
                        }
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
                if let TranscriptBlock::ChildAgent {
                    outcome: child_outcome,
                    ..
                } = &mut self.transcript[index]
                {
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
        self.transcript
            .push(TranscriptBlock::SystemNotice { text: text.into() });
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
        self.transcript.push(TranscriptBlock::ChildAgent {
            agent_id,
            outcome: None,
        });
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

    #[test]
    fn activity_log_records_events_the_curated_transcript_never_renders() {
        let mut state = AppState::default();
        let agent_id = AgentId::new();

        // BackendRequestStarted has no transcript representation at all
        // (see `fold_event`'s empty match arm for it) — the whole point of
        // the activity log is that it's visible there anyway.
        state.fold_event(envelope(
            EventId::new(),
            agent_id,
            AgentEvent::BackendRequestStarted {
                request_id: harness_protocol::ids::RequestId::new(),
            },
        ));

        assert!(state.transcript.is_empty());
        assert_eq!(state.log.len(), 1);
        assert!(state.log[0].text.contains("backend request started"));
    }

    #[test]
    fn activity_log_excludes_streaming_deltas() {
        let mut state = AppState::default();
        let agent_id = AgentId::new();
        let message_id = MessageId::new();

        state.fold_event(envelope(
            EventId::new(),
            agent_id,
            AgentEvent::AssistantTextDelta {
                message_id,
                delta: "hi".to_owned(),
            },
        ));
        state.fold_event(envelope(
            EventId::new(),
            agent_id,
            AgentEvent::ReasoningDelta {
                message_id,
                delta: "thinking".to_owned(),
            },
        ));

        // Both events land in the transcript (folded into the same
        // AssistantMessage block) but neither should reach the log.
        assert_eq!(state.transcript.len(), 1);
        assert!(state.log.is_empty());
    }

    #[test]
    fn activity_log_survives_a_failed_run_with_a_readable_summary() {
        let mut state = AppState::default();
        let agent_id = AgentId::new();

        state.fold_event(envelope(
            EventId::new(),
            agent_id,
            AgentEvent::Failed {
                error: harness_protocol::commands::AgentError {
                    code: "spawn_failed".to_owned(),
                    message: "failed to spawn Claude Code CLI".to_owned(),
                    details: None,
                },
            },
        ));

        assert_eq!(state.log.len(), 1);
        assert!(state.log[0].text.starts_with("FAILED [spawn_failed]"));
        assert!(state.log[0]
            .text
            .contains("failed to spawn Claude Code CLI"));
    }

    #[test]
    fn toggle_log_flips_visibility() {
        let mut state = AppState::default();
        assert!(!state.log_open);
        state.toggle_log();
        assert!(state.log_open);
        state.toggle_log();
        assert!(!state.log_open);
    }
}
