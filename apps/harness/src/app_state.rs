use std::collections::VecDeque;

use harness_protocol::{
    events::AgentEventEnvelope,
    ids::{AgentId, PermissionId, SessionId},
    messages::{AgentMessage, ContentBlock, MessageRole},
    usage::AgentUsageSnapshot,
};

use crate::{
    model::{LogEntry, TranscriptBlock},
    providers::{fallback_options, ProviderOption, SessionSelection},
};

mod activity_log;
mod events;
mod modal;
mod runtime_events;
mod transcript;

pub use self::modal::{ModalResult, ModalState};
use self::transcript::EventTranscript;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEntry {
    pub id: SessionId,
    pub title: String,
    pub provider: String,
    pub model: String,
    pub restorable: bool,
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
    events: EventTranscript,
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
        self.events.clear_indexes();
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
            Some(ModalState::Commands { selected }) => self.confirm_command(selected),
            Some(ModalState::Provider { selected }) => {
                self.modal = Some(ModalState::Account { provider: selected });
                ModalResult::None
            }
            Some(ModalState::Account { provider }) => self.confirm_account(provider),
            Some(ModalState::Model {
                provider, value, ..
            }) => self.confirm_model(provider, value),
            None => ModalResult::None,
        }
    }

    fn confirm_command(&mut self, selected: usize) -> ModalResult {
        match selected {
            0 => self.open_new_session(),
            1 => self.context_inspector_open = true,
            2 => self.log_open = true,
            _ => self.should_quit = true,
        }
        ModalResult::None
    }

    fn confirm_account(&mut self, provider: usize) -> ModalResult {
        match modal::confirm_account(provider, &self.providers) {
            Ok(next_modal) => self.modal = Some(next_modal),
            Err(message) => {
                self.error_banner = Some(message);
                self.modal = Some(ModalState::Account { provider });
            }
        }
        ModalResult::None
    }

    fn confirm_model(&mut self, provider: usize, value: String) -> ModalResult {
        match modal::confirm_model(provider, value, &self.providers) {
            Ok(selection) => ModalResult::StartSession(selection),
            Err((modal, message)) => {
                self.modal = Some(modal);
                self.error_banner = Some(message);
                ModalResult::None
            }
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
        self.events.resolve_permission(
            &mut self.transcript,
            &mut self.pending_permissions,
            id,
            approved,
        );
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
        let agent_id = envelope.agent_id;
        let event = envelope.event.clone();
        if !self.events.mark_seen(envelope.event_id) {
            return;
        }

        if let Some(text) = activity_log::log_summary(&event) {
            self.log.push(LogEntry {
                sequence: envelope.agent_sequence,
                text,
            });
            if self.log.len() > 500 {
                let overflow = self.log.len() - 500;
                self.log.drain(..overflow);
            }
        }

        let Some(event) = self.events.fold_conversation_event(
            &mut self.transcript,
            &mut self.pending_permissions,
            agent_id,
            event.clone(),
        ) else {
            self.apply_event(events::fold_event(event));
            if self.auto_follow {
                self.follow_bottom();
            }
            return;
        };

        self.apply_event(events::fold_event(event));

        if self.auto_follow {
            self.follow_bottom();
        }
    }

    fn apply_event(&mut self, action: events::EventAction) {
        match action {
            events::EventAction::StateChanged(status) => self.status = status,
            events::EventAction::Notice(text) => self.notice(text),
            events::EventAction::UsageUpdated(usage) => self.usage = Some(usage),
            events::EventAction::Failed { code, message } => {
                self.transcript
                    .push(TranscriptBlock::Error { code, message });
            }
            events::EventAction::Ignore => {}
        }
    }

    fn notice(&mut self, text: impl Into<String>) {
        self.transcript
            .push(TranscriptBlock::SystemNotice { text: text.into() });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_protocol::{
        events::{AgentEvent, EventVisibility},
        ids::{AgentId, EventId, MessageId, RunId, Timestamp},
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
