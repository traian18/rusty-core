use anyhow::{anyhow, Result};
use harness_engine::SessionHandle;
use harness_protocol::{
    commands::PermissionDecision, events::AgentEventEnvelope, ids::PermissionId,
};
use harness_session_store::SessionSummary;
use tokio::sync::broadcast;

use crate::{
    app_state::{AppState, SessionEntry},
    harness_setup::{AppHarness, SessionOptions},
    providers::{selection_for_backend, ProviderOption, SessionSelection},
};

struct LiveSession {
    handle: Option<SessionHandle>,
    events: Option<broadcast::Receiver<AgentEventEnvelope>>,
    state: AppState,
}

/// Owns engine handles and asynchronous session operations for the TUI.
pub struct AppController {
    harness: AppHarness,
    sessions: Vec<LiveSession>,
    active: usize,
}

impl AppController {
    pub async fn new(
        harness: AppHarness,
        options: SessionOptions,
        selection: SessionSelection,
    ) -> Result<Self> {
        let provider_options = harness
            .provider_options()
            .await
            .unwrap_or_else(|_| crate::providers::fallback_options());
        let catalog = harness.list_sessions().await.unwrap_or_default();

        // Honor the requested provider when it is ready. Otherwise fall back to
        // the first ready provider so a user who only has a CLI login (Claude
        // Code, Codex, Copilot) or only an API key is not blocked behind the
        // default provider they never chose.
        let requested_ready = provider_options
            .iter()
            .find(|provider| provider.id == selection.provider_id)
            .is_some_and(|provider| provider.ready);

        let live = if requested_ready {
            match harness.start(options).await {
                Ok(handle) => live_from_handle(handle, selection, provider_options, catalog),
                Err(error) => live_welcome(
                    selection,
                    provider_options,
                    catalog,
                    format!("Could not start provider session: {error}"),
                ),
            }
        } else if let Some(ready) = provider_options
            .iter()
            .find(|provider| provider.ready)
            .cloned()
        {
            let active = selection_from_option(&ready);
            match harness.start_selected(&active).await {
                Ok(handle) => live_from_handle(handle, active, provider_options, catalog),
                Err(error) => live_welcome(
                    active,
                    provider_options,
                    catalog,
                    format!("Could not start provider session: {error}"),
                ),
            }
        } else {
            live_welcome(
                selection,
                provider_options,
                catalog,
                "No provider is ready. Open the picker with Ctrl+N and use /login for connection instructions.",
            )
        };

        Ok(Self {
            harness,
            sessions: vec![live],
            active: 0,
        })
    }

    pub fn state(&self) -> &AppState {
        &self.sessions[self.active].state
    }

    pub fn state_mut(&mut self) -> &mut AppState {
        &mut self.sessions[self.active].state
    }

    /// Fold newly-arrived events and refresh derived session state.
    ///
    /// Returns whether anything visible changed so terminal frontends can
    /// avoid continuously repainting an otherwise idle screen.
    pub fn tick(&mut self) -> bool {
        let mut changed = false;
        for live in &mut self.sessions {
            if let Some(events) = &mut live.events {
                while let Ok(event) = events.try_recv() {
                    live.state.fold_event(event);
                    changed = true;
                }
            }
            if let Some(handle) = &live.handle {
                let status = format!("{:?}", handle.snapshot().status);
                if live.state.status != status {
                    live.state.status = status;
                    changed = true;
                }

                let context = handle.context_inspection();
                if live.state.context.as_ref() != Some(&context) {
                    live.state.context = Some(context);
                    changed = true;
                }
            }
        }
        changed
    }

    pub async fn previous_session(&mut self) -> Result<()> {
        self.select_relative(-1).await
    }

    pub async fn next_session(&mut self) -> Result<()> {
        self.select_relative(1).await
    }

    async fn select_relative(&mut self, offset: isize) -> Result<()> {
        let catalog = self.state().sessions.clone();
        let current_id = self.state().active_session;
        let current = catalog
            .iter()
            .position(|entry| Some(entry.id) == current_id)
            .unwrap_or(0);
        let target = current
            .saturating_add_signed(offset)
            .min(catalog.len().saturating_sub(1));
        let Some(entry) = catalog.get(target).cloned() else {
            return Ok(());
        };

        if let Some(index) = self.sessions.iter().position(|session| {
            session
                .handle
                .as_ref()
                .is_some_and(|handle| handle.session_id() == entry.id)
        }) {
            self.active = index;
            return Ok(());
        }
        if !entry.restorable {
            return Err(anyhow!(
                "session {} has durable history but no restore checkpoint",
                entry.id
            ));
        }

        let stored = self.harness.load_session(entry.id).await?;
        let snapshot = stored
            .snapshot
            .as_ref()
            .ok_or_else(|| anyhow!("session {} has no restore checkpoint", entry.id))?;
        let root = snapshot
            .agents
            .iter()
            .find(|agent| agent.agent_id == snapshot.root_agent_id)
            .ok_or_else(|| anyhow!("session {} has no root agent state", entry.id))?;
        let selection =
            selection_for_backend(Some(&root.backend.descriptor.name), &root.backend_config);
        let handle = self.harness.restore(entry.id).await?;
        let events = handle.subscribe();
        let mut state = AppState::from_snapshot(handle.snapshot().status, entry.id, selection);
        state.providers = self.state().providers.clone();
        state.sessions = catalog;
        state.hydrate_messages(root.agent_id, &root.messages);

        self.sessions.push(LiveSession {
            handle: Some(handle),
            events: Some(events),
            state,
        });
        self.active = self.sessions.len() - 1;
        self.sync_session_lists();
        Ok(())
    }

    pub async fn start_selected(&mut self, selection: SessionSelection) -> Result<()> {
        let handle = self.harness.start_selected(&selection).await?;
        let session_id = handle.session_id();
        let events = handle.subscribe();
        let mut state = AppState::from_snapshot(handle.snapshot().status, session_id, selection);
        state.providers = self.state().providers.clone();
        let mut entries = self.state().sessions.clone();
        entries.insert(
            0,
            SessionEntry {
                id: session_id,
                title: "New conversation".to_owned(),
                provider: state.provider.clone(),
                model: state.model.clone(),
                restorable: true,
            },
        );
        entries.dedup_by_key(|entry| entry.id);
        state.sessions = entries;

        self.sessions.push(LiveSession {
            handle: Some(handle),
            events: Some(events),
            state,
        });
        self.active = self.sessions.len() - 1;
        self.sync_session_lists();
        Ok(())
    }

    pub async fn refresh_active_models(&mut self) -> Result<()> {
        let provider_key = self
            .state()
            .providers
            .iter()
            .find(|provider| provider.name == self.state().provider)
            .map(|provider| provider.id.clone())
            .ok_or_else(|| anyhow!("active provider is not in the catalog"))?;
        let refreshed = self.harness.refresh_provider(&provider_key).await?;
        for session in &mut self.sessions {
            if let Some(existing) = session
                .state
                .providers
                .iter_mut()
                .find(|provider| provider.id == provider_key)
            {
                *existing = refreshed.clone();
            }
        }
        Ok(())
    }

    pub fn auth_instruction(&self) -> Result<String> {
        let provider = self
            .state()
            .providers
            .iter()
            .find(|provider| provider.name == self.state().provider)
            .ok_or_else(|| anyhow!("active provider is not in the catalog"))?;
        let flow = self.harness.auth_flow(&provider.id)?;
        Ok(match flow.current() {
            Some(harness_engine::AuthFlowState::WaitingForExternalCommand { program, args }) => {
                format!(
                    "Connect in a foreground terminal: {} {}",
                    program,
                    args.join(" ")
                )
            }
            Some(harness_engine::AuthFlowState::Connected { profile }) => {
                format!("Connected as {}", profile.label)
            }
            Some(harness_engine::AuthFlowState::Failed { safe_message }) => safe_message.clone(),
            Some(state) => format!("Authentication state: {state:?}"),
            None => "Authentication flow returned no state".into(),
        })
    }

    pub async fn send(&mut self, prompt: &str) -> Result<()> {
        let handle = self.sessions[self.active]
            .handle
            .as_ref()
            .ok_or_else(|| anyhow!("start a provider session before sending a prompt"))?;
        handle.send(prompt).await?;
        Ok(())
    }

    pub async fn cancel(&mut self) -> Result<()> {
        let handle = self.sessions[self.active]
            .handle
            .as_ref()
            .ok_or_else(|| anyhow!("there is no active provider session to cancel"))?;
        handle.cancel().await?;
        Ok(())
    }

    pub async fn resolve_permission(
        &mut self,
        id: PermissionId,
        decision: PermissionDecision,
    ) -> Result<()> {
        let handle = self.sessions[self.active]
            .handle
            .as_ref()
            .ok_or_else(|| anyhow!("there is no active provider session"))?;
        handle.resolve_permission(id, decision).await?;
        Ok(())
    }

    pub fn sync_session_lists(&mut self) {
        let entries = self.state().sessions.clone();
        for session in &mut self.sessions {
            session.state.sessions.clone_from(&entries);
        }
    }
}

fn live_from_handle(
    handle: SessionHandle,
    selection: SessionSelection,
    provider_options: Vec<ProviderOption>,
    catalog: Vec<SessionSummary>,
) -> LiveSession {
    let events = handle.subscribe();
    let mut state =
        AppState::from_snapshot(handle.snapshot().status, handle.session_id(), selection);
    state.set_provider_options(provider_options);
    state
        .sessions
        .extend(catalog.into_iter().map(entry_from_summary));
    LiveSession {
        handle: Some(handle),
        events: Some(events),
        state,
    }
}

fn live_welcome(
    selection: SessionSelection,
    provider_options: Vec<ProviderOption>,
    catalog: Vec<SessionSummary>,
    message: impl Into<String>,
) -> LiveSession {
    let mut state = AppState::welcome(selection);
    state.set_provider_options(provider_options);
    state
        .sessions
        .extend(catalog.into_iter().map(entry_from_summary));
    state.open_new_session();
    state.set_start_error(message.into());
    LiveSession {
        handle: None,
        events: None,
        state,
    }
}

fn selection_from_option(option: &ProviderOption) -> SessionSelection {
    SessionSelection {
        provider_id: option.id.clone(),
        credential_profile: option.credential_profile.clone(),
        integration: option.integration.clone(),
        provider: option.name.clone(),
        model: option.default_model.clone(),
    }
}

fn entry_from_summary(summary: SessionSummary) -> SessionEntry {
    let selection = selection_for_backend(summary.backend_name.as_deref(), &summary.backend_config);
    SessionEntry {
        id: summary.session_id,
        title: summary.title,
        provider: selection.provider,
        model: selection.model,
        restorable: summary.restorable,
    }
}
